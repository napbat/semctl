//! Catalog and structural-inspection query handlers.

use std::fmt::Write;

use super::render::{
    local_path, render_files, render_hits, render_job, render_projects, render_tree,
    truncation_note,
};
use super::{Client, api, urlencode};

pub(super) const FILES_PAGE_MAX: u32 = 1000;

/// Safety bound on how many rows the graph-list tools (imports / symbol-edges /
/// external-links) will accumulate across pages before stopping — keeps a
/// pathological codebase from streaming an unbounded dump into the agent. The
/// tool notes when it truncates.
pub(super) const GRAPH_LIST_CAP: usize = 20_000;

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
pub(super) async fn fetch_files_page(
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

/// Persisted index totals plus the latest sync run queued by this MCP session.
/// The catalog totals are deliberately independent of the latest job: a no-op
/// sync has a 0-file plan even when the codebase already contains thousands of
/// indexed files.
pub async fn sync_status(
    client: &Client,
    job_id: Option<&str>,
    local_watch_active: bool,
) -> String {
    let codebase_id = match client.codebase() {
        Ok(id) => id,
        Err(e) => return format!("sync_status failed: {e}"),
    };
    let totals = catalog_totals(client, codebase_id).await;
    let job = match job_id {
        Some(id) => Some((
            id,
            client
                .get::<api::JobStatus>(&format!("/v1/jobs/{id}"))
                .await,
        )),
        None => None,
    };

    let watch = if local_watch_active {
        "active"
    } else {
        "not active (no local checkout selected)"
    };
    let mut out =
        format!("codebase {codebase_id}\nlocal checkout watch: {watch}\ntotal indexed state:");
    match totals {
        Ok((files, bytes)) => {
            write!(
                out,
                "\n  files: {files}\n  source bytes: {} ({bytes} bytes)",
                human_bytes(bytes)
            )
            .unwrap();
        }
        Err(e) => write!(out, "\n  catalog totals unavailable: {e}").unwrap(),
    }
    if let Some((_, Ok(status))) = &job
        && let Some(chunks) = status.chunk_count
    {
        write!(out, "\n  chunks: {chunks} (post-sync total)").unwrap();
    }

    match job {
        Some((id, Ok(status))) => {
            out.push_str("\n\n");
            out.push_str(&render_job(id, &status));
        }
        Some((id, Err(e))) => {
            write!(out, "\n\nlast sync run {id}: status unavailable — {e}").unwrap();
        }
        None if local_watch_active => out.push_str(
            "\n\nlast sync run: no server job queued yet; the active local watcher may still \
             be scanning/preparing its initial sync. The totals above are the persisted server \
             state.",
        ),
        None => out.push_str(
            "\n\nlast sync run: none queued by this MCP session; the totals above are the \
             persisted server state.",
        ),
    }
    out
}

async fn catalog_totals(
    client: &Client,
    codebase_id: &str,
) -> std::result::Result<(u32, i64), String> {
    let mut page = 0u32;
    let mut bytes = 0i64;
    loop {
        let p = fetch_files_page(client, codebase_id, page, FILES_PAGE_MAX).await?;
        let got = p.items.len();
        let files = p.total;
        bytes = p
            .items
            .iter()
            .fold(bytes, |sum, f| sum.saturating_add(f.size.max(0)));
        page += 1;
        if got == 0 || page.saturating_mul(FILES_PAGE_MAX) >= files {
            return Ok((files, bytes));
        }
    }
}

pub(super) fn human_bytes(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let bytes = u64::try_from(bytes).unwrap_or(0);
    let mut unit = 0;
    let mut divisor = 1u64;
    while bytes / divisor >= 1024 && unit + 1 < UNITS.len() {
        divisor *= 1024;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        let mut whole = bytes / divisor;
        let mut tenth = ((bytes % divisor) * 10 + divisor / 2) / divisor;
        if tenth == 10 {
            whole += 1;
            tenth = 0;
        }
        format!("{whole}.{tenth} {}", UNITS[unit])
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
