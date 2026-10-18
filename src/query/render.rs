//! Pure result-rendering: format the typed `api::…` responses (and plain
//! strings) the query operations receive into the text an MCP host shows the
//! model. No `Client`, no `async`, no network — just formatting, so this
//! layer is cheap to unit-test in isolation.

use std::borrow::Cow;
use std::fmt::Write;
use std::path::Path;

use crate::client::api;

use super::inspection::GRAPH_LIST_CAP;
use super::source::HitSource;

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
    let mut out = String::from("| Location | Symbol | Access |\n| --- | --- | --- |\n");
    for h in hits {
        let marker = write_marker(h);
        writeln!(
            out,
            "| {} | {} | {} |",
            markdown_cell(&hit_location(h, root)),
            markdown_cell(&hit_symbol(h)),
            markdown_cell(if marker.is_empty() {
                "-"
            } else {
                marker.trim()
            })
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
        writeln!(out, "- {}", markdown_code(b)).expect("writing to a String cannot fail");
    }
    writeln!(
        out,
        "\n({} boundar{})",
        boundaries.len(),
        if boundaries.len() == 1 { "y" } else { "ies" }
    )
    .expect("writing to a String cannot fail");
    out
}

/// Render a file list (sorted by path), absolutizing each against the local
/// checkout when known.
pub(super) fn render_files(files: &[api::CodebaseFile], root: Option<&Path>) -> String {
    let mut out = String::from("| Path | Bytes |\n| --- | ---: |\n");
    for f in files {
        writeln!(
            out,
            "| {} | {} |",
            markdown_cell(&local_path(root, &f.path)),
            f.size
        )
        .expect("writing to a String cannot fail");
    }
    out
}

/// Keep outline depth explicit and declaration bodies separate from metadata.
pub(super) fn render_outline(outline: &api::FileOutline, root: Option<&Path>) -> String {
    let location = local_path(root, &outline.path);
    let mut out = format!(
        "{} · {} outline entries\n\n| Depth | Lines | Kind | Symbol |\n| ---: | --- | --- | --- |\n",
        markdown_code(&location),
        outline.entries.len(),
    );
    for entry in &outline.entries {
        let symbol = entry
            .qualified_symbol
            .as_deref()
            .or(entry.symbol.as_deref())
            .unwrap_or("-");
        writeln!(
            out,
            "| {} | {}-{} | {} | {} |",
            entry.depth,
            entry.line_start,
            entry.line_end,
            markdown_cell(entry.symbol_kind.as_deref().unwrap_or(&entry.kind)),
            markdown_cell(symbol)
        )
        .expect("writing to a String cannot fail");
    }
    out.push('\n');
    for entry in &outline.entries {
        if let Some(body) = &entry.body {
            writeln!(
                out,
                "### {}\n",
                markdown_code(&format!(
                    "{location}:{}-{}",
                    entry.line_start, entry.line_end
                ))
            )
            .expect("writing to a String cannot fail");
            render_fenced(&mut out, body, source_language_hint(&outline.path));
            out.push('\n');
        }
    }
    out
}

/// Render project fields as Markdown tables. Tables also remain readable in
/// ordinary terminal output.
pub fn render_projects(graph: &api::ProjectGraph) -> String {
    if graph.projects.is_empty() && graph.groups.is_empty() {
        return "(no projects detected — synced files may not include manifests)".into();
    }
    let mut out = String::new();
    if !graph.groups.is_empty() {
        out.push_str("| Group | Kind | Root | Members |\n| --- | --- | --- | ---: |\n");
        for g in &graph.groups {
            writeln!(
                out,
                "| {} | {} | {} | {} |",
                markdown_cell(&g.name),
                markdown_cell(&g.kind),
                markdown_cell(&g.root),
                g.children.len()
            )
            .expect("writing to a String cannot fail");
        }
        out.push('\n');
    }
    if !graph.projects.is_empty() {
        out.push_str(
            "| Project | Kind | Root | Files | Config |\n| --- | --- | --- | ---: | --- |\n",
        );
        for p in &graph.projects {
            writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                markdown_cell(&p.name),
                markdown_cell(&p.kind),
                markdown_cell(&p.root),
                p.file_count,
                markdown_cell(p.config_file.as_deref().unwrap_or("-"))
            )
            .expect("writing to a String cannot fail");
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
/// Each hit has a heading, a fenced snippet, and source metadata.
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
        &[],
        false,
    )
}

/// As [`render_hits`], but includes the per-hit source-binding outcome before
/// indexed evidence. `full_body` prints the full returned snippet; source
/// validation never replaces or relocates that evidence.
pub(super) fn render_hits_inner(
    hits: &[api::SearchHit],
    empty_msg: &str,
    context: HitContext<'_>,
    show_score: bool,
    sources: &[HitSource],
    full_body: bool,
) -> String {
    if hits.is_empty() {
        return empty_msg.to_string();
    }
    let mut out = format!(
        "**{} {}**",
        hits.len(),
        if hits.len() == 1 { "match" } else { "matches" }
    );
    out.push('\n');
    if show_score {
        render_search_table(&mut out, hits, context, sources);
    }
    let mut expanded_chars = 0_usize;
    let mut shortened_hits = 0_usize;
    for (index, h) in hits.iter().enumerate() {
        let root = context.root_for(h);
        if show_score {
            writeln!(out, "\n**{}**", index + 1).expect("writing to a String cannot fail");
        } else {
            render_hit_heading(&mut out, h, root);
        }
        let expand = full_body
            && (expanded_chars == 0 || expanded_chars + h.snippet.len() <= EXPANDED_BODY_CHARS);
        if expand {
            expanded_chars += h.snippet.len();
        } else if full_body {
            shortened_hits += 1;
        }
        // The preview starts at the first nonblank line. Expanded bodies retain
        // all source bytes, including leading blank lines and indentation.
        let snippet = if expand {
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
            render_fenced(&mut out, &snippet, h.language.as_deref());
        }
        render_hit_metadata(&mut out, h);
        if show_score {
            writeln!(out, "Score: {:.3}", h.score).expect("writing to a String cannot fail");
        }
        if let Some(source) = sources.get(index) {
            if matches!(
                source,
                HitSource::ChangedLocal | HitSource::IndexedRangeMismatch
            ) && let Some(reason) = source.warning()
            {
                writeln!(out, "Source detail: {reason}").expect("writing to a String cannot fail");
            }
            render_source_diagnostic(&mut out, h.path.as_deref().unwrap_or("(no path)"), source);
        }
        out.push('\n');
    }
    if sources
        .iter()
        .any(|source| matches!(source, HitSource::Bound))
    {
        out.push_str("Verified source means matching ready-source bytes and reported coordinates, not semantic symbol/focus identity.\n");
    }
    if shortened_hits > 0 {
        writeln!(
            out,
            "{shortened_hits} hit(s) shown as snippets: expanded bodies stop at \
             {EXPANDED_BODY_CHARS} characters. Use expand_chunk for one of them."
        )
        .expect("writing to a String cannot fail");
    }
    out
}

/// Rank rows summarize the same evidence retained in the source previews below.
fn render_search_table(
    out: &mut String,
    hits: &[api::SearchHit],
    context: HitContext<'_>,
    sources: &[HitSource],
) {
    // Match the native inspector: signed or unavailable scores have no ratio bar.
    let maximum = hits
        .iter()
        .all(|hit| hit.score.is_finite() && hit.score >= 0.0)
        .then(|| hits.iter().map(|hit| hit.score).fold(0.0, f64::max))
        .filter(|maximum| *maximum > 0.0);
    out.push_str("\n| | Match | Source |\n| --- | --- | --- |\n");
    for (index, hit) in hits.iter().enumerate() {
        let binding = match sources.get(index) {
            Some(HitSource::Bound) => "bound",
            Some(HitSource::ChangedLocal) => "changed",
            Some(HitSource::IndexedRangeMismatch) => "range mismatch",
            Some(HitSource::Unverified(_)) => "unverified",
            None => "not probed",
        };
        writeln!(
            out,
            "| {}\u{a0}{} | {} — {} | {} |",
            index + 1,
            score_bar(hit.score, maximum),
            markdown_code(&hit_location(hit, context.root_for(hit))).replace('|', "\\|"),
            markdown_code(&format!("{}{}", hit_symbol(hit), write_marker(hit))).replace('|', "\\|"),
            binding,
        )
        .expect("writing to a String cannot fail");
    }
}

fn score_bar(score: f64, maximum: Option<f64>) -> &'static str {
    const BARS: [&str; 6] = ["▱▱▱▱▱", "▰▱▱▱▱", "▰▰▱▱▱", "▰▰▰▱▱", "▰▰▰▰▱", "▰▰▰▰▰"];
    maximum.map_or("—", |maximum| {
        let ratio = score / maximum;
        BARS[[0.0, 0.2, 0.4, 0.6, 0.8].partition_point(|threshold| ratio > *threshold)]
    })
}

fn render_hit_heading(out: &mut String, hit: &api::SearchHit, root: Option<&Path>) {
    out.push_str("### ");
    write!(
        out,
        "{} — {}",
        markdown_code(&hit_location(hit, root)),
        markdown_code(&hit_symbol(hit))
    )
    .expect("writing to a String cannot fail");
    let marker = write_marker(hit);
    if !marker.is_empty() {
        write!(out, " {}", markdown_code(marker.trim())).expect("writing to a String cannot fail");
    }
    out.push('\n');
}

/// Raw failures follow the preview and remain fenced, including hostile Markdown.
pub(crate) fn render_source_diagnostic(out: &mut String, path: &str, source: &HitSource) {
    if let HitSource::Unverified(reason) = source {
        writeln!(
            out,
            "\nSource verification diagnostic for {}:",
            markdown_code(path)
        )
        .expect("writing to a String cannot fail");
        render_fenced(out, reason, Some("text"));
    }
}

fn render_hit_metadata(out: &mut String, hit: &api::SearchHit) {
    write!(out, "Domain: {}", markdown_code(&hit.domain_id))
        .expect("writing to a String cannot fail");
    if !hit.kind.is_empty() {
        write!(out, " · Kind: {}", markdown_code(&hit.kind))
            .expect("writing to a String cannot fail");
    }
    if let Some(codebase) = &hit.codebase_id {
        write!(out, " · Codebase: {}", markdown_code(codebase))
            .expect("writing to a String cannot fail");
    }
    out.push('\n');
}

/// Keep untrusted values on one line and outside Markdown syntax.
pub(super) fn markdown_code(value: &str) -> String {
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

/// Keep pipe escapes outside code spans, including values ending in backslashes.
pub(super) fn markdown_cell(value: &str) -> String {
    if !value.contains('|') {
        return markdown_code(value);
    }
    let mut out = String::new();
    for (index, part) in value.split('|').enumerate() {
        if index > 0 {
            out.push_str("\\|");
        }
        if !part.is_empty() {
            out.push_str(&markdown_code(part));
        }
    }
    out
}

/// Pass suffixes as display-only hints to the host's grammar-alias resolver.
/// This does not identify a parser language; absent or unsafe hints render text.
pub(super) fn source_language_hint(path: &str) -> Option<&str> {
    let path = Path::new(path);
    match path.file_name()?.to_str()? {
        "CMakeLists.txt" => return Some("cmake"),
        "Makefile" | "makefile" => return Some("makefile"),
        "Dockerfile" => return Some("dockerfile"),
        _ => {}
    }
    let suffix = path.extension()?.to_str()?;
    // Fence normalization lowercases hints. Preserve the C++ meaning of `.C`.
    Some(if suffix == "C" { "cpp" } else { suffix })
}

/// Protect whitespace and source delimiters without changing the body bytes.
pub(super) fn render_fenced(out: &mut String, body: &str, language: Option<&str>) {
    let fence = "`".repeat(longest_backtick_run(body).saturating_add(1).max(3));
    let language = fence_language(language);
    write!(out, "{fence}{language}\n{body}").expect("writing to a String cannot fail");
    if !body.ends_with('\n') {
        out.push('\n');
    }
    writeln!(out, "{fence}").expect("writing to a String cannot fail");
}

fn longest_backtick_run(value: &str) -> usize {
    value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}

/// Keep source literal even when the host reserves a fence for rich rendering.
fn fence_language(language: Option<&str>) -> Cow<'_, str> {
    let language = language
        .filter(|value| {
            !value.is_empty()
                && value.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '_' | '-' | '+' | '#' | '.')
                })
        })
        .unwrap_or("text");
    if language.eq_ignore_ascii_case("mermaid") {
        Cow::Borrowed("text")
    } else if language.eq_ignore_ascii_case("svg") {
        Cow::Borrowed("xml")
    } else if language.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(language.to_ascii_lowercase())
    } else {
        Cow::Borrowed(language)
    }
}

/// The total length of expanded bodies in one search answer. The first hit is
/// always expanded; a later hit that does not fit is shown as a snippet. The
/// server has no size limit of its own, and one expanded search with a broad
/// `top_k` can otherwise return tens of thousands of characters.
const EXPANDED_BODY_CHARS: usize = 30_000;

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

    use super::{EXPANDED_BODY_CHARS, HitContext, api, render_hits_inner, strip_verbatim_prefix};
    use crate::query::HitSource;

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

    /// A hit whose expanded body has `lines` lines of 10 characters each.
    fn body(id: &str, lines: usize) -> api::SearchHit {
        let mut hit = node(id, &format!("{id}.rs"));
        hit.snippet = format!("{id}-body-ln\n").repeat(lines);
        hit
    }

    fn expanded(hits: &[api::SearchHit]) -> String {
        render_hits_inner(hits, "none", HitContext::local(None), true, &[], true)
    }

    #[test]
    fn expanded_bodies_stop_at_the_limit_and_later_hits_that_fit_still_expand() {
        // 2,000 + 1,500 + 500 lines of 10 characters: the second body would pass
        // 30,000 characters, the third still fits after it is skipped.
        let out = expanded(&[body("a", 2_000), body("b", 1_500), body("c", 500)]);

        assert_eq!(out.matches("a-body-ln").count(), 2_000);
        assert_eq!(out.matches("b-body-ln").count(), 4, "shown as a snippet");
        assert_eq!(out.matches("c-body-ln").count(), 500);
        assert!(
            out.ends_with(&format!(
                "1 hit(s) shown as snippets: expanded bodies stop at {EXPANDED_BODY_CHARS} \
                 characters. Use expand_chunk for one of them.\n"
            )),
            "{out}"
        );
    }

    #[test]
    fn the_first_expanded_body_is_never_cut_and_small_answers_have_no_note() {
        let oversized = expanded(&[body("a", 4_000)]);
        assert_eq!(oversized.matches("a-body-ln").count(), 4_000);
        assert!(!oversized.contains("shown as snippets"), "{oversized}");

        let small = expanded(&[body("a", 10), body("b", 10)]);
        assert!(!small.contains("shown as snippets"), "{small}");
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
        let sources = [HitSource::ChangedLocal];
        let output = render_hits_inner(
            &[local, remote, unidentified],
            "none",
            context,
            true,
            &sources,
            false,
        );
        let local_row = output
            .lines()
            .find(|line| line.contains("`local`"))
            .unwrap();
        let remote_row = output
            .lines()
            .find(|line| line.contains("`remote`"))
            .unwrap();
        let unknown_row = output
            .lines()
            .find(|line| line.contains("`unidentified`"))
            .unwrap();
        assert!(local_row.contains(&root.join("src/lib.rs").display().to_string()));
        assert!(remote_row.contains("`src/lib.rs:1-2`") && !remote_row.contains("checkout-a"));
        assert!(unknown_row.contains("`src/lib.rs:1-2`") && !unknown_row.contains("checkout-a"));
        assert!(output.contains("Codebase: `a`") && output.contains("Codebase: `b`"));
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
            &[],
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
            !out.contains("hybrid score"),
            "graph hits have no relevance score: {out}"
        );
    }

    #[test]
    fn source_diagnostics_preserve_raw_failure_after_source_without_markdown_injection() {
        let mut hit = node("original", "source.rs");
        hit.snippet = "fn original() {}".into();
        let diagnostic = "FileNotIndexed Pending\n```\n# injected heading\n````\n";
        let output = render_hits_inner(
            &[hit],
            "none",
            HitContext::local(None),
            false,
            &[HitSource::Unverified(diagnostic.into())],
            true,
        );
        assert!(output.contains("source.rs:1-2") && output.contains("fn original() {}"));
        assert!(output.find("fn original() {}").unwrap() < output.find("FileNotIndexed").unwrap());
        assert!(output.contains(&format!("`````text\n{diagnostic}`````\n")));
    }

    #[test]
    fn render_fences_preserve_snippet_markdown_and_preview_line_selection() {
        let mut hit = node("snippet", "source.rs");
        hit.snippet =
            "\n  \nfn example() {\n    ```rust\n    ~~~\n    **text** <tag>  \n    omitted\n}"
                .into();
        let output = render_hits_inner(&[hit], "none", HitContext::local(None), false, &[], false);
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
        let output = render_hits_inner(&[hit], "none", HitContext::local(None), false, &[], true);
        assert!(
            output.contains(&format!("\n``````rust\n{source}``````\n")),
            "{output}"
        );
    }

    #[test]
    fn render_untrusted_values_cannot_open_markdown_blocks() {
        let mut hit = node("`symbol`|\n<!--", "`source`|\n```\n.rs");
        hit.language = Some("rust\n```".into());
        hit.domain_id = "<details>\n```".into();
        hit.codebase_id = Some("a`\r\n# hidden".into());
        hit.snippet = "let value = 1;".into();
        let output = render_hits_inner(&[hit], "none", HitContext::local(None), true, &[], false);
        let row = output
            .lines()
            .find(|line| line.contains("`symbol`"))
            .unwrap();
        assert!(row.contains("```` `source`\\|\\n```\\n.rs:1-2 ````"));
        assert!(row.contains("`` `symbol`\\|\\n<!-- ``"));
        assert!(output.contains("Codebase: ``a`\\r\\n# hidden``"));
        assert!(output.contains("\n```text\nlet value = 1;\n```\n"));
    }

    #[test]
    fn render_empty_results_preserves_the_exact_message() {
        for message in ["no results", "nothing calls `run`", ""] {
            assert_eq!(
                render_hits_inner(&[], message, HitContext::local(None), true, &[], false,),
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
        let output = render_hits_inner(&[hit], "none", HitContext::local(None), false, &[], false);
        assert!(output.contains("`(no location)` — (empty)"));
        assert!(output.contains("Domain: (empty)"));
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
}
