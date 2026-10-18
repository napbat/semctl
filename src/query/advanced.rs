//! P0 inspection and P1 workspace-edit query surfaces.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::client::{Client, api};

use super::render::{markdown_cell, markdown_code, render_fenced, source_language_hint};
use super::{ToolError, bound_codebase, local_path, render_hits, urlencode};

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

/// Show server copy identities without inferring per-copy graph state.
async fn describe_copies(client: &Client, codebase_id: &str, source_id: Option<&str>) -> String {
    let copies = match client
        .get::<Vec<api::CodebaseVersionSummary>>(&format!("/v1/codebases/{codebase_id}/versions"))
        .await
    {
        Ok(copies) => copies,
        Err(error) => {
            return format!(
                "## Indexed copies\n\nCopies unavailable: {}\n",
                super::render::markdown_code(&format!("{error:#}"))
            );
        }
    };
    let mut out = String::from("## Indexed copies\n\n");
    if copies.is_empty() {
        out.push_str("(no indexed copies)\n");
        return out;
    }
    out.push_str(
        "| Copy | Source identity | Canonical | Checkout | Revision | Files |\n\
         | --- | --- | --- | --- | --- | ---: |\n",
    );
    for copy in copies {
        let checkout = source_id.map_or("unknown", |source| {
            if copy.source_id == source {
                "current"
            } else {
                "other"
            }
        });
        writeln!(
            out,
            "| {} | {} | {} | {checkout} | {} | {} |",
            markdown_cell(&copy.id),
            markdown_cell(&copy.source_id),
            copy.is_canonical,
            markdown_cell(copy.revision.as_deref().unwrap_or("-")),
            copy.files
        )
        .expect("writing to a String cannot fail");
    }
    out
}

pub async fn list_codebases(client: &Client) -> Result<String, ToolError> {
    match fetch_codebases(client).await {
        Ok(codebases) if codebases.is_empty() => Ok("no visible codebases".into()),
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
                .expect("writing to a String cannot fail");
            }
            let mut rendered = String::new();
            render_fenced(&mut rendered, &out, None);
            Ok(rendered)
        }
        Err(error) => Err(ToolError::from_client("list_codebases", &error)),
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
        .and_then(|path| crate::codebase::checkout_source_id(path).ok());
    let codebase = match client.codebase() {
        Ok(value) => value.to_string(),
        Err(_) => "(unbound)".into(),
    };
    let mut out = format!(
        "| Field | Value |\n| --- | --- |\n\
         | server | {} |\n| tenant | {} |\n| codebase | {} |\n\
         | checkout root | {} |\n| source identity | {} |\n| watcher | {} |\n",
        markdown_cell(client.server_url()),
        markdown_cell(&tenant),
        markdown_cell(&codebase),
        markdown_cell(&root),
        markdown_cell(source.as_deref().unwrap_or("(none)")),
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
                    "| indexed revision | {} |\n| graph generation | {}/{} |\n\
                     | graph fresh | {} |\n",
                    markdown_cell(summary.revision.as_deref().unwrap_or("-")),
                    summary.graph_generation,
                    summary.graph_materialized_generation,
                    summary.graph_fresh
                )
                .expect("writing to a String cannot fail");
            }
            Err(error) => writeln!(
                out,
                "| graph/index state unavailable | {} |",
                markdown_cell(&error.to_string())
            )
            .expect("writing to a String cannot fail"),
        }
    }
    if let Some(job) = last_job_id {
        writeln!(out, "| last session sync job | {} |", markdown_cell(job))
            .expect("writing to a String cannot fail");
    }
    if codebase != "(unbound)" {
        out.push('\n');
        out.push_str(&describe_copies(client, &codebase, source.as_deref()).await);
    }
    out
}

pub async fn read_source(
    client: &Client,
    path: &str,
    revision: Option<&str>,
    byte_range: Option<(u64, u64)>,
    line_range: Option<(u32, u32)>,
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "read_source")?;
    let mut url = format!("/v1/codebases/{cb}/files/content?path={}", urlencode(path));
    if let Some(revision) = revision {
        write!(url, "&revision={}", urlencode(revision)).expect("writing to a String cannot fail");
    }
    if let Some((start, end)) = byte_range {
        write!(url, "&byteStart={start}&byteEnd={end}").expect("writing to a String cannot fail");
    }
    if let Some((start, end)) = line_range {
        write!(url, "&lineStart={start}&lineEnd={end}").expect("writing to a String cannot fail");
    }
    let content = client
        .get::<api::FileContent>(&url)
        .await
        .map_err(|error| ToolError::from_client("read_source", &error))?;

    let local = client
        .local_root()
        .and_then(|root| verified_local_window(root, path, &content));
    let (source, body) = local
        .as_deref()
        .map_or(("content-plane", content.content.as_str()), |body| {
            ("verified-local", body)
        });
    let mut out = format!(
        "{} · bytes {}-{} of {} · source {}",
        markdown_code(&local_path(client.local_root(), &content.path)),
        content.byte_start,
        content.byte_end,
        content.size,
        markdown_code(source),
    );
    if let (Some(start), Some(end)) = (content.line_start, content.line_end) {
        write!(out, " · lines {start}-{end}").expect("writing to a String cannot fail");
    }
    if content.truncated {
        out.push_str(" · **truncated**");
    }
    out.push('\n');
    let language = (!content.is_binary)
        .then(|| source_language_hint(&content.path))
        .flatten();
    render_fenced(&mut out, body, language);
    writeln!(
        out,
        "Revision: {} · Encoding: {} · Codebase: {}",
        markdown_code(&content.content_hash),
        markdown_code(&content.encoding),
        markdown_code(cb),
    )
    .expect("writing to a String cannot fail");
    Ok(out)
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

pub async fn search_symbols(
    client: &Client,
    options: &SymbolSearchOptions<'_>,
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "search_symbols")?;
    let mut url = format!(
        "/v1/codebases/{cb}/graph/symbols?query={}&mode={}&limit={}",
        urlencode(options.query),
        urlencode(options.mode),
        options.limit.clamp(1, 500)
    );
    for kind in options.kinds {
        write!(url, "&kinds={}", urlencode(kind)).expect("writing to a String cannot fail");
    }
    for (name, value) in [
        ("pathPrefix", options.path_prefix),
        ("project", options.project),
        ("language", options.language),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            write!(url, "&{name}={}", urlencode(value)).expect("writing to a String cannot fail");
        }
    }
    match client.get::<Vec<api::SymbolSearchHit>>(&url).await {
        Ok(hits) if hits.is_empty() => Ok(format!("no symbols match `{}`", options.query)),
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
                .expect("writing to a String cannot fail");
            }
            Ok(out)
        }
        Err(error) => Err(ToolError::from_client("search_symbols", &error)),
    }
}

pub async fn type_hierarchy(
    client: &Client,
    symbol: &str,
    direction: &str,
    depth: u32,
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "type_hierarchy")?;
    let url = format!(
        "/v1/codebases/{cb}/graph/type-hierarchy?symbol={}&direction={}&depth={}",
        urlencode(symbol),
        urlencode(direction),
        depth.clamp(1, 16)
    );
    match client.get::<api::TypeHierarchy>(&url).await {
        Ok(hierarchy) => Ok(render_type_hierarchy(client, &hierarchy)),
        Err(error) => Err(ToolError::from_client("type_hierarchy", &error)),
    }
}

pub async fn call_graph(
    client: &Client,
    symbol: &str,
    depth: u32,
    direction: &str,
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "call_graph")?;
    let url = format!(
        "/v1/codebases/{cb}/graph/call-graph?symbol={}&depth={}&direction={}",
        urlencode(symbol),
        depth.clamp(1, 10),
        urlencode(direction)
    );
    match client.get::<api::CallGraph>(&url).await {
        Ok(graph) if graph.nodes.is_empty() => Ok(format!("no call graph for `{symbol}`")),
        Ok(graph) => {
            let mut out = format!("## Nodes ({})\n\n", graph.nodes.len());
            out.push_str(&render_hits(&graph.nodes, "", client.local_root(), false));
            writeln!(out, "## Edges ({})\n", graph.edges.len())
                .expect("writing to a String cannot fail");
            if !graph.edges.is_empty() {
                out.push_str("| From | To |\n| --- | --- |\n");
            }
            for edge in &graph.edges {
                writeln!(
                    out,
                    "| {} | {} |",
                    markdown_cell(&edge.from),
                    markdown_cell(&edge.to)
                )
                .expect("writing to a String cannot fail");
            }
            Ok(out)
        }
        Err(error) => Err(ToolError::from_client("call_graph", &error)),
    }
}

pub async fn cycles(client: &Client) -> Result<String, ToolError> {
    graph_clusters(client, "cycles", "cycles").await
}

pub async fn duplicates(client: &Client) -> Result<String, ToolError> {
    graph_clusters(client, "duplicates", "duplicate groups").await
}

pub async fn unused(client: &Client, page: u32, page_size: u32) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "unused")?;
    let url = format!(
        "/v1/codebases/{cb}/graph/unused?page={page}&pageSize={}",
        page_size.clamp(1, 500)
    );
    match client.get_page::<api::UnusedDefinition>(&url).await {
        Ok(result) if result.items.is_empty() => Ok("no unused definitions".into()),
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
                .expect("writing to a String cannot fail");
                if let Some(identity) = item.definition.qualified_symbol.as_deref() {
                    writeln!(out, "  symbol: {identity}").expect("writing to a String cannot fail");
                }
            }
            writeln!(
                out,
                "page {}: {} of {}",
                result.number,
                result.items.len(),
                result.total
            )
            .expect("writing to a String cannot fail");
            let mut rendered = String::new();
            render_fenced(&mut rendered, &out, None);
            Ok(rendered)
        }
        Err(error) => Err(ToolError::from_client("unused", &error)),
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

pub fn render_edit_plan(plan: &api::WorkspaceEditPlan) -> Result<String> {
    serde_json::to_string_pretty(plan).context("render plan")
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
    writeln!(out, "nodes ({})", hierarchy.nodes.len()).expect("writing to a String cannot fail");
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
        writeln!(out, "  {}  {}  {}", node.identity, node.kind, location)
            .expect("writing to a String cannot fail");
    }
    writeln!(out, "edges ({})", hierarchy.edges.len()).expect("writing to a String cannot fail");
    for edge in &hierarchy.edges {
        writeln!(
            out,
            "  {} -> {}  {} {} depth={}",
            edge.subtype, edge.supertype, edge.relation, edge.origin, edge.depth
        )
        .expect("writing to a String cannot fail");
    }
    let mut rendered = String::new();
    render_fenced(&mut rendered, &out, None);
    rendered
}

async fn graph_clusters(
    client: &Client,
    endpoint: &'static str,
    label: &str,
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, endpoint)?;
    let url = format!("/v1/codebases/{cb}/graph/{endpoint}");
    match client.get::<Vec<api::GraphCluster>>(&url).await {
        Ok(groups) if groups.is_empty() => Ok(format!("no {label}")),
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
                .expect("writing to a String cannot fail");
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
                    .expect("writing to a String cannot fail");
                }
            }
            let mut rendered = String::new();
            render_fenced(&mut rendered, &out, None);
            Ok(rendered)
        }
        Err(error) => Err(ToolError::from_client(endpoint, &error)),
    }
}

fn edit_url(client: &Client, operation: &str) -> Result<String> {
    Ok(format!(
        "/v1/codebases/{}/edits/{operation}",
        client.codebase()?
    ))
}

#[cfg(test)]
mod tests {
    use super::{current_context, describe_copies, read_source, verified_local_window};
    use crate::client::Client;
    use crate::client::stub::{Reply, Stub};
    use crate::query::render::{
        markdown_cell, render_fenced, render_outline, source_language_hint,
    };

    const COPIES: &str = r#"[
        {"id":"canonical","sourceId":"vcs-source","isCanonical":true,"revision":"canonical-rev","files":99},
        {"id":"selected","sourceId":"checkout-2","isCanonical":false,"revision":"selected-rev","files":42},
        {"id":"older","sourceId":"checkout-20","isCanonical":false,"revision":"older-rev","files":17},
        {"id":"case-distinct","sourceId":"CHECKOUT-2","isCanonical":false,"revision":null,"files":3}
    ]"#;

    #[test]
    fn source_hints_delegate_suffix_aliases_and_reject_unsafe_fence_tokens() {
        for (path, language) in [
            ("source.rs", "rs"),
            ("source.RS", "rs"),
            ("source.C", "cpp"),
            ("source.c", "c"),
            ("source.h", "h"),
            ("source.S", "s"),
            ("source.ps1", "ps1"),
            ("CMakeLists.txt", "cmake"),
            ("Makefile", "makefile"),
            ("Dockerfile", "dockerfile"),
            ("Dockerfile.custom", "custom"),
            ("source.unknown", "unknown"),
            ("source.mermaid", "text"),
            ("source.MERMAID", "text"),
            ("source.svg", "xml"),
            ("source.rs\n```", "text"),
            ("README", "text"),
        ] {
            let mut text = String::new();
            render_fenced(&mut text, "body", source_language_hint(path));
            assert!(text.starts_with(&format!("```{language}\n")));
            let outline = serde_json::from_value(serde_json::json!({
                "path": path,
                "entries": [{
                    "chunkId": "one", "lineStart": 2, "lineEnd": 3,
                    "kind": "function", "body": "\r\n    ```\r\n"
                }]
            }))
            .unwrap();
            let text = render_outline(&outline, None);
            assert!(text.contains(&format!("\n````{language}\n\r\n    ```\r\n````\n")));
        }
        for invalid in [None, Some(""), Some("rust\n```")] {
            let mut text = String::new();
            render_fenced(&mut text, "body", invalid);
            assert!(text.starts_with("```text\n"));
        }
    }

    #[tokio::test]
    async fn source_results_keep_full_bodies_and_place_bounds_before_provenance() {
        let source = format!("\r\n    ```\r\n{}", "    source line\r\n".repeat(3_000));
        for (path, binary, language) in [
            ("remote/source.rs", false, "rs"),
            ("remote/source.unknown", false, "unknown"),
            ("remote/source.mermaid", false, "text"),
            ("remote/source.svg", false, "xml"),
            ("remote/source.rs", true, "text"),
        ] {
            let body = if binary { "YmluYXJ5AA==" } else { &source };
            let end = body.len() + 20;
            let response = serde_json::json!({
                "path": path, "contentHash": "pinned-hash", "content": body,
                "encoding": if binary { "base64" } else { "utf-8" },
                "isBinary": binary, "size": end + 100,
                "byteStart": 20, "byteEnd": end, "lineStart": 4, "lineEnd": 8,
                "truncated": true
            });
            let stub = Stub::serve(vec![Reply::ok(&response.to_string())]).await;
            let client = Client::for_test_server(&stub.url).with_codebase("A".into());
            let text = read_source(
                &client,
                path,
                Some("pinned-hash"),
                Some((20, u64::try_from(end).unwrap())),
                None,
            )
            .await
            .unwrap();
            let heading = text.lines().next().unwrap();
            assert!(heading.contains(path));
            assert!(heading.contains(&format!("20-{end} of {}", end + 100)));
            assert!(heading.contains("lines 4-8"));
            assert!(heading.contains("content-plane"));
            assert!(heading.contains("truncated"));
            let fence = if binary { "```" } else { "````" };
            let separator = if body.ends_with('\n') { "" } else { "\n" };
            assert!(text.contains(&format!("{fence}{language}\n{body}{separator}{fence}\n")));
            let body_end = text.find(body).unwrap() + body.len();
            assert!(text[body_end..].contains("pinned-hash"));
            assert!(text[body_end..].contains("Codebase: `A`"));
            assert_eq!(stub.requests(), 1);
            assert!(
                stub.request_lines()[0]
                    .contains(&format!("revision=pinned-hash&byteStart=20&byteEnd={end}"))
            );
        }
    }

    #[test]
    fn verified_local_windows_require_matching_content_identity_and_preserve_bytes() {
        let root = tempfile::tempdir().unwrap();
        let source = "prefix\r\n    exact source\t\r\nsuffix";
        std::fs::write(root.path().join("source.rs"), source).unwrap();
        let mut content: crate::client::api::FileContent = serde_json::from_value(serde_json::json!({
            "path": "source.rs", "contentHash": blake3::hash(source.as_bytes()).to_hex().to_string(),
            "content": "server body", "encoding": "utf-8", "isBinary": false,
            "size": source.len(), "byteStart": 6, "byteEnd": source.len() - 6,
            "truncated": false
        })).unwrap();
        assert_eq!(
            verified_local_window(root.path(), "source.rs", &content).as_deref(),
            Some("\r\n    exact source\t\r\n"),
        );
        content.content_hash = "different-revision".into();
        assert!(verified_local_window(root.path(), "source.rs", &content).is_none());
    }

    #[tokio::test]
    async fn indexed_copies_keep_distinct_identities_and_match_the_exact_checkout_source() {
        let stub = Stub::serve(vec![Reply::ok(COPIES)]).await;
        let client = Client::for_test_server(&stub.url);
        let text = describe_copies(&client, "A", Some("checkout-2")).await;

        for (id, source, canonical, checkout, revision, files) in [
            (
                "canonical",
                "vcs-source",
                true,
                "other",
                "canonical-rev",
                99,
            ),
            (
                "selected",
                "checkout-2",
                false,
                "current",
                "selected-rev",
                42,
            ),
            ("older", "checkout-20", false, "other", "older-rev", 17),
            ("case-distinct", "CHECKOUT-2", false, "other", "-", 3),
        ] {
            let row = text
                .lines()
                .find(|line| line.contains(&markdown_cell(id)))
                .unwrap();
            assert!(row.contains(&markdown_cell(source)), "{row}");
            assert!(
                row.contains(&format!("| {canonical} | {checkout} |")),
                "{row}"
            );
            assert!(row.contains(&markdown_cell(revision)), "{row}");
            assert!(row.ends_with(&format!("| {files} |")), "{row}");
        }
        assert_eq!(stub.requests(), 1);
        assert_eq!(
            stub.request_lines(),
            ["GET /v1/codebases/A/versions HTTP/1.1"]
        );
    }

    #[tokio::test]
    async fn indexed_copy_cells_cannot_create_columns_or_select_a_missing_checkout() {
        let id = "copy\\|`id`\n# not a heading";
        let source = "(none)";
        let revision = "rev|```";
        let data = serde_json::json!([{
            "id": id,
            "sourceId": source,
            "isCanonical": true,
            "revision": revision,
            "files": 5
        }]);
        let stub = Stub::serve(vec![Reply::ok(&data.to_string())]).await;
        let client = Client::for_test_server(&stub.url);
        let text = describe_copies(&client, "A", None).await;

        let row = text
            .lines()
            .find(|line| line.contains(&markdown_cell(id)))
            .unwrap();
        assert!(row.contains(&markdown_cell(source)));
        assert!(row.contains(&markdown_cell(revision)));
        assert!(row.contains("| true | unknown |"));
        assert!(!text.contains("\n# not a heading"));
        assert_eq!(stub.requests(), 1);
    }

    #[tokio::test]
    async fn indexed_copy_failures_keep_native_details_without_opening_markdown_blocks() {
        let message = "native\n# message with | and ```";
        let stub = Stub::serve(vec![Reply::Answer {
            status: "400 Bad Request",
            headers: Vec::new(),
            body: serde_json::json!({
                "success": false,
                "errors": [{"code": "CopyUnavailable", "message": message}]
            })
            .to_string(),
        }])
        .await;
        let client = Client::for_test_server(&stub.url);
        let text = describe_copies(&client, "A", Some("checkout")).await;

        assert!(text.contains("CopyUnavailable"), "{text}");
        assert!(text.contains("native\\n# message with | and ```"), "{text}");
        assert!(!text.contains("\n# message"));
        assert_eq!(stub.requests(), 1);
    }

    #[tokio::test]
    async fn context_reports_copies_even_when_the_codebase_summary_is_unavailable() {
        let stub = Stub::serve(vec![
            Reply::Answer {
                status: "400 Bad Request",
                headers: Vec::new(),
                body: r#"{"success":false,"errors":[{"code":"SummaryUnavailable","message":"summary unavailable"}]}"#.into(),
            },
            Reply::ok(COPIES),
        ])
        .await;
        let client = Client::for_test_server(&stub.url).with_codebase("A".into());
        let text = current_context(&client, false, None).await;

        assert!(text.contains("SummaryUnavailable"), "{text}");
        for id in ["canonical", "selected", "older", "case-distinct"] {
            assert!(text.contains(&markdown_cell(id)), "{text}");
        }
        assert_eq!(stub.requests(), 2);
        assert_eq!(
            stub.request_lines(),
            [
                "GET /v1/codebases/A HTTP/1.1",
                "GET /v1/codebases/A/versions HTTP/1.1"
            ]
        );
    }
}
