//! Pure result-rendering: format the typed `api::…` responses (and plain
//! strings) the query operations receive into the text an MCP host shows the
//! model. No `Client`, no `async`, no network — just formatting, so this
//! layer is cheap to unit-test in isolation.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

use crate::client::api;

use super::inspection::GRAPH_LIST_CAP;

/// Local paths are valid only for hits from the selected checkout.
#[derive(Clone, Copy)]
pub(super) struct HitContext<'a> {
    root: Option<&'a Path>,
    codebase_id: Option<&'a str>,
    allow_unidentified: bool,
}

impl<'a> HitContext<'a> {
    fn local(root: Option<&'a Path>) -> Self {
        Self {
            root,
            codebase_id: None,
            allow_unidentified: true,
        }
    }

    pub(super) fn search(
        root: Option<&'a Path>,
        codebase_id: Option<&'a str>,
        scoped: bool,
    ) -> Self {
        Self {
            root,
            codebase_id,
            allow_unidentified: scoped,
        }
    }

    pub(super) fn root_for(self, hit: &api::SearchHit) -> Option<&'a Path> {
        match (hit.codebase_id.as_deref(), self.codebase_id) {
            (Some(hit_id), Some(selected)) if hit_id == selected => self.root,
            (None, _) | (Some(_), None) if self.allow_unidentified => self.root,
            _ => None,
        }
    }
}

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
        .expect("writing to a String cannot fail");
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
    .expect("writing to a String cannot fail");
    out
}

/// Render a file list (sorted by path), absolutizing each against the local
/// checkout when known.
pub(super) fn render_files(files: &[api::CodebaseFile], root: Option<&Path>) -> String {
    let mut out = String::new();
    for f in files {
        writeln!(out, "{}  ({} bytes)", local_path(root, &f.path), f.size)
            .expect("writing to a String cannot fail");
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
        write!(out, "\n  error: {e}").expect("writing to a String cannot fail");
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
        .expect("writing to a String cannot fail");
    }
    for p in &graph.projects {
        writeln!(
            out,
            "{}  ({}, {})  {} file(s)",
            p.name, p.kind, p.root, p.file_count
        )
        .expect("writing to a String cannot fail");
        if let Some(cfg) = &p.config_file {
            writeln!(out, "    {cfg}").expect("writing to a String cannot fail");
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
        .expect("writing to a String cannot fail");
    }
}

/// Render a file tree depth-first with two-space indentation; directories get a
/// trailing `/`.
pub(super) fn render_tree(nodes: &[api::FileTreeNode], depth: usize, out: &mut String) {
    for n in nodes {
        let pad = "  ".repeat(depth);
        if n.is_directory {
            writeln!(out, "{pad}{}/", n.name).expect("writing to a String cannot fail");
            if let Some(children) = &n.children {
                render_tree(children, depth + 1, out);
            }
        } else {
            writeln!(out, "{pad}{}", n.name).expect("writing to a String cannot fail");
        }
    }
}

/// Render a hit list as Markdown. The count and first location lead the output.
/// Each hit has a heading, metadata, and a fenced snippet.
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
    render_hits_inner(
        hits,
        empty_msg,
        HitContext::local(root),
        show_score,
        &HashSet::new(),
        false,
    )
}

/// As [`render_hits`], but annotates any hit whose codebase-relative path is in
/// `stale` (its local file no longer matches the index), and — when `full_body`
/// — prints the whole snippet instead of the first four lines (for `--expand`,
/// where the server already returned the full enclosing-symbol body). The public
/// [`render_hits`] passes an empty set and `false`; only `search` varies them.
pub(super) fn render_hits_inner(
    hits: &[api::SearchHit],
    empty_msg: &str,
    context: HitContext<'_>,
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

    let mut out = format!(
        "**{} {}**\n\n",
        hits.len(),
        if hits.len() == 1 { "match" } else { "matches" }
    );
    let mut separated = false;
    for h in hits {
        let root = context.root_for(h);
        if let Some(floor) = weak_below
            && !separated
            && h.score < floor
        {
            out.push_str("### Weaker matches (below ");
            write!(out, "{:.0}% of top score)\n\n", WEAK_HIT_FRACTION * 100.0)
                .expect("writing to a String cannot fail");
            separated = true;
        }
        let marker = write_marker(h);
        let classification = if marker.is_empty() {
            String::new()
        } else {
            format!(" {}", markdown_code(marker.trim()))
        };
        writeln!(
            out,
            "### {} — {}{classification}",
            markdown_code(&hit_location(h, root)),
            markdown_code(&hit_symbol(h))
        )
        .expect("writing to a String cannot fail");
        if root.is_some() && h.path.as_deref().is_some_and(|p| stale.contains(p)) {
            // Put this warning before metadata so collapsed host views show it.
            out.push_str("⚠ stale (edited since indexed)\n");
        }
        render_hit_metadata(&mut out, h, show_score);
        // The preview starts at the first nonblank line. Expanded bodies retain
        // all source bytes, including leading blank lines and indentation.
        let snippet = if full_body {
            Cow::Borrowed(h.snippet.as_str())
        } else {
            Cow::Owned(
                h.snippet
                    .lines()
                    .skip_while(|line| line.trim().is_empty())
                    .take(4)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        };
        if !snippet.is_empty() {
            let fence = "`".repeat(longest_backtick_run(&snippet).saturating_add(1).max(3));
            let language = fence_language(h.language.as_deref());
            write!(out, "\n{fence}{language}\n{snippet}").expect("writing to a String cannot fail");
            if !snippet.ends_with('\n') {
                out.push('\n');
            }
            writeln!(out, "{fence}").expect("writing to a String cannot fail");
        }
        out.push('\n');
    }
    out
}

fn render_hit_metadata(out: &mut String, hit: &api::SearchHit, show_score: bool) {
    write!(out, "Domain: {}", markdown_code(&hit.domain_id))
        .expect("writing to a String cannot fail");
    if let Some(language) = &hit.language {
        write!(out, " · Language: {}", markdown_code(language))
            .expect("writing to a String cannot fail");
    }
    if !hit.kind.is_empty() {
        write!(out, " · Kind: {}", markdown_code(&hit.kind))
            .expect("writing to a String cannot fail");
    }
    if show_score {
        write!(out, " · Score: {:.3}", hit.score).expect("writing to a String cannot fail");
    }
    if let Some(codebase) = &hit.codebase_id {
        write!(out, " · Codebase: {}", markdown_code(codebase))
            .expect("writing to a String cannot fail");
    }
    out.push('\n');
}

/// Keep untrusted values on one line and outside Markdown syntax.
fn markdown_code(value: &str) -> String {
    if value.is_empty() {
        return "(empty)".into();
    }
    let mut text = String::new();
    for character in value.chars() {
        if character.is_control() {
            text.extend(character.escape_default());
        } else {
            text.push(character);
        }
    }
    let fence = "`".repeat(longest_backtick_run(&text).saturating_add(1));
    // Padding separates delimiter backticks and preserves surrounding spaces.
    // CommonMark removes one padding space at each end of a code span.
    let needs_padding = text.starts_with('`')
        || text.ends_with('`')
        || (text.starts_with(' ') && text.ends_with(' ') && !text.trim().is_empty());
    let padding = if needs_padding { " " } else { "" };
    format!("{fence}{padding}{text}{padding}{fence}")
}

fn longest_backtick_run(value: &str) -> usize {
    value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}

/// A fence language must be a single token with no Markdown delimiters.
fn fence_language(language: Option<&str>) -> String {
    language
        .filter(|value| {
            value.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '_' | '-' | '+' | '#' | '.')
            })
        })
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
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
fn write_marker(h: &api::SearchHit) -> String {
    let mut parts = Vec::new();
    if let Some(is_write) = h.is_write {
        parts.push(if is_write {
            "write".to_string()
        } else {
            "read".to_string()
        });
    }
    if let Some(namespace) = &h.reference_namespace {
        parts.push(namespace.to_ascii_lowercase());
    }
    if let Some(kind) = &h.reference_kind {
        parts.push(kind.clone());
    }
    if let Some(target) = &h.qualified_symbol {
        parts.push(format!("-> {target}"));
    } else if let Some(target) = &h.external_target {
        parts.push(format!("-> {target}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join("; "))
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

    use super::{HitContext, api, render_hits_inner, render_job, strip_verbatim_prefix};

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
            byte_start: Some(0),
            byte_end: Some(1),
            focus_line: Some(1),
            focus_byte: Some(0),
            snippet_line_start: Some(1),
            snippet_line_end: Some(2),
            snippet_byte_start: Some(0),
            snippet_byte_end: Some(1),
            language: Some("rust".into()),
            symbol: Some(id.into()),
            qualified_symbol: None,
            enclosing_symbol: None,
            is_write: None,
            reference_namespace: None,
            reference_kind: None,
            external_target: None,
            codebase_id: None,
            kind: "function".into(),
            snippet: String::new(),
        }
    }

    #[test]
    fn render_marks_only_stale_paths() {
        let hits = [node("a", "dirty.rs"), node("b", "clean.rs")];
        let stale: HashSet<String> = ["dirty.rs".to_string()].into_iter().collect();

        let out = render_hits_inner(
            &hits,
            "none",
            HitContext::local(Some(std::path::Path::new("/repo"))),
            true,
            &stale,
            false,
        );
        let dirty = out
            .split("\n### ")
            .find(|hit| hit.contains("dirty.rs"))
            .unwrap();
        let clean = out
            .split("\n### ")
            .find(|hit| hit.contains("clean.rs"))
            .unwrap();

        assert!(dirty.contains("⚠ stale"), "edited file flagged");
        assert!(!clean.contains("⚠ stale"), "untouched file not flagged");
    }

    #[test]
    fn search_locations_keep_codebases_with_the_same_path_separate() {
        let root = std::path::Path::new("checkout-a");
        let mut local = node("local", "src/lib.rs");
        local.codebase_id = Some("a".into());
        let mut remote = node("remote", "src/lib.rs");
        remote.codebase_id = Some("b".into());
        let unidentified = node("unidentified", "src/lib.rs");
        let context = HitContext::search(Some(root), Some("a"), false);
        let stale = HashSet::from(["src/lib.rs".to_string()]);
        let output = render_hits_inner(
            &[local, remote, unidentified],
            "none",
            context,
            true,
            &stale,
            false,
        );
        let sections: Vec<_> = output.split("\n### ").skip(1).collect();
        assert!(sections[0].contains("Codebase: `a`"));
        assert!(sections[0].contains(&root.join("src/lib.rs").display().to_string()));
        assert!(sections[0].contains("stale"));
        assert!(sections[1].starts_with("`src/lib.rs:1-2`"));
        assert!(sections[1].contains("Codebase: `b`"));
        assert!(!sections[1].contains("checkout-a"));
        assert!(!sections[1].contains("stale"));
        assert!(!sections[2].contains("checkout-a"));
        assert!(!sections[2].contains("stale"));
        assert!(!sections[2].contains("Codebase:"));
    }

    #[test]
    fn legacy_search_hits_use_local_paths_only_for_a_scoped_request() {
        let root = std::path::Path::new("checkout");
        let hit = node("legacy", "source.rs");
        let scoped = HitContext::search(Some(root), Some("a"), true);
        let broad = HitContext::search(Some(root), Some("a"), false);
        assert_eq!(scoped.root_for(&hit), Some(root));
        assert_eq!(broad.root_for(&hit), None);
    }

    #[test]
    fn render_qualifies_a_reference_and_marks_a_write() {
        let mut read = node("dispatch", "run.rs");
        read.enclosing_symbol = Some("run_until".into());
        read.is_write = Some(false);
        let mut write = node("dispatch", "run.rs");
        write.enclosing_symbol = Some("reset".into());
        write.is_write = Some(true);
        write.reference_namespace = Some("Value".into());
        write.reference_kind = Some("assignment".into());
        write.qualified_symbol = Some("crate::dispatch".into());

        let out = render_hits_inner(
            &[read, write],
            "none",
            HitContext::local(None),
            false,
            &HashSet::new(),
            false,
        );
        let lines: Vec<&str> = out.lines().filter(|l| l.contains("run.rs")).collect();

        assert!(lines[0].contains("dispatch in run_until"), "{out}");
        assert!(lines[0].contains("(read)"), "{out}");
        assert!(lines[1].contains("dispatch in reset"), "{out}");
        assert!(
            lines[1].contains("(write; value; assignment; -> crate::dispatch)"),
            "{out}"
        );
        assert!(
            !out.contains("Score:"),
            "graph hits have no relevance score: {out}"
        );
    }

    #[test]
    fn render_count_location_and_stale_warning_lead_the_output() {
        let hits = [node("first", "dirty.rs"), node("second", "clean.rs")];
        let output = render_hits_inner(
            &hits,
            "none",
            HitContext::local(Some(std::path::Path::new("/repo"))),
            true,
            &HashSet::from(["dirty.rs".to_string()]),
            false,
        );
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines[0], "**2 matches**");
        assert_eq!(lines[1], "");
        assert!(lines[2].starts_with("### `"), "{output}");
        assert!(lines[2].contains("dirty.rs:1-2` — `first`"), "{output}");
        assert_eq!(lines[3], "⚠ stale (edited since indexed)");
        assert_eq!(
            lines[4],
            "Domain: `code` · Language: `rust` · Kind: `function` · Score: 0.000"
        );
    }

    #[test]
    fn render_fences_preserve_snippet_markdown_and_preview_line_selection() {
        let mut hit = node("snippet", "source.rs");
        hit.snippet =
            "\n  \nfn example() {\n    ```rust\n    ~~~\n    **text** <tag>  \n    omitted\n}"
                .into();
        let output = render_hits_inner(
            &[hit],
            "none",
            HitContext::local(None),
            false,
            &HashSet::new(),
            false,
        );
        assert!(output.starts_with("**1 match**\n"), "{output}");
        assert!(
            output.contains(
                "\n````rust\nfn example() {\n    ```rust\n    ~~~\n    **text** <tag>  \n````\n"
            ),
            "{output}"
        );
        assert!(!output.contains("omitted"), "{output}");
    }

    #[test]
    fn render_expanded_body_preserves_all_source_bytes() {
        let mut hit = node("expanded", "source.rs");
        let source = "\r\n  fn example() {\r\n    one\r\n    two\r\n    three\r\n    four\r\n    `````\r\n  }\r\n";
        hit.snippet = source.into();
        let output = render_hits_inner(
            &[hit],
            "none",
            HitContext::local(None),
            false,
            &HashSet::new(),
            true,
        );
        assert!(
            output.contains(&format!("\n``````rust\n{source}``````\n")),
            "{output}"
        );
    }

    #[test]
    fn render_untrusted_values_cannot_open_markdown_blocks() {
        let mut hit = node("`symbol`\n<!--", "`source`\n```\n.rs");
        hit.language = Some("rust\n```".into());
        hit.domain_id = "<details>\n```".into();
        hit.codebase_id = Some("a`\r\n# hidden".into());
        hit.snippet = "let value = 1;".into();
        let output = render_hits_inner(
            &[hit],
            "none",
            HitContext::local(None),
            true,
            &HashSet::new(),
            false,
        );
        let lines: Vec<_> = output.lines().collect();
        assert!(
            lines[2].contains("```` `source`\\n```\\n.rs:1-2 ````"),
            "{output}"
        );
        assert!(lines[2].contains("`` `symbol`\\n<!-- ``"), "{output}");
        assert!(
            lines[3].contains("Codebase: ``a`\\r\\n# hidden``"),
            "{output}"
        );
        assert!(output.contains("\n```\nlet value = 1;\n```\n"), "{output}");
        assert_eq!(output.matches("\n### ").count(), 1, "{output}");
    }

    #[test]
    fn render_keeps_weak_matches_in_one_group() {
        let mut hits = [
            node("strong", "a.rs"),
            node("weak", "b.rs"),
            node("weaker", "c.rs"),
        ];
        hits[0].score = 1.0;
        hits[1].score = 0.4;
        hits[2].score = 0.2;
        let output = render_hits_inner(
            &hits,
            "none",
            HitContext::local(None),
            true,
            &HashSet::new(),
            false,
        );
        assert!(output.starts_with("**3 matches**\n"), "{output}");
        assert_eq!(
            output
                .matches("### Weaker matches (below 50% of top score)")
                .count(),
            1
        );
        assert!(output.find("`strong`").unwrap() < output.find("### Weaker").unwrap());
        assert!(output.find("### Weaker").unwrap() < output.find("`weak`").unwrap());
        assert!(output.contains("`weaker`"), "{output}");
    }

    #[test]
    fn render_empty_results_preserves_the_exact_message() {
        for message in ["no results", "nothing calls `run`", ""] {
            assert_eq!(
                render_hits_inner(
                    &[],
                    message,
                    HitContext::local(None),
                    true,
                    &HashSet::new(),
                    false,
                ),
                message
            );
        }
    }

    #[test]
    fn render_missing_and_empty_values_remain_visible() {
        let mut hit = node("", "source.rs");
        hit.path = None;
        hit.domain_id.clear();
        hit.language = None;
        hit.kind.clear();
        let output = render_hits_inner(
            &[hit],
            "none",
            HitContext::local(None),
            false,
            &HashSet::new(),
            false,
        );
        assert_eq!(
            output,
            "**1 match**\n\n### `(no location)` — (empty)\nDomain: (empty)\n\n"
        );
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
