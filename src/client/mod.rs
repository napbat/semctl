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
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::auth;

const TENANT_HEADER: &str = "X-Tenant-Id";
/// The checkout a request is made from. The server prefers that copy of a
/// codebase for any read that is about one, so an agent working in a checkout
/// is answered about the tree it is looking at.
const CHECKOUT_HEADER: &str = "X-Semctx-Source-Id";
const LOADING_RETRY_BUDGET: Duration = Duration::from_mins(1);
const LOADING_RETRY_MAX_DELAY: Duration = Duration::from_secs(5);

/// Cheap to clone — `reqwest::Client` is internally `Arc`'d and the
/// other fields are small strings. The MCP server handler holds a
/// `Client` and rmcp requires it to be `Clone`.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
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
}

impl Client {
    fn new(
        base_url: &str,
        tenant: Option<String>,
        codebase: Option<String>,
        repair_configured_tenant: bool,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("semctx-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client build is infallible with default config");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            tenant: Arc::new(RwLock::new(tenant)),
            repair_configured_tenant,
            tenant_repair: Arc::new(Mutex::new(())),
            codebase,
            local_root: None,
            checkout_source_id: None,
            capabilities: Arc::new(tokio::sync::OnceCell::new()),
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

    /// Return a copy with the codebase's local checkout root set — used by
    /// `semctl mcp` so hit paths can be absolutized for the host.
    pub fn with_local_root(mut self, root: Option<PathBuf>) -> Self {
        // Derived here, once, rather than per request: it hashes the
        // installation id with the path, and every read would otherwise pay
        // for a file read it does not need.
        self.checkout_source_id = root
            .as_deref()
            .and_then(|dir| crate::codebase::checkout_source_id(dir).ok());
        self.local_root = root;
        self
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
        let token = auth::get_valid_access_token(&self.http).await?;
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

    /// Send one request, repairing a stale persisted tenant once and honoring
    /// the server's bounded `Retry-After` contract for transient graph/file
    /// projection restores.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(reqwest::Response, String)> {
        let mut tenant_retried = false;
        let loading_deadline = Instant::now() + LOADING_RETRY_BUDGET;
        loop {
            let (mut req, url, rejected_tenant) = self.authed(method.clone(), path).await?;
            if let Some(json) = &body {
                req = req.json(json);
            }
            let resp = req
                .send()
                .await
                .with_context(|| format!("{method} {url}"))?;

            if let Some(delay) = loading_retry_delay(resp.status(), resp.headers()) {
                if delay > loading_deadline.saturating_duration_since(Instant::now()) {
                    return Ok((resp, url));
                }
                debug!(
                    status = %resp.status(),
                    retry_after_ms = delay.as_millis(),
                    "server projection is restoring; retrying request"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            if resp.status() != reqwest::StatusCode::FORBIDDEN {
                return Ok((resp, url));
            }

            let status = resp.status();
            let response_body = resp
                .text()
                .await
                .with_context(|| format!("{method} {url}: read body"))?;
            if !tenant_retried
                && tenant_binding_denied(&response_body)
                && self
                    .repair_tenant_after_denial(rejected_tenant.as_deref())
                    .await
            {
                tenant_retried = true;
                continue;
            }
            return Err(response_body_error(
                method.as_str(),
                &url,
                status,
                &response_body,
            ));
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

        let mut cfg = match crate::config::load() {
            Ok(cfg) => cfg,
            Err(error) => {
                warn!(%error, "couldn't load config while repairing stale tenant");
                return false;
            }
        };

        // Honour a validated switch performed by another process while this MCP
        // server was running before making another identity round-trip.
        if let Some(configured) = cfg.active_tenant.clone()
            && configured != rejected
        {
            *self.tenant.write().await = Some(configured.clone());
            info!(tenant = %configured, "adopted updated active tenant");
            return true;
        }

        let token = match auth::get_valid_access_token(&self.http).await {
            Ok(token) => token,
            Err(error) => {
                warn!(%error, "couldn't get token while repairing stale tenant");
                return false;
            }
        };
        let identity_url = match auth::discover_authority(&self.http, &self.base_url).await {
            Ok(url) => url,
            Err(error) => {
                warn!(%error, "couldn't discover identity while repairing stale tenant");
                return false;
            }
        };
        let memberships = match auth::fetch_tenants(&self.http, &identity_url, &token).await {
            Ok(memberships) => memberships,
            Err(error) => {
                warn!(%error, "couldn't list memberships while repairing stale tenant");
                return false;
            }
        };
        let [only] = memberships.as_slice() else {
            return false;
        };

        cfg.active_tenant = Some(only.slug.clone());
        if let Err(error) = crate::config::save(&cfg) {
            warn!(%error, tenant = %only.slug, "repaired tenant for this session but couldn't save it");
        }
        *self.tenant.write().await = Some(only.slug.clone());
        info!(
            rejected_tenant = %rejected,
            tenant = %only.slug,
            "repaired stale active tenant; retrying request"
        );
        true
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
        let (resp, url) = self.send(reqwest::Method::GET, path, None).await?;
        unwrap_envelope(resp, "GET", &url).await
    }

    /// Like [`Self::get`], but returns `Ok(None)` on a 404 instead of erroring —
    /// for "does this still exist?" probes (e.g. validating a cached codebase id
    /// before trusting it against the current server).
    pub async fn get_opt<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let (resp, url) = self.send(reqwest::Method::GET, path, None).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        unwrap_envelope(resp, "GET", &url).await.map(Some)
    }

    /// Like [`Self::get`] but tolerates a successful `data: null` — returns
    /// `Ok(None)` instead of erroring. For endpoints that 200 with no payload to
    /// mean "nothing here" (e.g. hover at a position with no symbol).
    pub async fn get_maybe<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let (resp, url) = self.send(reqwest::Method::GET, path, None).await?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .with_context(|| format!("GET {url}: read body"))?;
        let envelope: ApiEnvelope<T> = match serde_json::from_str(&body) {
            Ok(e) => e,
            Err(_) => return Err(gateway_error("GET", &url, status, &body)),
        };
        if !status.is_success() || !envelope.success {
            bail!("GET {url} -> {status}: {}", envelope.error_summary());
        }
        Ok(envelope.data)
    }

    /// GET a flat paginated endpoint, returning the page. Distinct from
    /// [`Self::get`], which unwraps the `data` envelope: paginated list endpoints
    /// inline the page (`items`/`total`/`page`/`pageSize`) beside `success` with
    /// no `data` wrapper (see [`unwrap_page`]).
    pub async fn get_page<T: DeserializeOwned>(&self, path: &str) -> Result<api::Page<T>> {
        let (resp, url) = self.send(reqwest::Method::GET, path, None).await?;
        unwrap_page(resp, "GET", &url).await
    }

    /// POST `path` with `body` serialised as JSON, parse the response
    /// as `T`. Same path semantics as [`Self::get`].
    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let body = serde_json::to_value(body).context("serialize POST body")?;
        let (resp, url) = self.send(reqwest::Method::POST, path, Some(body)).await?;
        unwrap_envelope(resp, "POST", &url).await
    }

    /// PUT `path` with `body` serialised as JSON, parse the response as `T`.
    /// Same path / envelope semantics as [`Self::post`].
    pub async fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let body = serde_json::to_value(body).context("serialize PUT body")?;
        let (resp, url) = self.send(reqwest::Method::PUT, path, Some(body)).await?;
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
    let seconds = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds.max(1)).min(LOADING_RETRY_MAX_DELAY))
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

/// Preserve a structured JSON denial even when it is not wrapped in the
/// resource server's usual API envelope. Tenant binding failures can be emitted
/// by middleware before controller envelope handling runs.
fn response_body_error(
    method: &str,
    url: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> anyhow::Error {
    if serde_json::from_str::<serde_json::Value>(body).is_ok() {
        anyhow!("{method} {url} -> {status}: {body}")
    } else {
        gateway_error(method, url, status, body)
    }
}

/// An error for a response that is not the API's JSON envelope at all.
///
/// A gateway between the CLI and the server (ingress, proxy, load balancer)
/// answers failures in ITS format, not the API's — typically an HTML error page.
/// Parsing that as the envelope produces `expected value at line 1 column 1`,
/// which names the CLI's own parser rather than the thing that actually went
/// wrong, and buries the status code that IS the diagnosis.
///
/// Reported by status instead, because those statuses have specific meanings a
/// user can act on: 502/503/504 come from the gateway, not the application, and
/// mean the request never got a real answer.
fn gateway_error(
    method: &str,
    url: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> anyhow::Error {
    let hint = match status.as_u16() {
        504 => "the gateway timed out waiting for the server — the request may still be running",
        502 => "the gateway could not reach the server, or the server closed the connection",
        503 => "the server is unavailable behind the gateway (starting, draining, or overloaded)",
        _ => "the response was not the API's JSON envelope",
    };
    // A short excerpt only: an HTML error page is pages long and none of it is
    // the diagnosis, but a truncated peek still distinguishes "HTML page" from
    // "empty body" when someone needs it.
    let excerpt: String = body.trim().chars().take(120).collect();
    anyhow!("{method} {url} -> {status}: {hint} (response was not JSON: {excerpt:?})")
}

/// Every server response is an `ApiResponse<T>` envelope
/// (`{ success, errors, httpStatusCode, data }`); unwrap it to the inner
/// `data`, surfacing the typed errors on failure rather than a raw body.
async fn unwrap_envelope<T: DeserializeOwned>(
    resp: reqwest::Response,
    method: &str,
    url: &str,
) -> Result<T> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .with_context(|| format!("{method} {url}: read body"))?;
    let envelope: ApiEnvelope<T> = match serde_json::from_str(&body) {
        Ok(e) => e,
        // Not the envelope: attribute it to whatever answered instead of blaming
        // the parser. See `gateway_error`.
        Err(_) => return Err(gateway_error(method, url, status, &body)),
    };
    if !status.is_success() || !envelope.success {
        bail!("{method} {url} -> {status}: {}", envelope.error_summary());
    }
    envelope
        .data
        .ok_or_else(|| anyhow!("{method} {url} -> {status}: success but no data"))
}

/// The server's `ApiResponse<T>` envelope. `errors` is captured untyped — the
/// CLI only renders it on failure, so its exact shape doesn't matter here.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiEnvelope<T> {
    success: bool,
    data: Option<T>,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
}

impl<T> ApiEnvelope<T> {
    fn error_summary(&self) -> String {
        summarize_errors(self.errors.as_deref().unwrap_or_default())
    }
}

fn summarize_errors(errors: &[serde_json::Value]) -> String {
    if errors.is_empty() {
        return "request failed".to_string();
    }
    errors
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Unwrap a flat paginated envelope (`PaginatedApiResponse<T>` —
/// `{ success, errors, items, total, page, pageSize }`, no `data`) into its
/// [`api::Page`]. The list endpoints inline the page beside `success`; the
/// standard [`unwrap_envelope`] (which extracts `.data`) doesn't apply.
async fn unwrap_page<T: DeserializeOwned>(
    resp: reqwest::Response,
    method: &str,
    url: &str,
) -> Result<api::Page<T>> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .with_context(|| format!("{method} {url}: read body"))?;
    let env: PageEnvelope<T> = serde_json::from_str(&body).with_context(|| {
        format!("{method} {url} -> {status}: parse paginated envelope ({body})")
    })?;
    if !status.is_success() || !env.success {
        bail!(
            "{method} {url} -> {status}: {}",
            summarize_errors(env.errors.as_deref().unwrap_or_default())
        );
    }
    Ok(api::Page {
        items: env.items,
        total: env.total,
        number: env.page,
        size: env.page_size,
    })
}

/// The flat `PaginatedApiResponse<T>` envelope — the page fields sit beside
/// `success`/`errors` (no `data` wrapper).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageEnvelope<T> {
    success: bool,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
    #[serde(default = "Vec::new")]
    items: Vec<T>,
    #[serde(default)]
    total: u32,
    #[serde(default)]
    page: u32,
    #[serde(default)]
    page_size: u32,
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

/// Build an authenticated `Client` from the loaded config + the global CLI flags.
pub fn from_cli(cli: &crate::cli::Cli) -> Result<Client> {
    let cfg = crate::config::load()?;
    let server = cfg.server_url(cli.server.as_deref());
    let (tenant, repair_configured_tenant) =
        tenant_selection(cfg.active_tenant.clone(), cli.tenant.as_deref());
    let codebase = cfg.active_codebase(cli.codebase.as_deref());
    Ok(Client::new(
        &server,
        tenant,
        codebase,
        repair_configured_tenant,
    ))
}

/// Like [`from_cli`], but ensures a codebase is set — resolving the working
/// directory's codebase when one wasn't configured explicitly. For the
/// codebase-scoped commands (`projects`, `graph …`) run inside a repo.
pub async fn for_cwd(cli: &crate::cli::Cli) -> Result<Client> {
    let client = from_cli(cli)?;
    if client.codebase_raw().is_some() {
        return Ok(client);
    }
    let dir = std::env::current_dir().context("read working directory")?;
    let id = crate::codebase::resolve(&client, &dir)
        .await?
        .map(|r| r.id)
        .ok_or_else(|| {
            anyhow!(
                "no codebase for {} — run `semctl index` first",
                dir.display()
            )
        })?;
    Ok(client.with_codebase(id))
}

#[cfg(test)]
mod gateway_error_tests {
    use std::time::Duration;

    use super::{gateway_error, loading_retry_delay, tenant_binding_denied, tenant_selection};

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
}
