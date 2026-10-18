//! Catalog and structural-inspection query handlers.

use std::fmt::Write;

use super::render::{
    local_path, markdown_cell, markdown_code, render_fenced, render_files, render_hits,
    render_projects, render_tree, truncation_note,
};
use super::{Client, ToolError, api, bound_codebase, urlencode};
use crate::engine::CoordinatorStatus;
use crate::mcp::readiness::FirstIndexPhase;
use crate::sync::SyncProgress;

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
) -> anyhow::Result<api::Page<api::CodebaseFile>> {
    let path = format!("/v1/codebases/{cb}/files?page={page}&pageSize={page_size}");
    client.get_page::<api::CodebaseFile>(&path).await
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
) -> anyhow::Result<(Vec<T>, u32)> {
    let sep = if base_path.contains('?') { '&' } else { '?' };
    let mut out: Vec<T> = Vec::new();
    let mut total = 0u32;
    for page in 0.. {
        let path = format!("{base_path}{sep}page={page}&pageSize={FILES_PAGE_MAX}");
        let p = client.get_page::<T>(&path).await?;
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
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "list_files")?;
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
            let p = fetch_files_page(client, cb, pg, FILES_PAGE_MAX)
                .await
                .map_err(|e| ToolError::from_client("list_files", &e))?;
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
            return Ok(EMPTY_CATALOG.into());
        }
        if matches.is_empty() {
            return Ok(format!(
                "(no indexed files match {} — searched all {total} catalogued files)",
                markdown_code(raw)
            ));
        }
        matches.sort_by(|a, b| a.path.cmp(&b.path));
        let mut out = render_files(&matches, root);
        writeln!(
            out,
            "\n({} of {total} files match {})",
            matches.len(),
            markdown_code(raw)
        )
        .expect("writing to a String cannot fail");
        return Ok(out);
    }

    // Unfiltered: one 0-based page, with a footer that says where we are and how
    // to scroll. Default page_size is the server max, so a normal repo comes
    // back in a single call; lower it to page through a very large catalog.
    let page_size = page_size.unwrap_or(FILES_PAGE_MAX).clamp(1, FILES_PAGE_MAX);
    let pg = page.unwrap_or(0);
    let p = fetch_files_page(client, cb, pg, page_size)
        .await
        .map_err(|e| ToolError::from_client("list_files", &e))?;
    let total = p.total as usize;
    if total == 0 {
        return Ok(EMPTY_CATALOG.into());
    }
    let mut files = p.items;
    if files.is_empty() {
        // total > 0 but this page is empty → scrolled past the end.
        let last = (total - 1) / page_size as usize;
        return Ok(format!(
            "(page {pg} is past the end — {total} files span pages 0–{last} at pageSize {page_size})"
        ));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let start = pg as usize * page_size as usize; // 0-based index of the first row
    let end = start + files.len(); // exclusive
    let mut out = render_files(&files, root);
    write!(out, "\n(rows {}–{end} of {total}", start + 1).expect("writing to a String cannot fail");
    if end < total {
        write!(out, "; pass page={} for the next {page_size}", pg + 1)
            .expect("writing to a String cannot fail");
    }
    out.push_str(")\n");
    Ok(out)
}

/// Persisted index totals plus the latest sync run queued by this MCP session.
/// The catalog totals are deliberately independent of the latest job: a no-op
/// sync has a 0-file plan even when the codebase already contains thousands of
/// indexed files.
///
/// The coordinator snapshot supplies local progress and failures. A failed
/// scan or upload must remain visible even when the previous server job passed.
pub async fn sync_status(
    client: &Client,
    checkout: Option<&CoordinatorStatus>,
    presentation: super::QueryPresentation,
) -> Result<super::QueryOutput, ToolError> {
    let codebase_id = bound_codebase(client, "sync_status")?;
    let totals = catalog_totals(client, codebase_id).await;
    let job_id = checkout.and_then(|status| status.last_job_id.as_deref());
    let job = match job_id {
        Some(id) => Some((
            id,
            client
                .get::<api::JobStatus>(&format!("/v1/jobs/{id}"))
                .await,
        )),
        None => None,
    };

    let first_index = checkout.and_then(|status| status.first_index.as_ref());
    let local_error = checkout.and_then(|status| status.last_error.as_deref());
    let job_status = job.as_ref().and_then(|(_, result)| result.as_ref().ok());
    let status_error = job
        .as_ref()
        .and_then(|(_, result)| result.as_ref().err())
        .or(totals.as_ref().err());
    let root = checkout
        .map(|status| status.root.as_path())
        .or(client.local_root());
    // A basename is only a display label for this already-bound checkout, never
    // an identity lookup. The full root and immutable id remain below.
    let label = root
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy());
    let mut out = format!(
        "{} · {}",
        markdown_code(label.as_deref().unwrap_or(codebase_id)),
        sync_summary(checkout, job_status, status_error.is_some()),
    );
    let local_attempt = checkout.is_some_and(|status| {
        status.running
            || status.sync_progress.is_some()
            || matches!(
                status.first_index,
                Some(FirstIndexPhase::Registering | FirstIndexPhase::Syncing)
            )
    });
    if let Some(progress) = checkout.and_then(|status| status.sync_progress.as_ref()) {
        write!(out, " · {}", markdown_code(&progress.to_string()))
            .expect("writing to a String cannot fail");
    } else if !local_attempt && let Some(status) = job_status {
        out.push_str(" · last server job: ");
        render_job_counts(&mut out, status);
    }
    let diagnostic = match first_index {
        Some(FirstIndexPhase::Failed { reason }) => Some(reason.as_str()),
        _ => local_error,
    }
    .or_else(|| {
        if local_attempt {
            None
        } else {
            job_status.and_then(|status| status.error.as_deref())
        }
    });
    if let Some(reason) = diagnostic {
        write!(out, " · {}", diagnostic_preview(reason)).expect("writing to a String cannot fail");
    }
    out.push_str("  \n");
    render_sync_recovery(&mut out, checkout, job_status, local_attempt, status_error);
    out.push_str("\n### Status evidence\n\n");
    writeln!(out, "Codebase: {}  ", markdown_code(codebase_id))
        .expect("writing to a String cannot fail");
    if let Some(root) = root {
        writeln!(
            out,
            "Local root: {}  ",
            markdown_code(&root.display().to_string())
        )
        .expect("writing to a String cannot fail");
    }
    if let Some(phase) = first_index {
        writeln!(out, "{}  ", first_index_line(phase)).expect("writing to a String cannot fail");
    }
    if let Some(progress) = checkout.and_then(|status| status.sync_progress.as_ref()) {
        writeln!(out, "{}  ", markdown_code(&current_sync_line(progress)))
            .expect("writing to a String cannot fail");
    }
    render_sync_state(&mut out, checkout, &totals, job_status);
    render_local_sync_failures(&mut out, checkout);
    render_sync_job(
        &mut out,
        checkout,
        job.as_ref().map(|(id, result)| (*id, result)),
    );
    let display = if presentation == super::QueryPresentation::Inspector {
        super::display::status(
            client,
            checkout,
            &totals,
            job.as_ref().map(|(id, result)| (*id, result)),
        )
    } else {
        None
    };
    Ok(super::QueryOutput::new(out, display))
}

/// Keep corrective actions beside the compact failure preview, not repeated
/// throughout the expanded evidence.
fn render_sync_recovery(
    out: &mut String,
    checkout: Option<&CoordinatorStatus>,
    job_status: Option<&api::JobStatus>,
    local_attempt: bool,
    status_error: Option<&anyhow::Error>,
) {
    let first_failed = checkout
        .is_some_and(|status| matches!(status.first_index, Some(FirstIndexPhase::Failed { .. })));
    let local_failed = checkout.is_some_and(|status| status.last_error.is_some());
    if first_failed {
        out.push_str("Correct the reported failure, then call `index_codebase` for this codebase to retry the failed first index.  \n");
    } else if local_failed {
        if let Some(seconds) = checkout.and_then(|status| status.retry_after_secs) {
            writeln!(
                out,
                "Correct the local failure; next local sync attempt in {seconds} s.  "
            )
            .expect("writing to a String cannot fail");
        } else {
            out.push_str("Correct the local failure; no retry delay reported.  \n");
        }
    }
    if let Some(status) = job_status.filter(|status| job_phase(status) == JobPhase::Failed) {
        out.push_str("Last server job failed");
        if (local_attempt || local_failed || first_failed)
            && let Some(reason) = &status.error
        {
            write!(out, ": {}", diagnostic_preview(reason))
                .expect("writing to a String cannot fail");
        }
        if status.files_failed > 0 {
            write!(out, ": {} failed files", status.files_failed)
                .expect("writing to a String cannot fail");
        }
        out.push_str("; correct the server failure before retrying.  \n");
    }
    if let Some(error) = status_error {
        writeln!(
            out,
            "Status unavailable: {}; resolve access/connectivity and call `sync_status` again.  ",
            diagnostic_preview(&error.to_string()),
        )
        .expect("writing to a String cannot fail");
    }
}

/// Keep the native collapsed preview short; the fenced diagnostic below is raw.
fn diagnostic_preview(reason: &str) -> String {
    let line = reason
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("(empty diagnostic)");
    let mut preview: String = line.chars().take(160).collect();
    if line.chars().nth(160).is_some() {
        preview.push('…');
    }
    markdown_code(&preview)
}

/// Catalog totals are persisted evidence, not a current sync's work counters.
fn render_sync_state(
    out: &mut String,
    checkout: Option<&CoordinatorStatus>,
    totals: &anyhow::Result<(u32, i64)>,
    job: Option<&api::JobStatus>,
) {
    match totals {
        Ok((files, bytes)) => {
            writeln!(
                out,
                "Persisted catalog: {files} files; {} ({bytes} bytes).  ",
                human_bytes(*bytes)
            )
            .expect("writing to a String cannot fail");
        }
        Err(_) => out.push_str("Persisted catalog totals: unknown (request failed).  \n"),
    }
    if let Some(chunks) = job.and_then(|status| status.chunk_count) {
        writeln!(out, "Chunks reported by last server job: {chunks}.  ")
            .expect("writing to a String cannot fail");
    }
    let watch = match checkout.map(|status| &status.watcher) {
        Some(crate::engine::coordinator::WatcherState::Active) => "active".to_string(),
        Some(crate::engine::coordinator::WatcherState::Unavailable(reason)) => {
            format!("not active ({reason}); periodic reconciliation may still run")
        }
        None => "not active (no local checkout selected)".to_string(),
    };
    writeln!(out, "Local checkout watch: {}  ", markdown_code(&watch))
        .expect("writing to a String cannot fail");
    if let Some(status) = checkout {
        writeln!(
            out,
            "Local reconcile: {}; pending triggers: {}{}; leases: {}.  ",
            if status.running || status.sync_progress.is_some() {
                "running"
            } else {
                "idle"
            },
            status.pending_triggers,
            if status.trigger_overflow {
                " (overflowed; changes coalesced)"
            } else {
                ""
            },
            status.leases,
        )
        .expect("writing to a String cannot fail");
        if let Some(seconds) = status.retry_after_secs {
            writeln!(
                out,
                "Retry schedule: next local sync attempt in {seconds} s.  "
            )
            .expect("writing to a String cannot fail");
        }
        if let Some(outcome) = &status.last_outcome {
            out.push_str("\nLast local outcome:\n\n");
            render_fenced(out, outcome, None);
        }
        if let crate::engine::coordinator::WatcherState::Unavailable(reason) = &status.watcher {
            out.push_str("\nWatcher diagnostic:\n\n");
            render_fenced(out, reason, None);
        }
    }
    if let Err(error) = totals {
        out.push_str("\n### Catalog totals unavailable\n\n");
        render_fenced(out, &error.to_string(), None);
        out.push_str("\nResolve the reported access/connectivity error, then call `sync_status` again; unknown totals are not zero.\n");
    }
}

/// Keep complete local diagnostics and the first-index retry gate together,
/// independent of any server job left over from an earlier reconcile.
fn render_local_sync_failures(out: &mut String, checkout: Option<&CoordinatorStatus>) {
    let Some(status) = checkout else { return };
    let first_failed = matches!(status.first_index, Some(FirstIndexPhase::Failed { .. }));
    if let Some(error) = &status.last_error
        && !matches!(&status.first_index, Some(FirstIndexPhase::Failed { reason }) if reason == error)
    {
        out.push_str("\n### Local scan/upload failure\n\n");
        render_fenced(out, error, None);
        out.push_str(
            "\nCorrect the reported local access, authentication, or server connectivity failure. ",
        );
        if !first_failed {
            if status.retry_after_secs.is_some() {
                out.push_str("Retained work retries on the reported schedule.\n");
            } else {
                out.push_str(
                    "No retry delay was reported; inspect a later `sync_status` snapshot.\n",
                );
            }
        }
    }
    if let Some(FirstIndexPhase::Failed { reason }) = &status.first_index {
        out.push_str("\n### First-index failure\n\n");
        render_fenced(out, reason, None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobPhase {
    Pending,
    Running,
    Done,
    Failed,
}

/// Timestamps and failure evidence take precedence over the wire status enum.
fn job_phase(job: &api::JobStatus) -> JobPhase {
    if job.error.is_some() || job.files_failed > 0 {
        JobPhase::Failed
    } else if job.completed_at.is_some() {
        JobPhase::Done
    } else if job.started_at.is_some() {
        JobPhase::Running
    } else {
        JobPhase::Pending
    }
}

fn render_job(job_id: &str, j: &api::JobStatus) -> String {
    let phase = job_phase(j);
    let label = match phase {
        JobPhase::Failed => "**Failed**",
        JobPhase::Done => "**Done**",
        JobPhase::Running => "**Running**",
        JobPhase::Pending => "**Pending**",
    };
    let mut out = format!("{label} · Server job {} · ", markdown_code(job_id));
    render_job_counts(&mut out, j);
    out.push_str("  \n");
    if let Some(started) = &j.started_at {
        writeln!(out, "Started: {}  ", markdown_code(started))
            .expect("writing to a String cannot fail");
    }
    if let Some(completed) = &j.completed_at {
        writeln!(out, "Completed: {}  ", markdown_code(completed))
            .expect("writing to a String cannot fail");
    }
    if let Some(error) = &j.error {
        out.push_str("\nServer diagnostic:\n\n");
        render_fenced(&mut out, error, None);
    }
    if phase == JobPhase::Done && j.files_to_embed == Some(0) && j.files_to_delete == Some(0) {
        out.push_str(
            "\nThis job reported no planned work; this does not establish checkout freshness.\n",
        );
    }
    out
}

/// Named wire counters are evidence, not a derived completion ratio.
fn render_job_counts(out: &mut String, job: &api::JobStatus) {
    for (index, (label, count)) in [
        ("embedded", Some(job.files_embedded)),
        ("deleted", job.files_deleted),
        ("failed", Some(job.files_failed)),
        ("planned embed", job.files_to_embed),
        ("planned delete", job.files_to_delete),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            out.push_str("; ");
        }
        write!(out, "{label} ").expect("writing to a String cannot fail");
        match count {
            Some(value) => write!(out, "{value}").expect("writing to a String cannot fail"),
            None => out.push_str("unknown"),
        }
    }
}

/// A recorded server job may predate the current local attempt. Preserve its
/// units, outcome and diagnostic without treating it as that attempt's result.
fn render_sync_job(
    out: &mut String,
    checkout: Option<&CoordinatorStatus>,
    job: Option<(&str, &anyhow::Result<api::JobStatus>)>,
) {
    match job {
        Some((id, Ok(status))) => {
            out.push_str("\n### Last recorded server job\n\n");
            if checkout.is_some_and(|local| {
                local.running || local.sync_progress.is_some() || local.last_error.is_some()
                    || matches!(local.first_index, Some(FirstIndexPhase::Registering | FirstIndexPhase::Syncing))
            }) {
                out.push_str("This job may precede the current local attempt.\n\n");
            }
            out.push_str(&render_job(id, status));
        }
        Some((id, Err(error))) => {
            writeln!(
                out,
                "\n### Server job status unknown\n\nLast sync run {}: status could not be read.\n",
                markdown_code(id)
            )
            .expect("writing to a String cannot fail");
            render_fenced(out, &error.to_string(), None);
            out.push_str("\nResolve the reported access/connectivity error, then call `sync_status` again; the job outcome remains unknown.\n");
        }
        None if checkout.is_some_and(|status| status.last_error.is_some()) => out.push_str(
            "\nNo server job queued; the local sync failed before it recorded a job.\n",
        ),
        None if checkout.is_some() => out.push_str(
            "\nNo server job queued yet; local scanning/preparation or retained work may still be pending.\n",
        ),
        None => out.push_str(
            "\nNo server job queued by this MCP session; local progress and checkout freshness are unknown.\n",
        ),
    }
}

/// Local failures and active work take precedence over a previously recorded
/// server job. Neither an HTTP success nor catalog totals prove indexing passed.
fn sync_summary(
    checkout: Option<&CoordinatorStatus>,
    job: Option<&api::JobStatus>,
    unavailable: bool,
) -> &'static str {
    let first = checkout.and_then(|status| status.first_index.as_ref());
    if matches!(first, Some(FirstIndexPhase::Failed { .. })) {
        return "**First index failed — retrieval not ready**";
    }
    if checkout.is_some_and(|status| status.last_error.is_some()) {
        return "**Local sync failed — indexed results may be stale**";
    }
    if checkout.is_some_and(|status| status.running || status.sync_progress.is_some()) {
        return "**Local sync running — indexing not yet complete**";
    }
    match first {
        Some(FirstIndexPhase::Registering) => {
            return "**First index registering — retrieval not ready**";
        }
        Some(FirstIndexPhase::Syncing) => {
            return "**First index syncing — waiting for local work; retrieval not ready**";
        }
        _ => {}
    }
    let phase = job.map(job_phase);
    if phase == Some(JobPhase::Failed) {
        return "**Server indexing failed — indexed results may be stale**";
    }
    if matches!(first, Some(FirstIndexPhase::Embedding { .. })) {
        return "**First index embedding — retrieval not ready**";
    }
    if unavailable {
        return "**Status incomplete — server state unknown**";
    }
    if matches!(phase, Some(JobPhase::Pending | JobPhase::Running)) {
        return if phase == Some(JobPhase::Running) {
            "**Server indexing running — not yet complete**"
        } else {
            "**Server indexing queued — not yet complete**"
        };
    }
    if checkout.is_some_and(|status| status.pending_triggers > 0 || status.trigger_overflow) {
        return "**Local sync pending — indexed results may be stale**";
    }
    if matches!(first, Some(FirstIndexPhase::Ready)) {
        return "**Ready** · Retrieval available; checkout freshness not established";
    }
    if phase == Some(JobPhase::Done) {
        return "**Last server job done** · Checkout freshness not established";
    }
    "**Indexing state unknown — no completion reported**"
}

fn first_index_line(phase: &FirstIndexPhase) -> String {
    match phase {
        FirstIndexPhase::Failed { .. } => "First index: failed".into(),
        _ => format!("First index: {}", markdown_code(&phase.to_string())),
    }
}

/// Reuse the engine's milestones and authoritative upload-batch counters.
fn current_sync_line(progress: &SyncProgress) -> String {
    format!("Current sync: {progress}")
}

async fn catalog_totals(client: &Client, codebase_id: &str) -> anyhow::Result<(u32, i64)> {
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
pub async fn list_projects(client: &Client) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "list_projects")?;
    match client
        .get::<api::ProjectGraph>(&format!("/v1/codebases/{cb}/projects"))
        .await
    {
        Ok(graph) => Ok(render_projects(&graph)),
        Err(e) => Err(ToolError::from_client("list_projects", &e)),
    }
}

/// File→file import edges across the codebase.
pub async fn imports(client: &Client) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "imports")?;
    let path = format!("/v1/codebases/{cb}/graph/imports");
    let (edges, total) = fetch_all_pages::<api::ImportEdge>(client, &path, GRAPH_LIST_CAP)
        .await
        .map_err(|e| ToolError::from_client("imports", &e))?;
    if edges.is_empty() {
        return Ok("(no import edges)".into());
    }
    let root = client.local_root();
    let mut out = String::from("| From | To | Import |\n| --- | --- | --- |\n");
    for e in &edges {
        writeln!(
            out,
            "| {} | {} | {} |",
            markdown_cell(&local_path(root, &e.from)),
            markdown_cell(&local_path(root, &e.to)),
            markdown_cell(&e.import_path)
        )
        .expect("writing to a String cannot fail");
    }
    truncation_note(&mut out, edges.len(), total);
    Ok(out)
}

/// Reference→definition symbol bindings across the codebase.
pub async fn symbol_edges(client: &Client) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "symbol_edges")?;
    let path = format!("/v1/codebases/{cb}/graph/symbol-edges");
    let (edges, total) = fetch_all_pages::<api::SymbolEdge>(client, &path, GRAPH_LIST_CAP)
        .await
        .map_err(|e| ToolError::from_client("symbol_edges", &e))?;
    if edges.is_empty() {
        return Ok("(no symbol bindings)".into());
    }
    let root = client.local_root();
    let mut out = String::from("| From | Name | To | Moniker |\n| --- | --- | --- | --- |\n");
    for e in &edges {
        writeln!(
            out,
            "| {} | {} | {} | {} |",
            markdown_cell(&local_path(root, &e.from_file)),
            markdown_cell(&e.name),
            markdown_cell(&local_path(root, &e.to_file)),
            markdown_cell(&e.moniker)
        )
        .expect("writing to a String cannot fail");
    }
    truncation_note(&mut out, edges.len(), total);
    Ok(out)
}

/// Cross-codebase links — this codebase's imports resolved into other
/// codebases the caller can see.
pub async fn external_links(client: &Client) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "external_links")?;
    let path = format!("/v1/codebases/{cb}/graph/external-links");
    let (links, total) = fetch_all_pages::<api::ExternalLink>(client, &path, GRAPH_LIST_CAP)
        .await
        .map_err(|e| ToolError::from_client("external_links", &e))?;
    if links.is_empty() {
        return Ok("(no cross-codebase links)".into());
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
        .expect("writing to a String cannot fail");
    }
    truncation_note(&mut out, links.len(), total);
    let mut rendered = String::new();
    render_fenced(&mut rendered, &out, None);
    Ok(rendered)
}

/// The symbol at `path:line[:column]` (1-based). With a column, the identifier
/// under the cursor resolved to its definition; without, the enclosing definition.
pub async fn symbol_at_position(
    client: &Client,
    path: &str,
    line: u32,
    column: Option<u32>,
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "symbol_at_position")?;
    let mut url = format!(
        "/v1/codebases/{cb}/graph/symbol-at-position?path={}&line={line}",
        urlencode(path)
    );
    if let Some(col) = column {
        write!(url, "&column={col}").expect("writing to a String cannot fail");
    }
    let at = match column {
        Some(col) => format!("{path}:{line}:{col}"),
        None => format!("{path}:{line}"),
    };
    match client.get_maybe::<api::SearchHit>(&url).await {
        Ok(Some(hit)) => Ok(render_hits(&[hit], "", client.local_root(), false)),
        Ok(None) => Ok(format!("(no symbol at {at})")),
        Err(e) => Err(ToolError::from_client("symbol_at_position", &e)),
    }
}

/// Resolve many symbols in one call — definitions (or references) for each.
pub async fn batch_lookup(
    client: &Client,
    symbols: &[String],
    references: bool,
    presentation: super::QueryPresentation,
) -> Result<super::QueryOutput, ToolError> {
    let cb = bound_codebase(client, "batch_lookup")?;
    if symbols.is_empty() {
        return Ok(super::QueryOutput::new(
            "(no symbols requested)".into(),
            if presentation == super::QueryPresentation::Inspector {
                super::display::batch(client, &[])
            } else {
                None
            },
        ));
    }
    let url = format!("/v1/codebases/{cb}/graph/batch");
    let body = serde_json::json!({ "symbols": symbols, "references": references });
    let results = client
        .post_read::<_, Vec<api::SymbolHits>>(&url, &body)
        .await
        .map_err(|e| ToolError::from_client("batch_lookup", &e))?;
    let (kind, column) = if references {
        ("references", "Reference location")
    } else {
        ("definitions", "Definition location")
    };
    let (matches, unmatched) = results.iter().fold((0, 0), |(matches, unmatched), result| {
        (
            matches + result.hits.len(),
            unmatched + usize::from(result.hits.is_empty()),
        )
    });
    let root = client.local_root();
    let mut out = format!(
        "**{} symbols · {matches} {kind} · {unmatched} unmatched**\n\n\
         | Symbol | {column} |\n| --- | --- |\n",
        results.len(),
    );
    for r in &results {
        let symbol = markdown_cell(&r.symbol);
        if r.hits.is_empty() {
            writeln!(out, "| {symbol} | No {kind} |").expect("writing to a String cannot fail");
            continue;
        }
        for h in &r.hits {
            let loc = h.path.as_deref().unwrap_or("?");
            writeln!(
                out,
                "| {symbol} | {} |",
                markdown_cell(&format!(
                    "{}:{}-{}",
                    local_path(root, loc),
                    h.line_start.unwrap_or(0),
                    h.line_end.unwrap_or(0)
                ))
            )
            .expect("writing to a String cannot fail");
        }
    }
    let display = if presentation == super::QueryPresentation::Inspector {
        super::display::batch(client, &results)
    } else {
        None
    };
    Ok(super::QueryOutput::new(out, display))
}

/// The codebase's files as an indented directory tree.
pub async fn file_tree(client: &Client) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "file_tree")?;
    let url = format!("/v1/codebases/{cb}/files/tree");
    match client.get::<Vec<api::FileTreeNode>>(&url).await {
        Ok(nodes) if nodes.is_empty() => Ok("(no files in the catalog for this codebase)".into()),
        Ok(nodes) => {
            let mut out = String::new();
            render_tree(&nodes, 0, &mut out);
            let mut rendered = String::new();
            render_fenced(&mut rendered, &out, None);
            Ok(rendered)
        }
        Err(e) => Err(ToolError::from_client("file_tree", &e)),
    }
}

/// List the engine's registered domains + their tag schema.
pub async fn list_domains(client: &Client) -> Result<String, ToolError> {
    match client
        .get::<Vec<api::DomainDescriptor>>("/v1/domains")
        .await
    {
        Ok(domains) if domains.is_empty() => Ok("(no domains registered)".into()),
        Ok(domains) => {
            let mut out = String::new();
            for d in &domains {
                writeln!(out, "{}  ({})", d.id, d.display_name)
                    .expect("writing to a String cannot fail");
                for t in &d.tag_schema {
                    writeln!(out, "    {} [{}]: {}", t.name, t.data_type, t.description)
                        .expect("writing to a String cannot fail");
                }
            }
            let mut rendered = String::new();
            render_fenced(&mut rendered, &out, None);
            Ok(rendered)
        }
        Err(e) => Err(ToolError::from_client("list_domains", &e)),
    }
}

#[cfg(test)]
mod tests;
