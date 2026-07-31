//! Pure result-rendering: format the typed `api::…` responses (and plain
//! strings) the query operations receive into the text an MCP host shows the
//! model. No `Client`, no `async`, no network — just formatting, so this
//! layer is cheap to unit-test in isolation.

use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

use crate::client::api;

use super::GRAPH_LIST_CAP;

/// Absolutize a codebase-relative path against the local checkout `root`,
/// so the host can open it directly. With no known root (canonical /
/// server-pulled codebase) the path is left relative — absolutizing it
/// would fabricate a location that isn't on disk.
pub(super) fn local_path(root: Option<&Path>, rel: &str) -> String {
    match root {
        Some(root) => strip_verbatim_prefix(root.join(rel).to_string_lossy().into_owned()),
        None => rel.to_string(),
    }
}

/// Drop Windows' `\\?\` extended-length ("verbatim") prefix that
/// `fs::canonicalize` stamps onto the checkout root: terminals and editors don't
/// treat a `\\?\C:\…` path as clickable, defeating the point of absolutizing.
/// `\\?\C:\x` -> `C:\x`; `\\?\UNC\srv\share` -> `\\srv\share`. No-op otherwise
/// (and on non-Windows, where the prefix never appears).
pub(super) fn strip_verbatim_prefix(p: String) -> String {
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = p.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        p
    }
}

/// Compact one-line-plus-signature renderer for a borrowed hit list — used by
/// `trace` for the caller/callee groups, where the full snippet block of
/// [`render_hits`] would bury the structure.
pub(super) fn render_compact(
    hits: &[api::SearchHit],
    empty_msg: &str,
    root: Option<&Path>,
) -> String {
    if hits.is_empty() {
        return format!("{empty_msg}\n");
    }
    let mut out = String::new();
    for h in hits {
        writeln!(
            out,
            "  {} {}{}",
            hit_location(h, root),
            hit_symbol(h),
            write_marker(h)
        )
        .unwrap();
    }
    out
}

/// Render a plain list of external-boundary monikers (the `reaches` /
/// `flows_into` value-flow results), one per line.
pub(super) fn render_boundaries(boundaries: &[String], empty_msg: &str) -> String {
    if boundaries.is_empty() {
        return format!("{empty_msg}\n");
    }
    let mut out = String::new();
    for b in boundaries {
        out.push_str("  ");
        out.push_str(b);
        out.push('\n');
    }
    writeln!(
        out,
        "({} boundar{})",
        boundaries.len(),
        if boundaries.len() == 1 { "y" } else { "ies" }
    )
    .unwrap();
    out
}

/// Render a file list (sorted by path), absolutizing each against the local
/// checkout when known.
pub(super) fn render_files(files: &[api::CodebaseFile], root: Option<&Path>) -> String {
    let mut out = String::new();
    for f in files {
        writeln!(out, "{}  ({} bytes)", local_path(root, &f.path), f.size).unwrap();
    }
    out
}

/// Render a job's progress. The phase is derived from timestamps/error rather
/// than the server's status enum, so it's robust to the enum's wire encoding.
pub(super) fn render_job(job_id: &str, j: &api::JobStatus) -> String {
    let phase = if j.error.is_some() {
        "failed"
    } else if j.completed_at.is_some() {
        "done"
    } else if j.started_at.is_some() {
        "running"
    } else {
        "queued"
    };
    let done = j.files_embedded + j.files_deleted + j.files_failed;
    let total = j.files_to_embed + j.files_to_delete;
    let mut out = format!(
        "last sync run {job_id}: {phase}\n  \
             files: {done}/{total} processed (embedded {}, deleted {}, failed {})",
        j.files_embedded, j.files_deleted, j.files_failed,
    );
    if let Some(e) = &j.error {
        write!(out, "\n  error: {e}").unwrap();
    }
    if phase == "done" && total == 0 {
        out.push_str("\n  (up to date — nothing to sync)");
    }
    out
}

/// Render a project graph as readable text — groups (workspaces/solutions),
/// then leaf projects with their file counts.
pub fn render_projects(graph: &api::ProjectGraph) -> String {
    if graph.projects.is_empty() && graph.groups.is_empty() {
        return "(no projects detected — synced files may not include manifests)".into();
    }
    let mut out = String::new();
    for g in &graph.groups {
        writeln!(
            out,
            "[group] {}  ({}, {})  {} member(s)",
            g.name,
            g.kind,
            g.root,
            g.children.len()
        )
        .unwrap();
    }
    for p in &graph.projects {
        writeln!(
            out,
            "{}  ({}, {})  {} file(s)",
            p.name, p.kind, p.root, p.file_count
        )
        .unwrap();
        if let Some(cfg) = &p.config_file {
            writeln!(out, "    {cfg}").unwrap();
        }
    }
    out
}

/// Append a note when the listing was capped below the reported total.
pub(super) fn truncation_note(out: &mut String, shown: usize, total: u32) {
    if shown < total as usize {
        write!(
            out,
            "\n(showing {shown} of {total} — capped at {GRAPH_LIST_CAP}; narrow the codebase to see the rest)\n"
        )
        .unwrap();
    }
}

/// Render a file tree depth-first with two-space indentation; directories get a
/// trailing `/`.
pub(super) fn render_tree(nodes: &[api::FileTreeNode], depth: usize, out: &mut String) {
    for n in nodes {
        let pad = "  ".repeat(depth);
        if n.is_directory {
            writeln!(out, "{pad}{}/", n.name).unwrap();
            if let Some(children) = &n.children {
                render_tree(children, depth + 1, out);
            }
        } else {
            writeln!(out, "{pad}{}", n.name).unwrap();
        }
    }
}

/// Render a hit list as readable text. Each hit is a location line plus
/// a few lines of snippet — enough for the model to decide whether to
/// expand, without flooding the context.
///
/// `show_score` gates the relevance score: it's meaningful for `search`
/// (ranked by hybrid similarity) but a constant 0.000 for the symbol-graph
/// lookups (`find_definition` / `find_references`), where printing it is
/// noise that invites the reader to mistrust a deterministic result.
pub(super) fn render_hits(
    hits: &[api::SearchHit],
    empty_msg: &str,
    root: Option<&Path>,
    show_score: bool,
) -> String {
    render_hits_inner(hits, empty_msg, root, show_score, &HashSet::new(), false)
}

/// As [`render_hits`], but annotates any hit whose codebase-relative path is in
/// `stale` (its local file no longer matches the index), and — when `full_body`
/// — prints the whole snippet instead of the first four lines (for `--expand`,
/// where the server already returned the full enclosing-symbol body). The public
/// [`render_hits`] passes an empty set and `false`; only `search` varies them.
pub(super) fn render_hits_inner(
    hits: &[api::SearchHit],
    empty_msg: &str,
    root: Option<&Path>,
    show_score: bool,
    stale: &HashSet<String>,
    full_body: bool,
) -> String {
    if hits.is_empty() {
        return empty_msg.to_string();
    }
    // Grouping (search only): hits arrive ranked, with a long low-relevance tail.
    // Split the tail off under a separator so strong matches aren't visually equal
    // to the noise — without DROPPING any hit (a relevance floor could silently cut
    // recall). "Weak" is relative to the top hit, adapting to each query's scale.
    let weak_below = show_score
        .then(|| hits.iter().map(|h| h.score).fold(f64::MIN, f64::max) * WEAK_HIT_FRACTION);

    let mut out = String::new();
    let mut separated = false;
    for h in hits {
        if let Some(floor) = weak_below
            && !separated
            && h.score < floor
        {
            out.push_str("--- weaker matches (below ");
            write!(
                out,
                "{:.0}% of top score) ---\n\n",
                WEAK_HIT_FRACTION * 100.0
            )
            .unwrap();
            separated = true;
        }
        let lang = h
            .language
            .as_deref()
            .map(|l| format!(" {l}"))
            .unwrap_or_default();
        let score = if show_score {
            format!("  (score {:.3})", h.score)
        } else {
            String::new()
        };
        let sym = hit_symbol(h);
        // Kind (container/function/block) lets the reader tell a type/def chunk
        // from a free block at a glance. Omitted when the server didn't send one.
        let kind = if h.kind.is_empty() {
            String::new()
        } else {
            format!(" ({})", h.kind)
        };
        let stale_mark = if h.path.as_deref().is_some_and(|p| stale.contains(p)) {
            "  ⚠ stale (edited since indexed)"
        } else {
            ""
        };
        writeln!(
            out,
            "[{}{lang}] {}  {sym}{kind}{}{score}{stale_mark}",
            h.domain_id,
            hit_location(h, root),
            write_marker(h)
        )
        .unwrap();
        // Skip leading blank lines so the declaration/signature leads the snippet
        // rather than whitespace. `--expand` shows the whole body; otherwise the
        // first few lines, enough to judge relevance without flooding context.
        let take = if full_body { usize::MAX } else { 4 };
        for line in h
            .snippet
            .lines()
            .skip_while(|l| l.trim().is_empty())
            .take(take)
        {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// A hit scoring below this fraction of the top hit's score is sorted under the
/// "weaker matches" separator. Relative (not absolute) because raw hybrid
/// scores aren't calibrated across queries.
const WEAK_HIT_FRACTION: f64 = 0.5;

/// The symbol column: the MATCHED symbol, qualified by the declaration that
/// contains it when the server reports one (`dispatch in run_until`). Without
/// the qualifier a references list reads as the same symbol repeated at
/// unrelated lines — indistinguishable from a stale index.
fn hit_symbol(h: &api::SearchHit) -> String {
    let sym = h.symbol.as_deref().unwrap_or("-");
    match h.enclosing_symbol.as_deref() {
        Some(enclosing) => format!("{sym} in {enclosing}"),
        None => sym.to_string(),
    }
}

/// Marks a reference site that WRITES the symbol; empty for a read or for a hit
/// carrying no classification (search / definition).
fn write_marker(h: &api::SearchHit) -> &'static str {
    if h.is_write == Some(true) {
        " (write)"
    } else {
        ""
    }
}

/// `path:line-range` for a hit, absolutized against the local checkout when
/// known. Shared by the hit renderer and the near-miss suggester.
pub(super) fn hit_location(h: &api::SearchHit, root: Option<&Path>) -> String {
    match (&h.path, h.line_start, h.line_end) {
        (Some(p), Some(s), Some(e)) => format!("{}:{s}-{e}", local_path(root, p)),
        (Some(p), Some(s), None) => format!("{}:{s}", local_path(root, p)),
        (Some(p), _, _) => local_path(root, p),
        _ => "(no location)".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{api, render_hits_inner, render_job, strip_verbatim_prefix};

    /// A `SearchHit` with a chosen chunk id + path — for the render test that
    /// keys on those.
    fn node(id: &str, path: &str) -> api::SearchHit {
        api::SearchHit {
            domain_id: "code".into(),
            id: id.into(),
            score: 0.0,
            path: Some(path.into()),
            line_start: Some(1),
            line_end: Some(2),
            focus_line: Some(1),
            language: Some("rust".into()),
            symbol: Some(id.into()),
            enclosing_symbol: None,
            is_write: None,
            kind: "function".into(),
            snippet: String::new(),
        }
    }

    #[test]
    fn render_marks_only_stale_paths() {
        let hits = [node("a", "dirty.rs"), node("b", "clean.rs")];
        let stale: HashSet<String> = ["dirty.rs".to_string()].into_iter().collect();

        let out = render_hits_inner(&hits, "none", None, true, &stale, false);
        let dirty = out.lines().find(|l| l.contains("dirty.rs")).unwrap();
        let clean = out.lines().find(|l| l.contains("clean.rs")).unwrap();

        assert!(dirty.contains("⚠ stale"), "edited file flagged");
        assert!(!clean.contains("⚠ stale"), "untouched file not flagged");
    }

    #[test]
    fn render_qualifies_a_reference_and_marks_a_write() {
        let mut read = node("dispatch", "run.rs");
        read.enclosing_symbol = Some("run_until".into());
        read.is_write = Some(false);
        let mut write = node("dispatch", "run.rs");
        write.enclosing_symbol = Some("reset".into());
        write.is_write = Some(true);

        let out = render_hits_inner(&[read, write], "none", None, false, &HashSet::new(), false);
        let lines: Vec<&str> = out.lines().filter(|l| l.contains("run.rs")).collect();

        assert!(lines[0].contains("dispatch in run_until"), "{out}");
        assert!(!lines[0].contains("(write)"), "a read isn't marked: {out}");
        assert!(lines[1].contains("dispatch in reset"), "{out}");
        assert!(lines[1].contains("(write)"), "{out}");
    }

    #[test]
    fn strips_drive_verbatim_prefix() {
        assert_eq!(
            strip_verbatim_prefix(r"\\?\C:\Users\dev\repo\src\lib.rs".into()),
            r"C:\Users\dev\repo\src\lib.rs"
        );
    }

    #[test]
    fn rewrites_unc_verbatim_prefix() {
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\server\share\file.rs".into()),
            r"\\server\share\file.rs"
        );
    }

    #[test]
    fn leaves_plain_paths_untouched() {
        // Already-clean Windows paths and POSIX paths pass through verbatim.
        assert_eq!(
            strip_verbatim_prefix(r"C:\already\clean.rs".into()),
            r"C:\already\clean.rs"
        );
        assert_eq!(
            strip_verbatim_prefix("/home/dev/repo/src/lib.rs".into()),
            "/home/dev/repo/src/lib.rs"
        );
    }

    #[test]
    fn job_progress_counts_failed_files_as_processed() {
        let job = api::JobStatus {
            files_to_embed: 10,
            files_to_delete: 2,
            files_embedded: 4,
            files_deleted: 2,
            files_failed: 3,
            chunk_count: None,
            error: None,
            started_at: Some("2026-07-14T00:00:00Z".into()),
            completed_at: None,
        };

        let out = render_job("job-1", &job);
        assert!(out.contains("files: 9/12 processed"), "{out}");
    }
}
