//! HTTP client for the semctx server. Thin wrapper over `reqwest` that
//! handles bearer-auth + the X-Tenant-Id header + JSON request/response.
//!
//! Typed request/response bodies live in [`api`] — hand-written to match
//! the server's controllers for now. When the `OpenAPI` spec stabilises
//! we'll swap [`api`] for a `progenitor`-generated client driven off a
//! vendored `openapi/v1.json` snapshot.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore},
    time::Instant,
};
use tracing::warn;

use crate::auth;
use crate::session::{CredentialScope, CredentialSource, SessionContext};

mod failure;
mod retry;
#[cfg(test)]
pub(crate) mod stub;
pub(crate) mod transport;

use failure::gateway_error;
use retry::Idempotency;

pub(crate) use failure::ApiFailure;
pub(crate) use retry::DEADLINE_MARGIN;
pub(crate) use transport::HttpTransport;

const TENANT_HEADER: &str = "X-Tenant-Id";
/// The checkout a request is made from. The server prefers that copy of a
/// codebase for any read that is about one, so an agent working in a checkout
/// is answered about the tree it is looking at.
const CHECKOUT_HEADER: &str = "X-Semctx-Source-Id";
const LOADING_RETRY_MIN_DELAY: Duration = Duration::from_secs(1);
const LOADING_RETRY_MAX_DELAY: Duration = Duration::from_secs(5);

/// Cheap to clone — `reqwest::Client` is internally `Arc`'d and the
/// other fields are small strings. The MCP server handler holds a
/// `Client` and rmcp requires it to be `Clone`.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    /// How this client's session authorizes its requests. Carried explicitly so
    /// two clients in one process can hold different credentials.
    credentials: CredentialSource,
    base_url: String,
    /// Shared so an MCP server and every codebase-bound clone can recover from
    /// a persisted tenant that identity no longer lists for this principal.
    tenant: Arc<RwLock<Option<String>>>,
    /// Only persisted config is eligible for automatic replacement. An
    /// explicit `--tenant` / `SEMCTX_TENANT` remains authoritative.
    repair_configured_tenant: bool,
    /// Serialize recovery so concurrent MCP requests do not all query identity
    /// and rewrite config after the same rejection.
    tenant_repair: Arc<Mutex<()>>,
    codebase: Option<String>,
    /// Local checkout root of `codebase`, when known (recorded by
    /// `semctl index`). Lets path-rendering absolutize the server's
    /// codebase-relative hit paths into Read-ready absolute paths. `None`
    /// for canonical / server-pulled codebases that have no local bytes.
    local_root: Option<PathBuf>,
    /// Opaque identity of the checkout this process is running in, when it is
    /// running in one. Sent with every request so a read about a codebase
    /// resolves to THIS working copy rather than to what the server pulled —
    /// the code in front of you, not the code on the trunk.
    checkout_source_id: Option<String>,
    /// What the server said it can do, read once per process and shared by
    /// every clone — a capability does not change under a running command,
    /// and asking again per call would put a round-trip in front of work
    /// that has nothing to do with it.
    capabilities: Arc<tokio::sync::OnceCell<Vec<String>>>,
    /// The engine's bound on concurrent request attempts, when this client was
    /// built by one. `None` for a one-shot command, which has nothing to bound.
    /// The handle comes from the caller: this module never reads the
    /// environment, so a session cannot raise a process-wide bound.
    remote_permits: Option<Arc<Semaphore>>,
    /// The moment by which every request of this client must have an answer,
    /// when the caller bounds its call. `None` for a CLI command and for the
    /// daemon's sync, which have no caller waiting on a clock.
    ///
    /// Only a per-call clone carries a deadline. A client that a long-lived
    /// owner keeps, such as a coordinator, never does: see
    /// [`Self::with_deadline`].
    deadline: Option<Instant>,
}

impl Client {
    /// Build isolated client state for tests that do not send HTTP requests.
    #[cfg(test)]
    pub(crate) fn for_test(codebase: &str, local_root: Option<PathBuf>) -> Self {
        let mut client = Self::new(
            &HttpTransport::new().expect("build the test transport"),
            CredentialSource::Stored,
            "http://127.0.0.1:1",
            None,
            Some(codebase.into()),
            false,
            None,
        );
        client.local_root = local_root;
        client
    }

    /// A client with no codebase for a stub server at `base_url`. A fixed
    /// token authorizes it, so a test never reads the credential store.
    #[cfg(test)]
    pub(crate) fn for_test_server(base_url: &str) -> Self {
        Self::new(
            &HttpTransport::new().expect("build the test transport"),
            CredentialSource::from_test_token("stub-token"),
            base_url,
            None,
            None,
            false,
            None,
        )
    }

    fn new(
        transport: &HttpTransport,
        credentials: CredentialSource,
        base_url: &str,
        tenant: Option<String>,
        codebase: Option<String>,
        repair_configured_tenant: bool,
        remote_permits: Option<Arc<Semaphore>>,
    ) -> Self {
        Self {
            http: transport.http().clone(),
            credentials,
            base_url: base_url.trim_end_matches('/').to_string(),
            tenant: Arc::new(RwLock::new(tenant)),
            repair_configured_tenant,
            tenant_repair: Arc::new(Mutex::new(())),
            codebase,
            local_root: None,
            checkout_source_id: None,
            capabilities: Arc::new(tokio::sync::OnceCell::new()),
            remote_permits,
            deadline: None,
        }
    }

    /// The resolved codebase id, or an error naming how to set it. Code /
    /// graph endpoints are codebase-scoped (`/v1/codebases/{id}/…`); the id is
    /// either configured explicitly (`SEMCTX_CODEBASE` / `--codebase`) or
    /// resolved from the working directory at MCP startup.
    pub fn codebase(&self) -> Result<&str> {
        self.codebase_raw().ok_or_else(|| {
            anyhow!(
                "no codebase for this directory — register it on the server, or set \
                 SEMCTX_CODEBASE / --codebase"
            )
        })
    }

    /// The codebase id if one is set, without erroring — for the resolve step
    /// (decide whether to look one up) and to scope `search` opportunistically.
    pub fn codebase_raw(&self) -> Option<&str> {
        self.codebase.as_deref().filter(|c| !c.is_empty())
    }

    /// Return a copy with `codebase` set — used after resolving it from the
    /// working directory.
    pub fn with_codebase(mut self, codebase: String) -> Self {
        self.codebase = Some(codebase);
        self
    }

    /// The same client with no codebase selected.
    ///
    /// A first index must not write into a codebase that was pinned for the
    /// session: the checkout being indexed gets the codebase its own
    /// registration names.
    #[must_use]
    pub(crate) fn without_codebase(mut self) -> Self {
        self.codebase = None;
        self
    }

    /// The same client, with every request bounded by `deadline`.
    ///
    /// This is for one call. Each attempt, each retry wait, and each retry is
    /// kept inside the deadline, and a request that cannot finish in time
    /// fails with an error that names the deadline. Never give a client with
    /// a deadline to an owner that outlives the call: its later requests
    /// would fail at once.
    #[must_use]
    pub(crate) fn with_deadline(mut self, deadline: Option<Instant>) -> Self {
        self.deadline = deadline;
        self
    }

    /// The deadline of this client, for tests that check which clients carry one.
    #[cfg(test)]
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Attach a checkout only when its source identity can be derived. A failed
    /// identity leaves both the request selector and local rendering unbound.
    pub fn with_local_root(mut self, root: Option<PathBuf>) -> Self {
        let root = root.filter(|dir| dir.is_dir());
        // Derived here, once, rather than per request: it hashes the
        // installation id with the path, and every read would otherwise pay
        // for a file read it does not need.
        self.checkout_source_id = root
            .as_deref()
            .and_then(|dir| crate::codebase::checkout_source_id(dir).ok());
        self.local_root = root.filter(|_| self.checkout_source_id.is_some());
        self
    }

    /// Attach only a checkout recorded for this codebase. A missing cache leaves
    /// the client unbound, so edit operations cannot select an unrelated directory.
    pub fn with_cached_local_root(self, prefer: Option<&Path>) -> Self {
        let root = self.codebase_raw().and_then(|id| {
            crate::config::load()
                .ok()
                .and_then(|config| config.codebase_root(id, prefer))
        });
        self.with_local_root(root)
    }

    /// The same client, asking about the project rather than about the
    /// checkout it is standing in.
    ///
    /// Dropping the header IS the request: the server prefers the copy a
    /// caller names, then theirs, then canonical — so saying nothing about
    /// where you are standing asks for what the project publishes.
    #[must_use]
    pub fn for_canonical(&self) -> Self {
        let mut client = self.clone();
        client.checkout_source_id = None;
        client.local_root = None;
        client
    }

    /// The codebase's local checkout root, if known. See [`Self::local_root`]
    /// field docs for when this is `None`.
    pub fn local_root(&self) -> Option<&Path> {
        self.local_root.as_deref()
    }

    /// Effective resource-server base URL. Contains no credentials.
    pub fn server_url(&self) -> &str {
        &self.base_url
    }

    /// The same client, authorized differently. Tests use it to build the two
    /// credential scopes one root can be attached under.
    #[cfg(test)]
    pub(crate) fn with_credentials(mut self, credentials: CredentialSource) -> Self {
        self.credentials = credentials;
        self
    }

    /// The comparable identity of this client's credentials. Shared state keyed
    /// by it can never be reused across authorizations.
    pub(crate) fn credential_scope(&self) -> CredentialScope {
        self.credentials.scope()
    }

    /// Effective active tenant selector. Contains only the configured slug/id,
    /// never an access token.
    pub async fn tenant(&self) -> Option<String> {
        self.tenant.read().await.clone()
    }

    /// Build an authenticated request for `method path`: attaches the bearer
    /// token and the `X-Tenant-Id` header, returning the builder alongside the
    /// resolved URL (for error context). The verb-specific body shaping
    /// (`.json(body)`) and response unwrapping stay with each caller.
    async fn authed(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<(reqwest::RequestBuilder, String, Option<String>)> {
        let token =
            auth::get_valid_access_token(&self.http, &self.base_url, &self.credentials).await?;
        let url = self.url(path);
        let mut req = self.http.request(method, &url).bearer_auth(&token);
        let tenant = self.tenant.read().await.clone();
        if let Some(t) = &tenant {
            req = req.header(TENANT_HEADER, t);
        }
        if let Some(source) = &self.checkout_source_id {
            req = req.header(CHECKOUT_HEADER, source);
        }
        Ok((req, url, tenant))
    }

    /// One in-flight request attempt, when this client is bound to an engine.
    ///
    /// The permit covers sending the request and receiving its response head.
    /// It is released before the caller reads the body, so a slow reader does
    /// not hold a permit, and it is released across a loading retry's sleep, so
    /// a restoring server does not pin the process's permits.
    async fn remote_permit(&self) -> Option<OwnedSemaphorePermit> {
        match &self.remote_permits {
            Some(permits) => crate::engine::scheduler::permit(permits).await,
            None => None,
        }
    }

    /// Replace a rejected persisted tenant when identity has exactly one
    /// membership. Best-effort: any discovery/config error leaves the original
    /// denial as the user-facing result.
    async fn repair_tenant_after_denial(&self, rejected: Option<&str>) -> bool {
        if !self.repair_configured_tenant {
            return false;
        }
        let Some(rejected) = rejected else {
            return false;
        };

        let _guard = self.tenant_repair.lock().await;

        // A concurrent request may already have repaired the shared selection.
        let current = self.tenant.read().await.clone();
        if current.as_deref() != Some(rejected) {
            return current.is_some();
        }

        let session = match auth::authenticated_session(
            &self.http,
            &self.base_url,
            &self.credentials,
        )
        .await
        {
            Ok(session) => session,
            Err(error) => {
                warn!(%error, "could not read the current login while repairing tenant selection");
                return false;
            }
        };
        if let Some(configured) = &session.active_tenant
            && configured != rejected
        {
            *self.tenant.write().await = Some(configured.clone());
            return true;
        }
        let memberships =
            match auth::fetch_tenants(&self.http, &session.authority_url, &session.access_token)
                .await
            {
                Ok(memberships) => memberships,
                Err(error) => {
                    warn!(%error, "could not list memberships while repairing tenant selection");
                    return false;
                }
            };
        let [only] = memberships.as_slice() else {
            return false;
        };
        match auth::set_active_tenant(
            &session.stamp,
            Some(rejected.to_string()),
            Some(only.slug.clone()),
        )
        .await
        {
            Ok(true) => {
                *self.tenant.write().await = Some(only.slug.clone());
                true
            }
            Ok(false) => false,
            Err(error) => {
                warn!(%error, "could not persist repaired tenant selection");
                false
            }
        }
    }

    /// Whether the server reports `capability`.
    ///
    /// Asked, never inferred. An old server ignores a query parameter it does
    /// not know and answers as though it had applied it, so "the filter came
    /// back with rows" says nothing about whether the filter ran. A server too
    /// old to answer at all reports nothing, which is the right answer for it.
    pub async fn supports(&self, capability: &str) -> bool {
        let capabilities = self
            .capabilities
            .get_or_init(|| async {
                self.get::<api::Whoami>("/v1/whoami")
                    .await
                    .map(|whoami| whoami.capabilities)
                    .unwrap_or_default()
            })
            .await;

        capabilities.iter().any(|name| name == capability)
    }

    /// GET `path`, parse the JSON response as `T`. The path is appended
    /// to the base URL — pass it WITH leading slash (`/v1/domains`).
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let (resp, url) = self
            .send(reqwest::Method::GET, path, None, Idempotency::Idempotent)
            .await?;
        unwrap_envelope(resp, "GET", &url).await
    }

    /// Like [`Self::get`], but returns `Ok(None)` on a 404 instead of erroring —
    /// for "does this still exist?" probes (e.g. validating a cached codebase id
    /// before trusting it against the current server).
    pub async fn get_opt<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let (resp, url) = self
            .send(reqwest::Method::GET, path, None, Idempotency::Idempotent)
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        unwrap_envelope(resp, "GET", &url).await.map(Some)
    }

    /// Like [`Self::get`] but tolerates a successful `data: null` — returns
    /// `Ok(None)` instead of erroring. For endpoints that 200 with no payload to
    /// mean "nothing here" (e.g. hover at a position with no symbol).
    pub async fn get_maybe<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let (resp, url) = self
            .send(reqwest::Method::GET, path, None, Idempotency::Idempotent)
            .await?;
        let (_, envelope) = read_envelope::<T>(resp, "GET", &url).await?;
        Ok(envelope.data)
    }

    /// GET a flat paginated endpoint, returning the page. Distinct from
    /// [`Self::get`], which unwraps `data` as the entire response payload:
    /// paginated list endpoints put their rows (`data`, or legacy `items`) and
    /// page metadata beside `success` (see [`unwrap_page`]).
    pub async fn get_page<T: DeserializeOwned>(&self, path: &str) -> Result<api::Page<T>> {
        let (resp, url) = self
            .send(reqwest::Method::GET, path, None, Idempotency::Idempotent)
            .await?;
        unwrap_page(resp, "GET", &url).await
    }

    /// POST `path` with `body` serialised as JSON, parse the response
    /// as `T`. Same path semantics as [`Self::get`].
    ///
    /// The request can write, so a gateway or connection failure is not
    /// retried. Use [`Self::post_read`] for a request that only reads.
    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        self.post_with(path, body, Idempotency::NotIdempotent).await
    }

    /// POST `path` for a request that only reads, such as a search. The server
    /// holds the same state after it answers the request twice, so a gateway
    /// or connection failure is retried once, as for a GET. Parsed like
    /// [`Self::post`].
    pub async fn post_read<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.post_with(path, body, Idempotency::Idempotent).await
    }

    async fn post_with<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        idempotency: Idempotency,
    ) -> Result<T> {
        let body = serde_json::to_value(body).context("serialize POST body")?;
        let (resp, url) = self
            .send(reqwest::Method::POST, path, Some(body), idempotency)
            .await?;
        unwrap_envelope(resp, "POST", &url).await
    }

    /// PUT `path` with `body` serialised as JSON, parse the response as `T`.
    /// Same path / envelope semantics as [`Self::post`].
    pub async fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let body = serde_json::to_value(body).context("serialize PUT body")?;
        let (resp, url) = self
            .send(
                reqwest::Method::PUT,
                path,
                Some(body),
                Idempotency::NotIdempotent,
            )
            .await?;
        unwrap_envelope(resp, "PUT", &url).await
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with('/') {
            format!("{}{}", self.base_url, path)
        } else {
            format!("{}/{}", self.base_url, path)
        }
    }
}

/// The server uses `409 + Retry-After` only for typed, request-safe loading
/// states (`GraphLoading` / `FileLoading`). Bound each advertised delay so a
/// malformed or hostile response cannot park the CLI for an arbitrary period;
/// the overall retry loop has its own deadline as well.
fn loading_retry_delay(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> Option<Duration> {
    if status != reqwest::StatusCode::CONFLICT {
        return None;
    }
    failure::retry_after(headers).map(bounded_loading_delay)
}

/// The one bound for a delay that a loading response advertises. The retry
/// loop and the typed failure classification both use it.
fn bounded_loading_delay(delay: Duration) -> Duration {
    delay.clamp(LOADING_RETRY_MIN_DELAY, LOADING_RETRY_MAX_DELAY)
}

fn tenant_binding_denied(body: &str) -> bool {
    fn contains_code(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
                (key.eq_ignore_ascii_case("code")
                    && value
                        .as_str()
                        .is_some_and(|code| code.eq_ignore_ascii_case("TenantBindingDenied")))
                    || contains_code(value)
            }),
            serde_json::Value::Array(values) => values.iter().any(contains_code),
            _ => false,
        }
    }

    serde_json::from_str(body).is_ok_and(|value| contains_code(&value))
}

/// Every server response is an `ApiResponse<T>` envelope
/// (`{ success, errors, httpStatusCode, data }`); unwrap it to the inner
/// `data`, surfacing the typed errors on failure rather than a raw body.
async fn unwrap_envelope<T: DeserializeOwned>(
    resp: reqwest::Response,
    method: &str,
    url: &str,
) -> Result<T> {
    let (status, envelope) = read_envelope::<T>(resp, method, url).await?;
    envelope
        .data
        .ok_or_else(|| anyhow!("{method} {url} -> {status}: success but no data"))
}

/// Read one envelope answer. A failure by status or by the envelope's own flag
/// becomes an [`ApiFailure`] that carries the typed error code and the
/// server's retry delay.
async fn read_envelope<T: DeserializeOwned>(
    resp: reqwest::Response,
    method: &str,
    url: &str,
) -> Result<(reqwest::StatusCode, ApiEnvelope<T>)> {
    let status = resp.status();
    let retry_after = failure::retry_after(resp.headers());
    let body = resp
        .text()
        .await
        .with_context(|| format!("{method} {url}: read body"))?;
    // Not the envelope: attribute it to whatever answered instead of blaming
    // the parser. See `gateway_error`.
    let Ok(envelope) = serde_json::from_str::<ApiEnvelope<T>>(&body) else {
        return Err(gateway_error(method, url, status, retry_after, &body));
    };
    if !status.is_success() || !envelope.success {
        let errors = envelope.errors.as_deref().unwrap_or_default();
        return Err(ApiFailure::from_errors(method, url, status, retry_after, errors).into());
    }
    Ok((status, envelope))
}

/// The server's `ApiResponse<T>` envelope. `errors` is captured untyped — only
/// a failure reads it, for the first typed code and the human text.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiEnvelope<T> {
    success: bool,
    data: Option<T>,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
}

/// Unwrap a flat paginated envelope (`PaginatedApiResponse<T>` —
/// `{ success, errors, data, total, page, pageSize }`) into its [`api::Page`].
/// The list endpoints inline the rows and page metadata beside `success`; the
/// standard [`unwrap_envelope`] (which extracts `.data` as one value) doesn't
/// apply.
async fn unwrap_page<T: DeserializeOwned>(
    resp: reqwest::Response,
    method: &str,
    url: &str,
) -> Result<api::Page<T>> {
    let status = resp.status();
    let retry_after = failure::retry_after(resp.headers());
    let body = resp
        .text()
        .await
        .with_context(|| format!("{method} {url}: read body"))?;
    let env: PageEnvelope<T> = serde_json::from_str(&body).with_context(|| {
        format!("{method} {url} -> {status}: parse paginated envelope ({body})")
    })?;
    if !status.is_success() || !env.success {
        let errors = env.errors.as_deref().unwrap_or_default();
        return Err(ApiFailure::from_errors(method, url, status, retry_after, errors).into());
    }
    Ok(env.page)
}

/// The flat `PaginatedApiResponse<T>` envelope — the rows and page fields sit
/// beside `success`/`errors`. Current servers call the rows `data`; older ones
/// called them `items`; [`api::Page`] owns that compatibility in one place.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageEnvelope<T> {
    success: bool,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
    #[serde(flatten)]
    page: api::Page<T>,
}

/// Whether a codebase's `source_kind` is `Local` — the caller's own working
/// copy, as opposed to a server-pulled (`Vcs`) index. Accepts both wire forms
/// (the enum serializes as the number `0` today; tolerate a `"Local"` string
/// if a converter is ever added) so a server-side change can't silently make
/// every folder resolve to nothing.
pub fn is_local_source(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Number(n) => n.as_i64() == Some(0),
        serde_json::Value::String(s) => s.eq_ignore_ascii_case("local"),
        _ => false,
    }
}

pub mod api;

fn tenant_selection(configured: Option<String>, explicit: Option<&str>) -> (Option<String>, bool) {
    if let Some(tenant) = explicit {
        (Some(tenant.to_string()), false)
    } else {
        let repairable = configured.is_some();
        (configured, repairable)
    }
}

/// Build an authenticated `Client` for one session.
///
/// This is the real constructor: every selection the client makes comes from
/// `context` and the loaded config, never from the process. `transport` supplies
/// the shared HTTP client, and `remote_permits` the engine's bound on
/// concurrent request attempts. Both are supplied by the caller that owns them,
/// so a session cannot create a second connection pool or raise a bound.
pub(crate) fn from_context(
    context: &SessionContext,
    transport: &HttpTransport,
    remote_permits: Option<Arc<Semaphore>>,
) -> Result<Client> {
    let cfg = crate::config::load()?;
    let server = auth::normalize_server_url(&cfg.server_url(context.server.as_deref()))?;
    let configured_tenant = (auth::normalize_server_url(&cfg.persisted_server_url())? == server)
        .then(|| cfg.active_tenant.clone())
        .flatten();
    let (tenant, repair_configured_tenant) =
        tenant_selection(configured_tenant, context.tenant.as_deref());
    let codebase = cfg.active_codebase(context.codebase.as_deref());
    Ok(Client::new(
        transport,
        context.credentials.clone(),
        &server,
        tenant,
        codebase,
        repair_configured_tenant,
        remote_permits,
    ))
}

/// Convenience for a one-shot command: read this process as one session, give
/// it its own transport, and build its client. Long-lived callers that serve
/// several sessions build the context and the transport themselves and use
/// [`from_context`].
pub fn from_cli(cli: &crate::cli::Cli) -> Result<Client> {
    let context = SessionContext::from_process(cli)?;
    // A one-shot command sends one interactive request at a time, so it needs
    // no remote bound. Its uploads are bounded by the sync limits it builds.
    from_context(&context, &HttpTransport::new()?, None)
}

/// Like [`from_cli`], but ensures a codebase is set — resolving the session's
/// working directory's codebase when one wasn't configured explicitly. For the
/// codebase-scoped commands (`projects`, `graph …`) run inside a repo.
pub async fn for_cwd(cli: &crate::cli::Cli) -> Result<Client> {
    let context = SessionContext::from_process(cli)?;
    let client = from_context(&context, &HttpTransport::new()?, None)?;
    let dir = context.cwd;
    if client.codebase_raw().is_some() {
        return Ok(client.with_cached_local_root(Some(&dir)));
    }
    let id = crate::codebase::resolve(&client, &dir)
        .await?
        .map(|r| r.id)
        .ok_or_else(|| {
            anyhow!(
                "no codebase for {} — run `semctl index` first",
                dir.display()
            )
        })?;
    Ok(client.with_codebase(id).with_cached_local_root(Some(&dir)))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde::Deserialize;

    use super::{
        Client, CredentialSource, HttpTransport, PageEnvelope, gateway_error, loading_retry_delay,
        tenant_binding_denied, tenant_selection,
    };

    /// A client with no codebase and no checkout, for the pure selection tests.
    fn test_client() -> Client {
        Client::new(
            &HttpTransport::new().expect("build the test transport"),
            CredentialSource::Stored,
            "https://example.invalid",
            None,
            None,
            false,
            None,
        )
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Row {
        id: u32,
    }

    #[test]
    fn http_page_decoder_reads_standard_data_rows() {
        let page: PageEnvelope<Row> = serde_json::from_str(
            r#"{"success":true,"errors":null,"data":[{"id":1}],
                "page":0,"pageSize":25,"count":1,"total":1}"#,
        )
        .expect("the live server's standard page shape must parse");

        assert_eq!(page.page.items, vec![Row { id: 1 }]);
        assert_eq!(page.page.total, 1);
        assert_eq!(page.page.number, 0);
        assert_eq!(page.page.size, 25);
    }

    #[test]
    fn http_page_decoder_still_reads_legacy_item_rows() {
        let page: PageEnvelope<Row> = serde_json::from_str(
            r#"{"success":true,"errors":null,"items":[{"id":1}],
                "page":0,"pageSize":25,"total":1}"#,
        )
        .expect("the legacy page shape must keep parsing");

        assert_eq!(page.page.items, vec![Row { id: 1 }]);
    }

    /// A gateway's HTML error page must be reported as the gateway failure it is,
    /// not as a JSON parse error.
    ///
    /// The reported symptom was `parse response envelope ... expected value at
    /// line 1 column 1` for a 504 — which names this CLI's parser while the
    /// actual diagnosis (the gateway timed out) appears nowhere.
    #[test]
    fn a_gateway_html_page_is_reported_as_the_gateway_failing() {
        let msg = gateway_error(
            "PUT",
            "https://example/v1/codebases/x/sync/y",
            reqwest::StatusCode::GATEWAY_TIMEOUT,
            None,
            "<html><head><title>504 Gateway Time-out</title></head><body>...</body></html>",
        )
        .to_string();

        assert!(
            msg.contains("gateway timed out"),
            "must name the gateway timing out: {msg}"
        );
        assert!(
            msg.contains("504"),
            "must keep the status, which is the diagnosis: {msg}"
        );
        assert!(
            !msg.contains("expected value at line"),
            "must not surface the JSON parser's complaint as the headline: {msg}"
        );
    }

    #[test]
    fn tenant_denial_is_detected_in_direct_and_enveloped_errors() {
        assert!(tenant_binding_denied(
            r#"{"code":"TenantBindingDenied","message":"denied"}"#
        ));
        assert!(tenant_binding_denied(
            r#"{"success":false,"errors":[{"code":"TenantBindingDenied"}]}"#
        ));
        assert!(!tenant_binding_denied(
            r#"{"code":"InsufficientPermission","message":"denied"}"#
        ));
        assert!(!tenant_binding_denied("<html>forbidden</html>"));
    }

    #[test]
    fn only_persisted_tenants_are_eligible_for_automatic_repair() {
        assert_eq!(
            tenant_selection(Some("saved".into()), None),
            (Some("saved".into()), true)
        );
        assert_eq!(
            tenant_selection(Some("saved".into()), Some("override")),
            (Some("override".into()), false)
        );
        assert_eq!(tenant_selection(None, None), (None, false));
    }

    #[test]
    fn only_typed_loading_responses_receive_a_bounded_retry_delay() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "1".parse().unwrap());
        assert_eq!(
            loading_retry_delay(reqwest::StatusCode::CONFLICT, &headers),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            loading_retry_delay(reqwest::StatusCode::OK, &headers),
            None,
            "a successful response must never be replayed"
        );

        headers.insert(reqwest::header::RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(
            loading_retry_delay(reqwest::StatusCode::CONFLICT, &headers),
            Some(Duration::from_secs(5)),
            "one server response cannot stall the client beyond the delay cap"
        );
        headers.insert(reqwest::header::RETRY_AFTER, "invalid".parse().unwrap());
        assert_eq!(
            loading_retry_delay(reqwest::StatusCode::CONFLICT, &headers),
            None,
            "malformed retry instructions are surfaced normally"
        );
    }
    /// A checkout answers about itself; asking for canonical drops the claim
    /// that the caller is standing anywhere, which is how the server hears
    /// "tell me what the project publishes".
    #[test]
    fn asking_for_canonical_stops_claiming_a_checkout() {
        let mut client = test_client();
        client.checkout_source_id = Some("digest".into());
        client.local_root = Some(std::path::PathBuf::from("checkout"));

        assert_eq!(client.for_canonical().checkout_source_id, None);
        assert_eq!(client.for_canonical().local_root(), None);

        // A view, not a move: the checkout client stays usable, so one
        // canonical lookup cannot silently redirect the rest of a session to
        // the trunk.
        assert_eq!(client.checkout_source_id.as_deref(), Some("digest"));
        assert_eq!(client.local_root(), Some(std::path::Path::new("checkout")));
    }

    /// Outside a checkout there is nothing to drop, and canonical is already
    /// what every read resolves to.
    #[test]
    fn canonical_is_a_no_op_when_no_checkout_is_claimed() {
        let client = test_client();

        assert_eq!(client.for_canonical().checkout_source_id, None);
    }

    #[test]
    fn failed_checkout_identity_clears_local_binding() {
        let directory = tempfile::tempdir().unwrap();
        let mut client = Client::for_test("codebase", Some(directory.path().to_path_buf()));
        client.checkout_source_id = Some("old-checkout".into());
        let client = client.with_local_root(Some(directory.path().join("missing")));
        assert_eq!(client.checkout_source_id, None);
        assert_eq!(client.local_root(), None);
    }
}
