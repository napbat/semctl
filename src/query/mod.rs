//! Tool bodies. Each function takes the shared [`Client`] + typed args,
//! hits one REST endpoint, and renders the response as the text an MCP
//! host shows the model. Errors are formatted inline (not raised as
//! protocol errors) so the model sees *why* a call failed and can adjust.
//!
//! The code/graph endpoints are codebase-scoped: the MCP server is launched
//! for one codebase (`SEMCTX_CODEBASE`), and these build
//! `/v1/codebases/{id}/…` paths from it. `search` / `list_domains` don't need
//! a codebase.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;

use crate::client::{Client, api};

mod render;

pub use render::render_projects;
use render::{
    hit_location, local_path, render_boundaries, render_compact, render_files, render_hits,
    render_hits_inner, render_job, render_tree, truncation_note,
};

/// Map the user's `--prefer` value to the server's `SearchPreference` enum name
/// (`"Code"` / `"Docs"`). `None` for an unrecognised value — a typo means "no
/// preference" rather than a failed search.
fn normalize_prefer(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "code" => Some("Code"),
        "docs" | "doc" => Some("Docs"),
        _ => None,
    }
}

/// Options that shape a `search` beyond the query — passed straight to the
/// server, which owns the shaping (ranking bias, kind filter, symbol
/// granularity) so every client behaves identically. All-default reproduces a
/// plain search.
#[derive(Debug, Default, Clone)]
pub struct SearchOpts {
    /// Ranking bias (`"code"` / `"docs"`); unrecognised = unbiased.
    pub prefer: Option<String>,
    /// Restrict to these chunk kinds (`function` / `container` / `block`).
    pub kinds: Vec<String>,
    /// Ask the server for full enclosing-symbol bodies, and render them in full.
    pub expand: bool,
}

/// Cross-domain search, scoped to the launched codebase when one is set. The
/// server applies the kind filter, ranking bias, and symbol granularity; the
/// client sends the options, annotates local staleness, and renders.
pub async fn search(
    client: &Client,
    query: &str,
    top_k: u32,
    domains: &[String],
    opts: &SearchOpts,
) -> String {
    let body = api::SearchRequestBody {
        query,
        top_k,
        codebase_id: client.codebase().ok(),
        domains: if domains.is_empty() {
            None
        } else {
            Some(domains.to_vec())
        },
        filters: None,
        kinds: (!opts.kinds.is_empty()).then(|| opts.kinds.clone()),
        prefer: opts
            .prefer
            .as_deref()
            .and_then(normalize_prefer)
            .map(str::to_string),
        granularity: opts.expand.then(|| "Symbol".to_string()),
    };
    let hits = match client
        .post::<_, Vec<api::SearchHit>>("/v1/search", &body)
        .await
    {
        Ok(h) => h,
        Err(e) => return format!("search failed: {e}"),
    };
    if hits.is_empty() {
        return "no results".to_string();
    }

    // Staleness is the one shaping the client owns — only it has the local bytes.
    let stale = staleness(client, &hits).await;
    render_hits_inner(
        &hits,
        "no results",
        client.local_root(),
        true,
        &stale,
        opts.expand,
    )
}

/// The set of hit paths whose local file no longer matches what the index
/// holds — best-effort. Empty (no annotations) when there's no local checkout
/// to compare against, or anything goes wrong: a missing staleness flag must
/// never be mistaken for "fresh", but a spurious one is worse, so we only flag
/// a genuine hash mismatch (or a file that's gone). Only the hits' own paths
/// are hashed, not the whole tree.
async fn staleness(client: &Client, hits: &[api::SearchHit]) -> HashSet<String> {
    let mut stale = HashSet::new();
    let Some(root) = client.local_root() else {
        // Server-pulled codebase with no local bytes — staleness is a
        // local-edit concern, so there's nothing to compare.
        return stale;
    };
    let paths: HashSet<&str> = hits.iter().filter_map(|h| h.path.as_deref()).collect();
    if paths.is_empty() {
        return stale;
    }
    let Ok(catalog) = catalog_hashes(client).await else {
        return stale;
    };
    for rel in paths {
        // Only decide when the catalog has a recorded hash for this path; an
        // unknown path (not yet catalogued) isn't flagged, to avoid noise.
        let Some(Some(indexed)) = catalog.get(rel) else {
            continue;
        };
        if is_stale(indexed, local_blake3(&root.join(rel)).as_deref()) {
            stale.insert(rel.to_string());
        }
    }
    stale
}

/// Whether a hit is stale: its `local` hash differs from the `indexed` one, or
/// the local file is gone/unreadable (`None`) while the index still references
/// it. A matching hash is fresh. Pure — the decision the staleness sweep makes.
fn is_stale(indexed: &str, local: Option<&str>) -> bool {
    match local {
        Some(local) => local != indexed,
        None => true,
    }
}

/// The MCP server pages a normal repo's catalog in a single request; cap the
/// walk so a pathological catalog can't turn one search into many round-trips.
const STALENESS_MAX_PAGES: u32 = 5;

/// Build a `codebase-relative path -> indexed content hash` map from the file
/// catalog. Best-effort and bounded by [`STALENESS_MAX_PAGES`].
async fn catalog_hashes(
    client: &Client,
) -> std::result::Result<HashMap<String, Option<String>>, String> {
    let cb = client.codebase().map_err(|e| e.to_string())?;
    let mut map = HashMap::new();
    let mut page = 0u32;
    loop {
        let p = fetch_files_page(client, cb, page, FILES_PAGE_MAX).await?;
        let total = p.total;
        for f in p.items {
            map.insert(f.path, f.content_hash);
        }
        page += 1;
        if page * FILES_PAGE_MAX >= total || page >= STALENESS_MAX_PAGES {
            break;
        }
    }
    Ok(map)
}

/// Blake3 hex of a local file's UTF-8 content — matching exactly what the sync
/// manifest hashes (see `crate::sync::sync`), so the digest is comparable to
/// the catalog's `content_hash`. `None` if the file can't be read as UTF-8.
fn local_blake3(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(blake3::hash(content.as_bytes()).to_hex().to_string())
}

/// One-shot symbol neighbourhood (#4): the definition of `symbol` plus its
/// direct callers and callees. The server's `/graph/trace` endpoint composes and
/// classifies the neighbourhood next to the graph; the client renders it.
pub async fn trace(client: &Client, symbol: &str, depth: u32) -> String {
    let cb = match client.codebase() {
        Ok(c) => c.to_string(),
        Err(e) => return format!("trace failed: {e}"),
    };
    let url = format!(
        "/v1/codebases/{cb}/graph/trace?symbol={}&depth={depth}",
        urlencode(symbol)
    );
    let t = match client.get::<api::TraceResult>(&url).await {
        Ok(t) => t,
        Err(e) => return format!("trace failed: {e}"),
    };
    if t.definition.is_empty() {
        return near_miss(client, symbol, "definition").await;
    }
    let root = client.local_root();

    let mut out = format!("definition of `{symbol}`:\n");
    out.push_str(&render_hits(&t.definition, "", root, false));

    write!(out, "\ncallers ({}):\n", t.callers.len()).unwrap();
    out.push_str(&render_compact(&t.callers, "  (none)", root));
    write!(out, "\ncallees ({}):\n", t.callees.len()).unwrap();
    out.push_str(&render_compact(&t.callees, "  (none)", root));
    out
}

/// Symbol-graph: definitions of `symbol` in the codebase.
pub async fn find_definition(client: &Client, symbol: &str) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("find_definition failed: {e}"),
    };
    let path = format!(
        "/v1/codebases/{cb}/graph/definitions?symbol={}",
        urlencode(symbol)
    );
    match client.get::<Vec<api::SearchHit>>(&path).await {
        Ok(hits) if hits.is_empty() => near_miss(client, symbol, "definition").await,
        Ok(hits) => render_hits(&hits, "", client.local_root(), false),
        Err(e) => format!("find_definition failed: {e}"),
    }
}

/// Symbol-graph: references to `symbol` in the codebase.
pub async fn find_references(client: &Client, symbol: &str) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("find_references failed: {e}"),
    };
    let path = format!(
        "/v1/codebases/{cb}/graph/references?symbol={}",
        urlencode(symbol)
    );
    match client.get::<Vec<api::SearchHit>>(&path).await {
        Ok(hits) if hits.is_empty() => near_miss(client, symbol, "references").await,
        Ok(hits) => render_hits(&hits, "", client.local_root(), false),
        Err(e) => format!("find_references failed: {e}"),
    }
}

/// Graph: incoming callers of `symbol` — the definitions that call it (the
/// inverse `calls` edge).
pub async fn who_calls(client: &Client, symbol: &str) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("who_calls failed: {e}"),
    };
    let path = format!(
        "/v1/codebases/{cb}/graph/who-calls?symbol={}",
        urlencode(symbol)
    );
    match client.get::<Vec<api::SearchHit>>(&path).await {
        Ok(hits) => render_hits(
            &hits,
            &format!("nothing calls `{symbol}`"),
            client.local_root(),
            false,
        ),
        Err(e) => format!("who_calls failed: {e}"),
    }
}

/// Graph: the types implementing `symbol` (a trait/interface) — the reverse
/// `implements` edge.
pub async fn implementations_of(client: &Client, symbol: &str) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("implementations_of failed: {e}"),
    };
    let path = format!(
        "/v1/codebases/{cb}/graph/implementations?symbol={}",
        urlencode(symbol)
    );
    match client.get::<Vec<api::SearchHit>>(&path).await {
        Ok(hits) => render_hits(
            &hits,
            &format!("no implementations of `{symbol}`"),
            client.local_root(),
            false,
        ),
        Err(e) => format!("implementations_of failed: {e}"),
    }
}

/// Graph: a shortest call chain from `from` to `to` — the chunks along one path
/// of `calls` edges, in order.
pub async fn call_path(client: &Client, from: &str, to: &str) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("call_path failed: {e}"),
    };
    let path = format!(
        "/v1/codebases/{cb}/graph/call-path?from={}&to={}",
        urlencode(from),
        urlencode(to)
    );
    match client.get::<Vec<api::SearchHit>>(&path).await {
        Ok(hits) => render_hits(
            &hits,
            &format!("no call path from `{from}` to `{to}`"),
            client.local_root(),
            false,
        ),
        Err(e) => format!("call_path failed: {e}"),
    }
}

/// Flow: the external boundaries a value entering from `from` flows out to
/// (forward, inter-procedural value flow). Boundaries are the un-indexed callees
/// the corpus crosses.
pub async fn reaches(client: &Client, from: &str) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("reaches failed: {e}"),
    };
    let path = format!(
        "/v1/codebases/{cb}/graph/flow/reaches?from={}",
        urlencode(from)
    );
    match client.get::<Vec<String>>(&path).await {
        Ok(boundaries) => render_boundaries(
            &boundaries,
            &format!("no boundary flow reached from `{from}`"),
        ),
        Err(e) => format!("reaches failed: {e}"),
    }
}

/// Flow: the external boundaries whose entering value reaches `to` (backward,
/// inter-procedural value flow) — the dual of [`reaches`].
pub async fn flows_into(client: &Client, to: &str) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("flows_into failed: {e}"),
    };
    let path = format!("/v1/codebases/{cb}/graph/flow/into?to={}", urlencode(to));
    match client.get::<Vec<String>>(&path).await {
        Ok(boundaries) => render_boundaries(&boundaries, &format!("nothing flows into `{to}`")),
        Err(e) => format!("flows_into failed: {e}"),
    }
}

/// Flow: the function chunks a value flows through from boundary `from` to
/// boundary `to` — the inter-procedural flow witness.
pub async fn flows_between(client: &Client, from: &str, to: &str) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("flows_between failed: {e}"),
    };
    let path = format!(
        "/v1/codebases/{cb}/graph/flow/between?from={}&to={}",
        urlencode(from),
        urlencode(to)
    );
    match client.get::<Vec<api::SearchHit>>(&path).await {
        Ok(hits) => render_hits(
            &hits,
            &format!("no value flow from `{from}` to `{to}`"),
            client.local_root(),
            false,
        ),
        Err(e) => format!("flows_between failed: {e}"),
    }
}

/// Literal / regex code search over the codebase's indexed file content — the
/// exact-match counterpart to semantic `search`. Renders `path:line: text`.
pub async fn grep(
    client: &Client,
    pattern: &str,
    regex: bool,
    ignore_case: bool,
    path: Option<&str>,
    max: u32,
) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("grep failed: {e}"),
    };
    let mut url = format!(
        "/v1/codebases/{cb}/grep?pattern={}&regex={regex}&ignoreCase={ignore_case}&max={max}",
        urlencode(pattern)
    );
    if let Some(p) = path.filter(|p| !p.is_empty()) {
        write!(url, "&path={}", urlencode(p)).unwrap();
    }
    match client.get::<Vec<api::GrepMatch>>(&url).await {
        Ok(matches) if matches.is_empty() => format!("no matches for `{pattern}`"),
        Ok(matches) => {
            let root = client.local_root();
            let mut out = String::new();
            for m in &matches {
                writeln!(
                    out,
                    "{}:{}: {}",
                    local_path(root, &m.path),
                    m.line_number,
                    m.line.trim_end()
                )
                .unwrap();
            }
            writeln!(
                out,
                "({} match{})",
                matches.len(),
                if matches.len() == 1 { "" } else { "es" }
            )
            .unwrap();
            out
        }
        Err(e) => format!("grep failed: {e}"),
    }
}

/// A file's table of contents — every indexed chunk (kind, symbol, line range)
/// in source order. Cheaper than reading the file when you only want its shape.
pub async fn file_outline(client: &Client, path: &str) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("file_outline failed: {e}"),
    };
    let url = format!("/v1/codebases/{cb}/graph/outline?path={}", urlencode(path));
    match client.get::<api::FileOutline>(&url).await {
        Ok(outline) if outline.entries.is_empty() => {
            format!("{path} is indexed but has no chunks")
        }
        Ok(outline) => {
            let local = local_path(client.local_root(), &outline.path);
            let mut out = format!("{local}\n");
            for e in &outline.entries {
                let sym = e.symbol.as_deref().unwrap_or("-");
                writeln!(out, "  {}-{}  {} {sym}", e.line_start, e.line_end, e.kind).unwrap();
            }
            out
        }
        Err(e) => format!("file_outline failed: {e}"),
    }
}

/// Chunks overlapping an inclusive line range in a file — "grow context around
/// a hit". Pass a search/definition hit's line range (widened to taste) to pull
/// the neighbouring chunks in source order.
pub async fn expand_chunk(client: &Client, path: &str, line_start: u32, line_end: u32) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("expand_chunk failed: {e}"),
    };
    let url = format!(
        "/v1/codebases/{cb}/graph/expand?path={}&lineStart={line_start}&lineEnd={line_end}",
        urlencode(path)
    );
    match client.get::<Vec<api::SearchHit>>(&url).await {
        Ok(hits) => render_hits(
            &hits,
            &format!("no indexed chunks overlap {path}:{line_start}-{line_end}"),
            client.local_root(),
            false,
        ),
        Err(e) => format!("expand_chunk failed: {e}"),
    }
}

/// The server caps a page at this many rows (`PaginatedRequest.PageSize`'s
/// `[Range(1, 1000)]`), so it's both the default fetch size and the chunk we
/// walk the catalog in when filtering.
const FILES_PAGE_MAX: u32 = 1000;

/// Safety bound on how many rows the graph-list tools (imports / symbol-edges /
/// external-links) will accumulate across pages before stopping — keeps a
/// pathological codebase from streaming an unbounded dump into the agent. The
/// tool notes when it truncates.
const GRAPH_LIST_CAP: usize = 20_000;

/// Shown when the catalog has zero rows — distinct from "this page is empty"
/// (scrolled past the end). Legitimately empty only while indexing is in flight
/// or nothing has been synced.
const EMPTY_CATALOG: &str = "(no files in the catalog for this codebase — \
    indexing is probably still running (check `sync_status`) or nothing has been synced yet. \
    The catalog and the search index are filled by the same sync, so this should not stay \
    empty while `search_codebase` returns hits — if it does, that's an inconsistency to \
    investigate, not an expected state.)";

/// Fetch one catalog page. `page` is ZERO-based server-side
/// (`Skip = page * pageSize`), so the first page is 0 — `page=1` would skip the
/// first `pageSize` rows.
async fn fetch_files_page(
    client: &Client,
    cb: &str,
    page: u32,
    page_size: u32,
) -> std::result::Result<api::Page<api::CodebaseFile>, String> {
    let path = format!("/v1/codebases/{cb}/files?page={page}&pageSize={page_size}");
    client
        .get_page::<api::CodebaseFile>(&path)
        .await
        .map_err(|e| format!("list_files failed: {e}"))
}

/// Walk every page of a flat paginated list endpoint into one `Vec`. The graph
/// list tools (imports / symbol-edges / external-links) want the whole result,
/// but the endpoints now page, so fetch `pageSize`-row windows until the total
/// is covered (or `cap` rows, a safety bound on pathological codebases). Returns
/// the rows plus the reported `total` so the caller can note truncation.
async fn fetch_all_pages<T: serde::de::DeserializeOwned>(
    client: &Client,
    base_path: &str,
    cap: usize,
) -> std::result::Result<(Vec<T>, u32), String> {
    let sep = if base_path.contains('?') { '&' } else { '?' };
    let mut out: Vec<T> = Vec::new();
    let mut total = 0u32;
    for page in 0.. {
        let path = format!("{base_path}{sep}page={page}&pageSize={FILES_PAGE_MAX}");
        let p = client
            .get_page::<T>(&path)
            .await
            .map_err(|e| e.to_string())?;
        total = p.total;
        let got = p.items.len();
        out.extend(p.items);
        if got == 0 || out.len() >= total as usize || out.len() >= cap {
            break;
        }
    }
    Ok((out, total))
}

/// Catalog listing. With `filter`, narrows to files whose codebase-relative
/// path contains it (case-insensitive), searched across the WHOLE catalog (not
/// just one page). Without a filter, returns one 0-based `page` of `page_size`
/// rows and tells the caller how to fetch the next — so a large catalog can be
/// scrolled rather than truncated.
pub async fn list_files(
    client: &Client,
    filter: Option<&str>,
    page: Option<u32>,
    page_size: Option<u32>,
) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("list_files failed: {e}"),
    };
    let root = client.local_root();

    // Filtered: the filter is the narrowing tool, so it spans the whole catalog
    // — walk every server page (max chunk) and keep the matches. `page` /
    // `page_size` don't apply here; the filter is what scopes the result.
    if let Some(raw) = filter {
        let needle = raw.to_lowercase();
        let mut matches: Vec<api::CodebaseFile> = Vec::new();
        // Set from the first fetch; the loop always runs once before any read.
        let mut total;
        let mut pg = 0u32;
        loop {
            let p = match fetch_files_page(client, cb, pg, FILES_PAGE_MAX).await {
                Ok(p) => p,
                Err(e) => return e,
            };
            total = p.total as usize;
            let got = p.items.len();
            matches.extend(
                p.items
                    .into_iter()
                    .filter(|f| f.path.to_lowercase().contains(&needle)),
            );
            // Stop once we've walked past the last row (or the server ran dry).
            if got == 0 || (pg as usize + 1) * FILES_PAGE_MAX as usize >= total {
                break;
            }
            pg += 1;
        }
        if total == 0 {
            return EMPTY_CATALOG.into();
        }
        if matches.is_empty() {
            return format!(
                "(no indexed files match `{raw}` — searched all {total} catalogued files)"
            );
        }
        matches.sort_by(|a, b| a.path.cmp(&b.path));
        let mut out = render_files(&matches, root);
        writeln!(out, "({} of {total} files match `{raw}`)", matches.len()).unwrap();
        return out;
    }

    // Unfiltered: one 0-based page, with a footer that says where we are and how
    // to scroll. Default page_size is the server max, so a normal repo comes
    // back in a single call; lower it to page through a very large catalog.
    let page_size = page_size.unwrap_or(FILES_PAGE_MAX).clamp(1, FILES_PAGE_MAX);
    let pg = page.unwrap_or(0);
    let p = match fetch_files_page(client, cb, pg, page_size).await {
        Ok(p) => p,
        Err(e) => return e,
    };
    let total = p.total as usize;
    if total == 0 {
        return EMPTY_CATALOG.into();
    }
    let mut files = p.items;
    if files.is_empty() {
        // total > 0 but this page is empty → scrolled past the end.
        let last = (total - 1) / page_size as usize;
        return format!(
            "(page {pg} is past the end — {total} files span pages 0–{last} at pageSize {page_size})"
        );
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let start = pg as usize * page_size as usize; // 0-based index of the first row
    let end = start + files.len(); // exclusive
    let mut out = render_files(&files, root);
    write!(out, "(rows {}–{end} of {total}", start + 1).unwrap();
    if end < total {
        write!(out, "; pass page={} for the next {page_size}", pg + 1).unwrap();
    }
    out.push_str(")\n");
    out
}

/// Status of an index job — the one the MCP server's background sync most
/// recently queued. Lets the agent see whether the codebase is still indexing
/// (so empty `list_files` / `search` results are "not done yet", not "nothing
/// there") without leaving the session.
pub async fn sync_status(client: &Client, codebase_id: &str, job_id: &str) -> String {
    match client
        .get::<api::JobStatus>(&format!("/v1/jobs/{job_id}"))
        .await
    {
        Ok(job) => render_job(codebase_id, job_id, &job),
        Err(e) => format!("sync_status failed: {e}"),
    }
}

/// The detected project graph for the codebase.
pub async fn list_projects(client: &Client) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("list_projects failed: {e}"),
    };
    match client
        .get::<api::ProjectGraph>(&format!("/v1/codebases/{cb}/projects"))
        .await
    {
        Ok(graph) => render_projects(&graph),
        Err(e) => format!("list_projects failed: {e}"),
    }
}

/// File→file import edges across the codebase.
pub async fn imports(client: &Client) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("imports failed: {e}"),
    };
    let path = format!("/v1/codebases/{cb}/graph/imports");
    let (edges, total) =
        match fetch_all_pages::<api::ImportEdge>(client, &path, GRAPH_LIST_CAP).await {
            Ok(v) => v,
            Err(e) => return format!("imports failed: {e}"),
        };
    if edges.is_empty() {
        return "(no import edges)".into();
    }
    let root = client.local_root();
    let mut out = String::new();
    for e in &edges {
        writeln!(
            out,
            "{}  ->  {}  ({})",
            local_path(root, &e.from),
            local_path(root, &e.to),
            e.import_path
        )
        .unwrap();
    }
    truncation_note(&mut out, edges.len(), total);
    out
}

/// Reference→definition symbol bindings across the codebase.
pub async fn symbol_edges(client: &Client) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("symbol_edges failed: {e}"),
    };
    let path = format!("/v1/codebases/{cb}/graph/symbol-edges");
    let (edges, total) =
        match fetch_all_pages::<api::SymbolEdge>(client, &path, GRAPH_LIST_CAP).await {
            Ok(v) => v,
            Err(e) => return format!("symbol_edges failed: {e}"),
        };
    if edges.is_empty() {
        return "(no symbol bindings)".into();
    }
    let root = client.local_root();
    let mut out = String::new();
    for e in &edges {
        writeln!(
            out,
            "{} `{}`  ->  {}  ({})",
            local_path(root, &e.from_file),
            e.name,
            local_path(root, &e.to_file),
            e.moniker
        )
        .unwrap();
    }
    truncation_note(&mut out, edges.len(), total);
    out
}

/// Cross-codebase links — this codebase's imports resolved into other
/// codebases the caller can see.
pub async fn external_links(client: &Client) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("external_links failed: {e}"),
    };
    let path = format!("/v1/codebases/{cb}/graph/external-links");
    let (links, total) =
        match fetch_all_pages::<api::ExternalLink>(client, &path, GRAPH_LIST_CAP).await {
            Ok(v) => v,
            Err(e) => return format!("external_links failed: {e}"),
        };
    if links.is_empty() {
        return "(no cross-codebase links)".into();
    }
    // Only `from_file` is local — `target_file` lives in the linked codebase,
    // so it stays relative (we have no root for it here).
    let root = client.local_root();
    let mut out = String::new();
    for l in &links {
        writeln!(
            out,
            "{} `{}`  ->  {} `{}`  ({} {}/{}, cb={} conf={})",
            local_path(root, &l.from_file),
            l.import_path,
            l.target_file,
            l.target_name,
            l.ecosystem,
            l.target_package,
            l.descriptor,
            l.target_codebase_id,
            l.confidence,
        )
        .unwrap();
    }
    truncation_note(&mut out, links.len(), total);
    out
}

/// The symbol at `path:line[:column]` (1-based). With a column, the identifier
/// under the cursor resolved to its definition; without, the enclosing definition.
pub async fn symbol_at_position(
    client: &Client,
    path: &str,
    line: u32,
    column: Option<u32>,
) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("symbol_at_position failed: {e}"),
    };
    let mut url = format!(
        "/v1/codebases/{cb}/graph/symbol-at-position?path={}&line={line}",
        urlencode(path)
    );
    if let Some(col) = column {
        write!(url, "&column={col}").unwrap();
    }
    let at = match column {
        Some(col) => format!("{path}:{line}:{col}"),
        None => format!("{path}:{line}"),
    };
    match client.get_maybe::<api::SearchHit>(&url).await {
        Ok(Some(hit)) => render_hits(&[hit], "", client.local_root(), false),
        Ok(None) => format!("(no symbol at {at})"),
        Err(e) => format!("symbol_at_position failed: {e}"),
    }
}

/// Resolve many symbols in one call — definitions (or references) for each.
pub async fn batch_lookup(client: &Client, symbols: &[String], references: bool) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("batch_lookup failed: {e}"),
    };
    if symbols.is_empty() {
        return "(no symbols requested)".into();
    }
    let url = format!("/v1/codebases/{cb}/graph/batch");
    let body = serde_json::json!({ "symbols": symbols, "references": references });
    let results = match client.post::<_, Vec<api::SymbolHits>>(&url, &body).await {
        Ok(r) => r,
        Err(e) => return format!("batch_lookup failed: {e}"),
    };
    let kind = if references {
        "references"
    } else {
        "definitions"
    };
    let root = client.local_root();
    let mut out = String::new();
    for r in &results {
        if r.hits.is_empty() {
            writeln!(out, "{} — no {kind}", r.symbol).unwrap();
            continue;
        }
        writeln!(out, "{} ({} {kind}):", r.symbol, r.hits.len()).unwrap();
        for h in &r.hits {
            let loc = h.path.as_deref().unwrap_or("?");
            writeln!(
                out,
                "  {}:{}-{}",
                local_path(root, loc),
                h.line_start.unwrap_or(0),
                h.line_end.unwrap_or(0)
            )
            .unwrap();
        }
    }
    out
}

/// The codebase's files as an indented directory tree.
pub async fn file_tree(client: &Client) -> String {
    let cb = match client.codebase() {
        Ok(c) => c,
        Err(e) => return format!("file_tree failed: {e}"),
    };
    let url = format!("/v1/codebases/{cb}/files/tree");
    match client.get::<Vec<api::FileTreeNode>>(&url).await {
        Ok(nodes) if nodes.is_empty() => "(no files in the catalog for this codebase)".into(),
        Ok(nodes) => {
            let mut out = String::new();
            render_tree(&nodes, 0, &mut out);
            out
        }
        Err(e) => format!("file_tree failed: {e}"),
    }
}

/// List the engine's registered domains + their tag schema.
pub async fn list_domains(client: &Client) -> String {
    match client
        .get::<Vec<api::DomainDescriptor>>("/v1/domains")
        .await
    {
        Ok(domains) if domains.is_empty() => "(no domains registered)".into(),
        Ok(domains) => {
            let mut out = String::new();
            for d in &domains {
                writeln!(out, "{}  ({})", d.id, d.display_name).unwrap();
                for t in &d.tag_schema {
                    writeln!(out, "    {} [{}]: {}", t.name, t.data_type, t.description).unwrap();
                }
            }
            out
        }
        Err(e) => format!("list_domains failed: {e}"),
    }
}

/// On an exact symbol-lookup miss, suggest near matches by name so the caller
/// can recover from a typo / wrong casing / partial name instead of hitting a
/// dead end. Best-effort: runs a search for the symbol and surfaces distinct
/// captured symbol names that look related (case-insensitive substring, either
/// direction). Falls back to the plain miss message when nothing relates.
async fn near_miss(client: &Client, symbol: &str, what: &str) -> String {
    let miss = format!("no {what} found for `{symbol}`");
    let body = api::SearchRequestBody {
        query: symbol,
        top_k: 10,
        codebase_id: client.codebase().ok(),
        domains: None,
        filters: None,
        kinds: None,
        prefer: None,
        granularity: None,
    };
    let Ok(hits) = client
        .post::<_, Vec<api::SearchHit>>("/v1/search", &body)
        .await
    else {
        return miss;
    };
    let needle = symbol.to_lowercase();
    let root = client.local_root();
    let mut seen = std::collections::HashSet::new();
    let mut out = String::new();
    for h in &hits {
        let Some(sym) = h.symbol.as_deref() else {
            continue;
        };
        let lower = sym.to_lowercase();
        // Related = one name contains the other, case-insensitively. Skips the
        // exact name (it had no def, so a same-named hit isn't a "did you mean").
        if sym != symbol
            && (lower.contains(&needle) || needle.contains(&lower))
            && seen.insert(sym.to_string())
        {
            writeln!(out, "  `{sym}`  {}", hit_location(h, root)).unwrap();
        }
        if seen.len() == 5 {
            break;
        }
    }
    if out.is_empty() {
        miss
    } else {
        format!("{miss}. Did you mean:\n{out}")
    }
}

/// Minimal percent-encoding for path segments. Avoids pulling a urlencoding
/// crate for the handful of chars that actually break a URL path segment.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => write!(out, "%{b:02X}").unwrap(),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{is_stale, local_blake3, normalize_prefer};

    #[test]
    fn normalize_prefer_maps_to_server_enum_names() {
        assert_eq!(normalize_prefer("code"), Some("Code"));
        assert_eq!(normalize_prefer("  CODE "), Some("Code"));
        assert_eq!(normalize_prefer("docs"), Some("Docs"));
        assert_eq!(normalize_prefer("doc"), Some("Docs"));
        assert_eq!(normalize_prefer("nonsense"), None);
    }

    #[test]
    fn is_stale_decisions() {
        assert!(!is_stale("abc", Some("abc")), "unchanged → fresh");
        assert!(is_stale("abc", Some("xyz")), "different hash → stale");
        assert!(is_stale("abc", None), "gone/unreadable → stale");
    }

    #[test]
    fn local_blake3_matches_manifest_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.rs");
        std::fs::write(&p, "fn main() {}\n").unwrap();

        // Must equal exactly what `crate::sync::sync` writes to the manifest.
        let want = blake3::hash("fn main() {}\n".as_bytes())
            .to_hex()
            .to_string();
        assert_eq!(local_blake3(&p).as_deref(), Some(want.as_str()));
        assert!(local_blake3(&dir.path().join("missing.rs")).is_none());
    }
}
