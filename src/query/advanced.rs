//! P0 inspection and P1 workspace-edit query surfaces.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;

use crate::client::{Client, api};

use super::{local_path, render_hits, urlencode};

const CODEBASE_PAGE_SIZE: u32 = 500;
const MAX_CODEBASE_PAGES: u32 = 100;

/// The codebases holding a copy of `source_id`. Empty when the server cannot
/// answer — a diagnostic must not fail the command it is describing.
async fn codebases_for_source(client: &Client, source_id: &str) -> Vec<String> {
    client
        .get_page::<api::CodebaseSummary>(&format!(
            "/v1/codebases?sourceId={}&page=0&pageSize={CODEBASE_PAGE_SIZE}",
            urlencode(source_id)
        ))
        .await
        .map(|page| page.items.into_iter().map(|row| row.id).collect())
        .unwrap_or_default()
}

/// How this codebase's copies relate to the checkout we are standing in.
async fn describe_copies(client: &Client, codebase_id: &str, source_id: &str) -> String {
    let Ok(copies) = client
        .get::<Vec<api::CodebaseVersionSummary>>(&format!(
            "/v1/codebases/{codebase_id}/versions"
        ))
        .await
    else {
        return "unknown".into();
    };
    let mine = copies.iter().any(|copy| copy.source_id == source_id);
    match (mine, copies.len()) {
        (true, 1) => "this checkout".into(),
        (true, n) => format!("this checkout, of {n}"),
        (false, 0) => "none".into(),
        (false, n) => format!("{n} other copy/copies"),
    }
}

pub async fn list_codebases(client: &Client) -> String {
    match fetch_codebases(client).await {
        Ok(codebases) if codebases.is_empty() => "no visible codebases".into(),
        Ok(codebases) => {
            let local_source = client
                .local_root()
                .and_then(|root| crate::codebase::checkout_source_id(root).ok());
            // Which of them hold a copy of THIS checkout. Asked of the
            // server: a project can hold several copies, and only it knows
            // whose is whose.
            let mine = match &local_source {
                Some(source) => codebases_for_source(client, source).await,
                None => Vec::new(),
            };
            let mut out = String::new();
            for codebase in &codebases {
                let this_checkout = mine.iter().any(|id| id == &codebase.id);
                writeln!(
                    out,
                    "{}  {}  {}\n  source={} access={} revision={} graph={}/{} fresh={} this_checkout={}",
                    codebase.id,
                    codebase.slug,
                    if codebase.display_name.is_empty() {
                        "-"
                    } else {
                        &codebase.display_name
                    },
                    enum_label(&codebase.source_kind, &["Local", "Vcs"]),
                    enum_label(
                        &codebase.visibility,
                        &["Personal", "Organization", "Global"]
                    ),
                    codebase.revision.as_deref().unwrap_or("-"),
                    codebase.graph_generation,
                    codebase.graph_materialized_generation,
                    codebase.graph_fresh,
                    this_checkout,
                )
                .unwrap();
            }
            out
        }
        Err(error) => format!("list_codebases failed: {error:#}"),
    }
}

pub async fn current_context(
    client: &Client,
    watcher_active: bool,
    last_job_id: Option<&str>,
) -> String {
    let tenant = client.tenant().await.unwrap_or_else(|| "(unset)".into());
    let root = client
        .local_root()
        .map_or_else(|| "(none)".into(), |path| path.display().to_string());
    let source = client
        .local_root()
        .and_then(|path| crate::codebase::checkout_source_id(path).ok())
        .unwrap_or_else(|| "(none)".into());
    let codebase = match client.codebase() {
        Ok(value) => value.to_string(),
        Err(_) => "(unbound)".into(),
    };
    let mut out = format!(
        "server {}\ntenant {}\ncodebase {}\ncheckout root {}\nsource identity {}\nwatcher {}",
        client.server_url(),
        tenant,
        codebase,
        root,
        source,
        if watcher_active { "active" } else { "inactive" }
    );

    if codebase != "(unbound)" {
        match client
            .get::<api::CodebaseSummary>(&format!("/v1/codebases/{codebase}"))
            .await
        {
            Ok(summary) => {
                write!(
                    out,
                    "\nindexed revision {}\ngraph generation {}/{}\ngraph fresh {}\nserver checkout binding {}",
                    summary.revision.as_deref().unwrap_or("-"),
                    summary.graph_generation,
                    summary.graph_materialized_generation,
                    summary.graph_fresh,
                    describe_copies(client, &codebase, &source).await
                )
                .unwrap();
            }
            Err(error) => write!(out, "\ngraph/index state unavailable: {error}").unwrap(),
        }
    }
    if let Some(job) = last_job_id {
        write!(out, "\nlast session sync job {job}").unwrap();
    }
    out
}

pub async fn read_source(
    client: &Client,
    path: &str,
    revision: Option<&str>,
    byte_range: Option<(u64, u64)>,
    line_range: Option<(u32, u32)>,
) -> String {
    let cb = match client.codebase() {
        Ok(value) => value,
        Err(error) => return format!("read_source failed: {error}"),
    };
    let mut url = format!("/v1/codebases/{cb}/files/content?path={}", urlencode(path));
    if let Some(revision) = revision {
        write!(url, "&revision={}", urlencode(revision)).unwrap();
    }
    if let Some((start, end)) = byte_range {
        write!(url, "&byteStart={start}&byteEnd={end}").unwrap();
    }
    if let Some((start, end)) = line_range {
        write!(url, "&lineStart={start}&lineEnd={end}").unwrap();
    }
    let content = match client.get::<api::FileContent>(&url).await {
        Ok(content) => content,
        Err(error) => return format!("read_source failed: {error}"),
    };

    let mut source = "content-plane";
    let mut body = content.content.clone();
    if let Some(root) = client.local_root()
        && let Some(local) = verified_local_window(root, path, &content)
    {
        source = "verified-local";
        body = local;
    }
    format!(
        "{}\nrevision {}\nbytes {}-{} of {}\nencoding {}\nsource {}{}\n\n{}",
        local_path(client.local_root(), &content.path),
        content.content_hash,
        content.byte_start,
        content.byte_end,
        content.size,
        content.encoding,
        source,
        if content.truncated {
            " (truncated)"
        } else {
            ""
        },
        body
    )
}

pub struct SymbolSearchOptions<'a> {
    pub query: &'a str,
    pub mode: &'a str,
    pub kinds: &'a [String],
    pub path_prefix: Option<&'a str>,
    pub project: Option<&'a str>,
    pub language: Option<&'a str>,
    pub limit: u32,
}

pub async fn search_symbols(client: &Client, options: &SymbolSearchOptions<'_>) -> String {
    let cb = match client.codebase() {
        Ok(value) => value,
        Err(error) => return format!("search_symbols failed: {error}"),
    };
    let mut url = format!(
        "/v1/codebases/{cb}/graph/symbols?query={}&mode={}&limit={}",
        urlencode(options.query),
        urlencode(options.mode),
        options.limit.clamp(1, 500)
    );
    for kind in options.kinds {
        write!(url, "&kinds={}", urlencode(kind)).unwrap();
    }
    for (name, value) in [
        ("pathPrefix", options.path_prefix),
        ("project", options.project),
        ("language", options.language),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            write!(url, "&{name}={}", urlencode(value)).unwrap();
        }
    }
    match client.get::<Vec<api::SymbolSearchHit>>(&url).await {
        Ok(hits) if hits.is_empty() => format!("no symbols match `{}`", options.query),
        Ok(hits) => {
            let mut out = String::new();
            for hit in &hits {
                writeln!(
                    out,
                    "{}:{}-{}  {}  {}  [{} project={} score={}]",
                    local_path(client.local_root(), &hit.path),
                    hit.line_start,
                    hit.line_end,
                    hit.qualified_name,
                    hit.kind,
                    hit.language,
                    hit.project.as_deref().unwrap_or("-"),
                    hit.score
                )
                .unwrap();
            }
            out
        }
        Err(error) => format!("search_symbols failed: {error}"),
    }
}

pub async fn type_hierarchy(client: &Client, symbol: &str, direction: &str, depth: u32) -> String {
    let cb = match client.codebase() {
        Ok(value) => value,
        Err(error) => return format!("type_hierarchy failed: {error}"),
    };
    let url = format!(
        "/v1/codebases/{cb}/graph/type-hierarchy?symbol={}&direction={}&depth={}",
        urlencode(symbol),
        urlencode(direction),
        depth.clamp(1, 16)
    );
    match client.get::<api::TypeHierarchy>(&url).await {
        Ok(hierarchy) => render_type_hierarchy(client, &hierarchy),
        Err(error) => format!("type_hierarchy failed: {error}"),
    }
}

pub async fn call_graph(client: &Client, symbol: &str, depth: u32, direction: &str) -> String {
    let cb = match client.codebase() {
        Ok(value) => value,
        Err(error) => return format!("call_graph failed: {error}"),
    };
    let url = format!(
        "/v1/codebases/{cb}/graph/call-graph?symbol={}&depth={}&direction={}",
        urlencode(symbol),
        depth.clamp(1, 10),
        urlencode(direction)
    );
    match client.get::<api::CallGraph>(&url).await {
        Ok(graph) if graph.nodes.is_empty() => format!("no call graph for `{symbol}`"),
        Ok(graph) => {
            let mut out = format!("nodes ({})\n", graph.nodes.len());
            out.push_str(&render_hits(&graph.nodes, "", client.local_root(), false));
            writeln!(out, "edges ({})", graph.edges.len()).unwrap();
            for edge in &graph.edges {
                writeln!(out, "  {} -> {}", edge.from, edge.to).unwrap();
            }
            out
        }
        Err(error) => format!("call_graph failed: {error}"),
    }
}

pub async fn cycles(client: &Client) -> String {
    graph_clusters(client, "cycles", "cycles").await
}

pub async fn duplicates(client: &Client) -> String {
    graph_clusters(client, "duplicates", "duplicate groups").await
}

pub async fn unused(client: &Client, page: u32, page_size: u32) -> String {
    let cb = match client.codebase() {
        Ok(value) => value,
        Err(error) => return format!("unused failed: {error}"),
    };
    let url = format!(
        "/v1/codebases/{cb}/graph/unused?page={page}&pageSize={}",
        page_size.clamp(1, 500)
    );
    match client.get_page::<api::UnusedDefinition>(&url).await {
        Ok(result) if result.items.is_empty() => "no unused definitions".into(),
        Ok(result) => {
            let mut out = String::new();
            for item in &result.items {
                writeln!(
                    out,
                    "{}\n  reason: {}\n  caveat: {}",
                    super::hit_location(&item.definition, client.local_root()),
                    item.reason,
                    item.completeness_caveat
                )
                .unwrap();
                if let Some(identity) = item.definition.qualified_symbol.as_deref() {
                    writeln!(out, "  symbol: {identity}").unwrap();
                }
            }
            writeln!(
                out,
                "page {}: {} of {}",
                result.number,
                result.items.len(),
                result.total
            )
            .unwrap();
            out
        }
        Err(error) => format!("unused failed: {error}"),
    }
}

pub async fn plan_rename(
    client: &Client,
    request: &api::RenameSymbolRequest,
) -> Result<api::WorkspaceEditPlan> {
    client.post(&edit_url(client, "rename")?, request).await
}

pub async fn plan_safe_delete(
    client: &Client,
    request: &api::SafeDeleteSymbolRequest,
) -> Result<api::WorkspaceEditPlan> {
    client
        .post(&edit_url(client, "safe-delete")?, request)
        .await
}

pub async fn plan_replace_body(
    client: &Client,
    request: &api::ReplaceSymbolBodyRequest,
) -> Result<api::WorkspaceEditPlan> {
    client
        .post(&edit_url(client, "replace-body")?, request)
        .await
}

pub async fn plan_insert(
    client: &Client,
    request: &api::InsertSymbolRequest,
    before: bool,
) -> Result<api::WorkspaceEditPlan> {
    client
        .post(
            &edit_url(
                client,
                if before {
                    "insert-before"
                } else {
                    "insert-after"
                },
            )?,
            request,
        )
        .await
}

pub fn render_edit_plan(plan: &api::WorkspaceEditPlan) -> String {
    serde_json::to_string_pretty(plan)
        .unwrap_or_else(|error| format!("render plan failed: {error}"))
}

async fn fetch_codebases(client: &Client) -> Result<Vec<api::CodebaseSummary>> {
    let mut all = Vec::new();
    for page in 0..MAX_CODEBASE_PAGES {
        let result = client
            .get_page::<api::CodebaseSummary>(&format!(
                "/v1/codebases?page={page}&pageSize={CODEBASE_PAGE_SIZE}"
            ))
            .await?;
        let consumed = result.number.saturating_add(1).saturating_mul(result.size);
        all.extend(result.items);
        if result.size == 0 || consumed >= result.total {
            break;
        }
    }
    Ok(all)
}

fn enum_label(value: &serde_json::Value, numeric: &[&str]) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            value
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| numeric.get(index).copied())
                .map(str::to_string)
        })
        .unwrap_or_else(|| value.to_string())
}

fn verified_local_window(root: &Path, path: &str, content: &api::FileContent) -> Option<String> {
    let target = std::fs::canonicalize(root.join(path)).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    if !target.starts_with(root) {
        return None;
    }
    let bytes = std::fs::read(target).ok()?;
    if blake3::hash(&bytes).to_hex().as_str() != content.content_hash {
        return None;
    }
    let start = usize::try_from(content.byte_start).ok()?;
    let end = usize::try_from(content.byte_end).ok()?;
    let window = bytes.get(start..end)?;
    if content.is_binary {
        use base64::Engine as _;
        Some(base64::engine::general_purpose::STANDARD.encode(window))
    } else {
        String::from_utf8(window.to_vec()).ok()
    }
}

fn render_type_hierarchy(client: &Client, hierarchy: &api::TypeHierarchy) -> String {
    let mut out = format!(
        "roots: {}\ncomplete: {}{}\n",
        hierarchy.roots.join(", "),
        hierarchy.complete,
        hierarchy
            .caveat
            .as_deref()
            .map(|caveat| format!(" ({caveat})"))
            .unwrap_or_default()
    );
    writeln!(out, "nodes ({})", hierarchy.nodes.len()).unwrap();
    for node in &hierarchy.nodes {
        let location = node.path.as_deref().map_or_else(
            || "external".into(),
            |path| {
                format!(
                    "{}:{}-{}",
                    local_path(client.local_root(), path),
                    node.line_start.unwrap_or_default(),
                    node.line_end.unwrap_or_default()
                )
            },
        );
        writeln!(out, "  {}  {}  {}", node.identity, node.kind, location).unwrap();
    }
    writeln!(out, "edges ({})", hierarchy.edges.len()).unwrap();
    for edge in &hierarchy.edges {
        writeln!(
            out,
            "  {} -> {}  {} {} depth={}",
            edge.subtype, edge.supertype, edge.relation, edge.origin, edge.depth
        )
        .unwrap();
    }
    out
}

async fn graph_clusters(client: &Client, endpoint: &str, label: &str) -> String {
    let cb = match client.codebase() {
        Ok(value) => value,
        Err(error) => return format!("{endpoint} failed: {error}"),
    };
    let url = format!("/v1/codebases/{cb}/graph/{endpoint}");
    match client.get::<Vec<api::GraphCluster>>(&url).await {
        Ok(groups) if groups.is_empty() => format!("no {label}"),
        Ok(groups) => {
            let mut out = String::new();
            for group in &groups {
                writeln!(
                    out,
                    "group {} ({} locations){}",
                    group.index,
                    group.size,
                    group
                        .hash
                        .as_deref()
                        .map(|hash| format!(" hash={hash}"))
                        .unwrap_or_default()
                )
                .unwrap();
                for hit in &group.chunks {
                    writeln!(
                        out,
                        "  {}  {}",
                        super::hit_location(hit, client.local_root()),
                        hit.qualified_symbol
                            .as_deref()
                            .or(hit.symbol.as_deref())
                            .unwrap_or("-")
                    )
                    .unwrap();
                }
            }
            out
        }
        Err(error) => format!("{endpoint} failed: {error}"),
    }
}

fn edit_url(client: &Client, operation: &str) -> Result<String> {
    Ok(format!(
        "/v1/codebases/{}/edits/{operation}",
        client.codebase()?
    ))
}
