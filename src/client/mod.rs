//! HTTP client for the semctx server. Thin wrapper over `reqwest` that
//! handles bearer-auth + the X-Tenant-Id header + JSON request/response.
//!
//! Typed request/response bodies live in [`api`] — hand-written to match
//! the server's controllers for now. When the `OpenAPI` spec stabilises
//! we'll swap [`api`] for a `progenitor`-generated client driven off a
//! vendored `openapi/v1.json` snapshot.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Serialize, de::DeserializeOwned};

use crate::auth;

const TENANT_HEADER: &str = "X-Tenant-Id";

/// Cheap to clone — `reqwest::Client` is internally `Arc`'d and the
/// other fields are small strings. The MCP server handler holds a
/// `Client` and rmcp requires it to be `Clone`.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    tenant: Option<String>,
    codebase: Option<String>,
    /// Local checkout root of `codebase`, when known (recorded by
    /// `semctl index`). Lets path-rendering absolutize the server's
    /// codebase-relative hit paths into Read-ready absolute paths. `None`
    /// for canonical / server-pulled codebases that have no local bytes.
    local_root: Option<PathBuf>,
}

impl Client {
    pub fn new(base_url: &str, tenant: Option<String>, codebase: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("semctx-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client build is infallible with default config");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            tenant,
            codebase,
            local_root: None,
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
        self.local_root = root;
        self
    }

    /// The codebase's local checkout root, if known. See [`Self::local_root`]
    /// field docs for when this is `None`.
    pub fn local_root(&self) -> Option<&Path> {
        self.local_root.as_deref()
    }

    /// Build an authenticated request for `method path`: attaches the bearer
    /// token and the `X-Tenant-Id` header, returning the builder alongside the
    /// resolved URL (for error context). The verb-specific body shaping
    /// (`.json(body)`) and response unwrapping stay with each caller.
    async fn authed(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<(reqwest::RequestBuilder, String)> {
        let token = auth::get_valid_access_token(&self.http).await?;
        let url = self.url(path);
        let mut req = self.http.request(method, &url).bearer_auth(&token);
        if let Some(t) = &self.tenant {
            req = req.header(TENANT_HEADER, t);
        }
        Ok((req, url))
    }

    /// GET `path`, parse the JSON response as `T`. The path is appended
    /// to the base URL — pass it WITH leading slash (`/v1/domains`).
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let (req, url) = self.authed(reqwest::Method::GET, path).await?;
        let resp = req.send().await.with_context(|| format!("GET {url}"))?;
        unwrap_envelope(resp, "GET", &url).await
    }

    /// Like [`Self::get`], but returns `Ok(None)` on a 404 instead of erroring —
    /// for "does this still exist?" probes (e.g. validating a cached codebase id
    /// before trusting it against the current server).
    pub async fn get_opt<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let (req, url) = self.authed(reqwest::Method::GET, path).await?;
        let resp = req.send().await.with_context(|| format!("GET {url}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        unwrap_envelope(resp, "GET", &url).await.map(Some)
    }

    /// Like [`Self::get`] but tolerates a successful `data: null` — returns
    /// `Ok(None)` instead of erroring. For endpoints that 200 with no payload to
    /// mean "nothing here" (e.g. hover at a position with no symbol).
    pub async fn get_maybe<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let (req, url) = self.authed(reqwest::Method::GET, path).await?;
        let resp = req.send().await.with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .with_context(|| format!("GET {url}: read body"))?;
        let envelope: ApiEnvelope<T> = serde_json::from_str(&body)
            .with_context(|| format!("GET {url} -> {status}: parse response envelope ({body})"))?;
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
        let (req, url) = self.authed(reqwest::Method::GET, path).await?;
        let resp = req.send().await.with_context(|| format!("GET {url}"))?;
        unwrap_page(resp, "GET", &url).await
    }

    /// POST `path` with `body` serialised as JSON, parse the response
    /// as `T`. Same path semantics as [`Self::get`].
    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let (req, url) = self.authed(reqwest::Method::POST, path).await?;
        let resp = req
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        unwrap_envelope(resp, "POST", &url).await
    }

    /// PUT `path` with `body` serialised as JSON, parse the response as `T`.
    /// Same path / envelope semantics as [`Self::post`].
    pub async fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let (req, url) = self.authed(reqwest::Method::PUT, path).await?;
        let resp = req
            .json(body)
            .send()
            .await
            .with_context(|| format!("PUT {url}"))?;
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
    let envelope: ApiEnvelope<T> = serde_json::from_str(&body)
        .with_context(|| format!("{method} {url} -> {status}: parse response envelope ({body})"))?;
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

/// Build an authenticated `Client` from the loaded config + the global CLI flags.
pub fn from_cli(cli: &crate::cli::Cli) -> Result<Client> {
    let cfg = crate::config::load()?;
    let server = cfg.server_url(cli.server.as_deref());
    let tenant = cfg.active_tenant(cli.tenant.as_deref());
    let codebase = cfg.active_codebase(cli.codebase.as_deref());
    Ok(Client::new(&server, tenant, codebase))
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
