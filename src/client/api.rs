//! Hand-typed bindings for the server's REST surface. Field shapes mirror the
//! C# records 1:1 (`Semctx.Server.Api.V1.*`).
//!
//! `dead_code` is allowed module-wide: several fields exist purely to match the
//! wire shape (so deserialization doesn't drop data and the types stay an honest
//! mirror of the server) even when no command prints them yet.
#![allow(dead_code)]

use schemars::JsonSchema;
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
    pub codebase_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
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
    #[serde(default)]
    pub byte_start: Option<u32>,
    #[serde(default)]
    pub byte_end: Option<u32>,
    /// Where the cursor belongs inside the match — a declaration's NAME token,
    /// which a leading attribute or doc comment puts below `line_start`.
    pub focus_line: Option<u32>,
    #[serde(default)]
    pub focus_byte: Option<u32>,
    #[serde(default)]
    pub snippet_line_start: Option<u32>,
    #[serde(default)]
    pub snippet_line_end: Option<u32>,
    #[serde(default)]
    pub snippet_byte_start: Option<u32>,
    #[serde(default)]
    pub snippet_byte_end: Option<u32>,
    pub language: Option<String>,
    pub symbol: Option<String>,
    #[serde(default)]
    pub qualified_symbol: Option<String>,
    /// The declaration CONTAINING the match: `run_until` for a `dispatch` call
    /// inside it. Absent when the match IS the declaration.
    pub enclosing_symbol: Option<String>,
    /// Reference sites only: `true` when the use WRITES the symbol.
    pub is_write: Option<bool>,
    #[serde(default)]
    pub reference_namespace: Option<String>,
    #[serde(default)]
    pub reference_kind: Option<String>,
    #[serde(default)]
    pub external_target: Option<String>,
    #[serde(default)]
    pub codebase_id: Option<String>,
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
    pub chunk_id: String,
    pub line_start: u32,
    pub line_end: u32,
    pub kind: String,
    pub symbol: Option<String>,
    #[serde(default)]
    pub qualified_symbol: Option<String>,
    #[serde(default)]
    pub symbol_kind: Option<String>,
    #[serde(default)]
    pub depth: u32,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    pub path: String,
    pub content_hash: String,
    pub content: String,
    pub encoding: String,
    pub is_binary: bool,
    pub size: i64,
    pub byte_start: i64,
    pub byte_end: i64,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub truncated: bool,
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

/// One page of a paged endpoint — the rows plus where they sit.
///
/// `items` reads both shapes the server may send. semctx is moving its list
/// endpoints onto the standard napbat envelope, where the rows are `data`; the
/// older shape called them `items`. Accepting both is what lets a semctl that
/// is already installed keep working across that change — this binary cannot be
/// upgraded in lockstep with the server, so it has to be the tolerant side.
///
/// Drop the alias once no server sends the old name.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    #[serde(rename = "data", alias = "items")]
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallEdge {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallGraph {
    pub nodes: Vec<SearchHit>,
    pub edges: Vec<CallEdge>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphCluster {
    pub index: u32,
    pub size: u32,
    pub chunks: Vec<SearchHit>,
    pub hash: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnusedDefinition {
    pub definition: SearchHit,
    pub reason: String,
    pub completeness_caveat: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolSearchHit {
    pub codebase_id: String,
    pub path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub byte_start: u32,
    pub byte_end: u32,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub project: Option<String>,
    pub language: String,
    pub score: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeHierarchyNode {
    pub identity: String,
    pub name: String,
    pub qualified_name: String,
    pub path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub byte_start: Option<u32>,
    pub byte_end: Option<u32>,
    pub kind: String,
    pub external: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeHierarchyEdge {
    pub subtype: String,
    pub supertype: String,
    pub relation: String,
    pub origin: String,
    pub depth: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeHierarchy {
    pub roots: Vec<String>,
    pub nodes: Vec<TypeHierarchyNode>,
    pub edges: Vec<TypeHierarchyEdge>,
    pub complete: bool,
    pub caveat: Option<String>,
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

/// A `GET /v1/codebases` row. Registration uses `slug` to recover a concurrent
/// deterministic checkout registration and `source_kind` to ensure it only
/// adopts a Local codebase. The server serializes that enum as the number `0`
/// today; [`is_local_source`](super::is_local_source) also tolerates a future
/// string representation.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodebaseSummary {
    pub id: String,
    pub slug: String,
    #[serde(default)]
    pub display_name: String,
    pub remote_url: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub visibility: serde_json::Value,
    #[serde(default)]
    pub source_kind: serde_json::Value,
    #[serde(default)]
    pub local_sync_source_id: Option<String>,
    #[serde(default)]
    pub graph_generation: i64,
    #[serde(default)]
    pub graph_materialized_generation: i64,
    #[serde(default)]
    pub graph_artifact_manifest_hash: Option<String>,
    #[serde(default)]
    pub graph_fresh: bool,
    #[serde(default)]
    pub local_checkout_bound: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SymbolTargetRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Independent server protocol options.
pub struct RenameSymbolRequest {
    pub target: SymbolTargetRequest,
    pub new_name: String,
    pub include_comments: bool,
    pub include_strings: bool,
    pub include_unresolved_text: bool,
    pub allow_uncertain: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeDeleteSymbolRequest {
    pub target: SymbolTargetRequest,
    pub allow_uncertain: bool,
    pub allow_public_without_known_consumers: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflection_patterns: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceSymbolBodyRequest {
    pub target: SymbolTargetRequest,
    pub replacement: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertSymbolRequest {
    pub target: SymbolTargetRequest,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EditSite {
    pub path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub byte_start: u64,
    pub byte_end: u64,
    pub category: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ByteEdit {
    pub start: u64,
    pub end: u64,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileEditPlan {
    pub path: String,
    pub preimage_hash: String,
    pub edits: Vec<ByteEdit>,
    pub expected_postimage_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FormatterStep {
    pub program: String,
    pub arguments: Vec<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEditPlan {
    pub schema_version: u32,
    pub plan_id: String,
    pub operation: String,
    pub codebase_id: String,
    pub graph_generation: u64,
    pub source_identity: String,
    pub graph_complete: bool,
    pub provider_generations_current: bool,
    pub dependent_codebases: Vec<String>,
    pub applicable: bool,
    pub confidence: String,
    pub files: Vec<FileEditPlan>,
    pub warnings: Vec<String>,
    pub refusal_reasons: Vec<String>,
    pub unresolved_sites: Vec<EditSite>,
    pub uncertain_sites: Vec<EditSite>,
    pub formatter: Option<FormatterStep>,
    pub rendered_diff: String,
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

/// Identity's `GET /v1/tenants` response. We hit identity directly for tenant
/// discovery — tenant membership is identity's source of truth, not the semctx
/// server's surface.
#[derive(Debug, Deserialize)]
pub struct TenantsEnvelope {
    pub success: bool,
    pub data: Option<TenantsPage>,
}

/// The tenants Identity returned, in whichever shape it sent them.
///
/// Identity is moving its list endpoints onto the standard napbat envelope,
/// where `data` is the rows; before that `data` was an object holding `items`.
///
/// semctl is installed on machines and cannot be upgraded in step with the
/// server, so it reads both. Without this, an older binary fails at login —
/// tenant selection is part of `semctl auth login` — and the only fix available
/// to the person holding it is to upgrade before they can authenticate.
///
/// Drop `Legacy` once no deployed Identity sends it.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum TenantsPage {
    Rows(Vec<TenantDto>),
    Legacy { items: Vec<TenantDto> },
}

impl TenantsPage {
    /// The rows, however they arrived.
    pub fn into_rows(self) -> Vec<TenantDto> {
        match self {
            Self::Rows(rows) => rows,
            Self::Legacy { items } => items,
        }
    }
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

#[cfg(test)]
mod envelope_tests {
    use super::*;

    /// A row shape shared by both cases below, so the assertions differ only in
    /// where the rows were found.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Row {
        id: u32,
    }

    #[test]
    fn a_page_reads_the_standard_envelope() {
        let page: Page<Row> = serde_json::from_str(
            r#"{"success":true,"httpStatusCode":200,"errors":null,
                "data":[{"id":1}],"page":0,"pageSize":25,"count":1,"total":1,
                "totalPages":1,"remainingItems":0,"remainingPages":0,"hasMore":false}"#,
        )
        .expect("the standard envelope must parse");

        assert_eq!(page.items, vec![Row { id: 1 }]);
        assert_eq!(page.total, 1);
        assert_eq!(page.size, 25);
    }

    #[test]
    fn a_page_still_reads_the_older_shape() {
        // An installed semctl talks to whatever server it finds. Until every
        // deployment has moved, that is sometimes one that still says `items`.
        let page: Page<Row> = serde_json::from_str(
            r#"{"success":true,"items":[{"id":1}],"total":1,"page":0,"pageSize":25}"#,
        )
        .expect("the older shape must still parse");

        assert_eq!(page.items, vec![Row { id: 1 }]);
    }

    #[test]
    fn tenants_read_the_standard_envelope() {
        let envelope: TenantsEnvelope = serde_json::from_str(
            r#"{"success":true,"data":[{"id":"t1","name":"Napbat","slug":"napbat"}],
                "page":0,"pageSize":1000,"total":1}"#,
        )
        .expect("the standard envelope must parse");

        let rows = envelope.data.map(TenantsPage::into_rows).unwrap_or_default();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, "napbat");
    }

    #[test]
    fn tenants_still_read_the_older_shape() {
        // Tenant selection happens during `semctl auth login`. If this stopped
        // parsing, the only fix available to the person holding the binary
        // would be to upgrade it before they could authenticate.
        let envelope: TenantsEnvelope = serde_json::from_str(
            r#"{"success":true,"data":{"items":[{"id":"t1","name":"Napbat","slug":"napbat"}],
                "total":1,"page":0,"pageSize":1000}}"#,
        )
        .expect("the older shape must still parse");

        let rows = envelope.data.map(TenantsPage::into_rows).unwrap_or_default();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, "napbat");
    }
}
