//! Hand-typed bindings for the server's REST surface. Field shapes mirror the
//! C# records 1:1 (`Semctx.Server.Api.V1.*`).
//!
//! `dead_code` is allowed module-wide: several fields exist purely to match the
//! wire shape (so deserialization doesn't drop data and the types stay an honest
//! mirror of the server) even when no command prints them yet.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// `GET /v1/domains` row. `#[serde(rename_all = "camelCase")]`
/// keeps Rust field names `snake_case` while the wire stays
/// camelCase to match the .NET server.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainDescriptor {
    pub id: String,
    pub display_name: String,
    pub tag_schema: Vec<TagSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagSpec {
    pub name: String,
    pub description: String,
    pub data_type: String,
}

/// `POST /v1/search` body. `filters` is left as
/// `serde_json::Value` until the CLI exposes the typed
/// `TagCondition` taxonomy.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequestBody<'a> {
    pub query: &'a str,
    pub top_k: u32,
    /// Scope to a single codebase when the MCP server is launched for one.
    /// Omitted = search everything the caller can access.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codebase_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<Vec<serde_json::Value>>,
    /// Restrict to these chunk kinds (`function`/`container`/`block`). The
    /// server folds it into a tag filter the engine applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    /// Ranking bias — `"Code"` or `"Docs"` (server-side reorder).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefer: Option<String>,
    /// `"Symbol"` returns full enclosing-symbol bodies (server-side snap).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granularity: Option<String>,
}

/// `POST /v1/search` response row.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub domain_id: String,
    pub id: String,
    pub score: f64,
    pub path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    /// Where the cursor belongs inside the match — a declaration's NAME token,
    /// which a leading attribute or doc comment puts below `line_start`.
    pub focus_line: Option<u32>,
    pub language: Option<String>,
    pub symbol: Option<String>,
    /// The declaration CONTAINING the match: `run_until` for a `dispatch` call
    /// inside it. Absent when the match IS the declaration.
    pub enclosing_symbol: Option<String>,
    /// Reference sites only: `true` when the use WRITES the symbol.
    pub is_write: Option<bool>,
    /// Chunk kind — "block", "container", or "function".
    #[serde(default)]
    pub kind: String,
    pub snippet: String,
}

/// `GET /v1/codebases/{id}/graph/outline` response — a file's table of
/// contents, one entry per indexed chunk in source order.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOutline {
    pub path: String,
    pub entries: Vec<FileOutlineEntry>,
}

/// One row of a [`FileOutline`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOutlineEntry {
    pub line_start: u32,
    pub line_end: u32,
    pub kind: String,
    pub symbol: Option<String>,
}

/// One `POST .../graph/batch` result — a requested symbol + its hits.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolHits {
    pub symbol: String,
    pub hits: Vec<SearchHit>,
}

/// One node of `GET .../files/tree` — a file or a directory. Directories
/// carry `children`; files carry `size`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeNode {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub children: Option<Vec<FileTreeNode>>,
}

/// One `GET /v1/codebases/{id}/grep` match — file, 1-based line, line text.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepMatch {
    pub path: String,
    pub line_number: u32,
    pub line: String,
}

/// One page of `GET /v1/codebases/{id}/files` (the catalog file list).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u32,
    #[serde(rename = "page")]
    pub number: u32,
    #[serde(rename = "pageSize")]
    pub size: u32,
}

/// A `GET /v1/codebases/{id}/files` row — the catalog's per-file record.
/// (`status` is an enum the server serializes as a number, so it's left off
/// here; path + size are all the listing needs.)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodebaseFile {
    pub path: String,
    pub size: i64,
    /// Blake3 hex of the file's indexed content — the same hash the sync
    /// manifest sends (see [`ManifestEntry::hash`]). Compared against a
    /// freshly-hashed local file to flag results that lag a local edit.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// When the file's chunks were last embedded. Carried for completeness
    /// (the staleness check keys on `content_hash`).
    #[serde(default)]
    pub embedded_at: Option<String>,
}

/// `GET /v1/codebases/{id}/graph/trace` — a symbol's definition plus its
/// direct callers and callees, pre-classified by the server.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceResult {
    pub definition: Vec<SearchHit>,
    pub callers: Vec<SearchHit>,
    pub callees: Vec<SearchHit>,
}

/// One file→file import edge — `GET .../graph/imports`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportEdge {
    pub from: String,
    pub to: String,
    pub import_path: String,
}

/// One reference→definition binding — `GET .../graph/symbol-edges`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolEdge {
    pub from_file: String,
    pub name: String,
    pub to_file: String,
    pub moniker: String,
}

/// One resolved cross-codebase link — `GET .../graph/external-links`.
/// Keyed by package coordinate (`ecosystem` + `target_package`) and
/// `descriptor`; `confidence` ranks candidates when several visible
/// codebases satisfy the same coordinate.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalLink {
    pub from_file: String,
    pub import_path: String,
    pub ecosystem: String,
    pub target_package: String,
    pub descriptor: String,
    pub target_codebase_id: String,
    pub target_file: String,
    pub target_name: String,
    pub confidence: i32,
}

/// `GET /v1/codebases/{id}/projects` — the detected project graph.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGraph {
    pub projects: Vec<ProjectDto>,
    pub groups: Vec<ProjectGroupDto>,
}

/// A leaf project (one buildable unit, rooted at a manifest).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDto {
    pub kind: String,
    pub name: String,
    pub root: String,
    pub config_file: Option<String>,
    pub file_count: u32,
}

/// A group (workspace / solution) containing leaf projects or sub-groups.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGroupDto {
    pub kind: String,
    pub name: String,
    pub root: String,
    pub children: Vec<String>,
}

/// A `GET /v1/codebases` row — enough to match the working directory to
/// its codebase by remote URL or slug, and to restrict the match to the
/// caller's *local* working copies. `source_kind` is captured raw (the
/// server serializes the enum as a number, `0` = Local) and tested with
/// [`is_local_source`](super::is_local_source).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodebaseSummary {
    pub id: String,
    pub slug: String,
    pub remote_url: Option<String>,
    #[serde(default)]
    pub source_kind: serde_json::Value,
}

/// `POST /v1/codebases` body. `sourceKind` and `visibility` are omitted on
/// purpose — the server defaults them to `Local` / `Personal`, exactly what
/// a locally-indexed working copy should be. `vcs` is sent only for a git
/// checkout.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCodebaseRequest<'a> {
    pub slug: &'a str,
    pub display_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcs: Option<CodebaseVcsInfo>,
}

/// VCS metadata for a create request. `vcs` (the system discriminator) is
/// omitted so the server defaults it to Git.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodebaseVcsInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_name: Option<String>,
    pub dirty: bool,
}

/// One file in a `POST /v1/codebases/{id}/sync` manifest.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub path: String,
    pub hash: String,
    pub size: i64,
}

/// `POST /v1/codebases/{id}/sync` body — the full current file list.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncManifestRequest {
    pub files: Vec<ManifestEntry>,
    /// Opaque identity of the local checkout issuing this full manifest.
    pub source_id: String,
}

/// The diff the server computed — paths whose content must be uploaded, and
/// paths it will delete. (`status` is an enum-number on the wire; skipped.)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlan {
    pub job_id: String,
    pub need_content: Vec<String>,
    pub to_delete: Vec<String>,
}

/// One uploaded file in a `PUT /v1/codebases/{id}/sync/{jobId}` body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncFileContent {
    pub path: String,
    pub content: String,
    /// The content's manifest hash. Lets a later sync plan see the path is
    /// already staged server-side and skip re-requesting its content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// `PUT /v1/codebases/{id}/sync/{jobId}` body — content for the changed paths.
/// A multi-batch upload marks every batch but the last `final: false` (the
/// server stages them); the last — possibly empty — PUT completes the sync
/// and queues the job. Absent means final, so one-PUT uploads are unchanged.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncContentRequest {
    pub files: Vec<SyncFileContent>,
    /// Must match the source that created the sync plan.
    pub source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#final: Option<bool>,
}

/// `GET /v1/jobs/{id}` — an index job's progress. `completed_at` is the
/// terminal signal (set once the worker finishes, success or fail);
/// `error` is set only on failure.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStatus {
    #[serde(default)]
    pub files_to_embed: i64,
    #[serde(default)]
    pub files_to_delete: i64,
    pub files_embedded: i64,
    #[serde(default)]
    pub files_deleted: i64,
    pub files_failed: i64,
    #[serde(default)]
    pub chunk_count: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
    /// Set once the worker picks the job up (i.e. it's running).
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

/// Identity's `GET /v1/tenants` envelope. We hit identity directly
/// for tenant discovery — tenant membership is identity's
/// source-of-truth, not the semctx server's surface.
#[derive(Debug, Deserialize)]
pub struct TenantsEnvelope {
    pub success: bool,
    pub data: Option<TenantsPage>,
}

#[derive(Debug, Deserialize)]
pub struct TenantsPage {
    pub items: Vec<TenantDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantDto {
    pub id: String,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub role_name: Option<String>,
}
