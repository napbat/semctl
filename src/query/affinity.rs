//! Semantic file affinity from the server's hybrid search ranking.
//!
//! The server returns no raw embedding vectors. Each file therefore gets a
//! retrieval profile: the ranked result files of one search whose query
//! describes the file. Two files are related when their profiles overlap, or
//! when one file occurs in the results of the other. A file can have a useful
//! profile even when its own bounded ranking does not return it.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use crate::client::ApiFailure;

use super::display::{AffinityFile, AffinityPair, QuerySource, project_affinity};
use super::{
    Client, FailureKind, QueryOutput, QueryPresentation, ToolError, api, bound_codebase, urlencode,
};

const OP: &str = "affinity";
const MIN_FILES: usize = 2;
const MAX_FILES: usize = 64;
const MAX_PATH_BYTES: usize = 4096;
const TOP_K: u32 = 100;
const MAX_IN_FLIGHT: usize = 8;
const MAX_MARKDOWN_BYTES: u64 = 256 * 1024;
const MAX_TERMS: usize = 16;
const MAX_QUERY_CHARS: usize = 600;
const MIN_TERM_LETTERS: usize = 3;
const SHARED_THRESHOLD: f64 = 0.05;

/// The semantic affinity of 2 to 64 codebase-relative files.
pub async fn affinity(
    client: &Client,
    paths: &[String],
    presentation: QueryPresentation,
) -> Result<QueryOutput, ToolError> {
    let files = requested_paths(paths)?;
    // A loading layer gives a caveat for one file, not a wait for all files.
    let snapshot = client.available_snapshot();
    let client = &snapshot;
    let codebase: Arc<str> = bound_codebase(client, OP)?.into();

    let descriptions = bounded(
        files
            .iter()
            .map(|path| describe(client.clone(), Arc::clone(&codebase), (*path).to_owned())),
    )
    .await?;
    let searches = bounded(files.iter().zip(&descriptions).map(|(path, (query, _))| {
        search(
            client.clone(),
            Arc::clone(&codebase),
            (*path).to_owned(),
            query.clone(),
        )
    }))
    .await?;

    let mut caveats = Vec::new();
    let ranked: Vec<Option<Vec<String>>> = searches
        .into_iter()
        .map(|search| match search {
            Searched::Ranked(paths) => Some(paths),
            Searched::Loading(caveat) => {
                caveats.push(caveat);
                None
            }
        })
        .collect();
    let profiles: Vec<Option<HashMap<&str, f64>>> = ranked
        .iter()
        .map(|paths| paths.as_deref().map(profile))
        .collect();
    let pairs = related_pairs(&files, &profiles);
    let entries: Vec<AffinityFile<'_>> = files
        .iter()
        .zip(&descriptions)
        .zip(&ranked)
        .map(|((&path, (_, query)), paths)| AffinityFile::new(path, *query, paths.as_deref()))
        .collect();
    let text = summary(&entries, pairs.len(), caveats.len());
    caveats.push("Affinity compares the ranked result files of one bounded hybrid search per file. The results do not measure index coverage or show structural relationships.".into());
    let display = if presentation == QueryPresentation::Inspector {
        Some(project_affinity(client, entries, pairs, caveats)?)
    } else {
        None
    };
    Ok(QueryOutput::new(text, display))
}

fn invalid(detail: impl Into<String>) -> ToolError {
    ToolError::new(OP, FailureKind::InvalidArgument, detail)
}

/// Validate each path like the overview projection does. Duplicates collapse to
/// their first occurrence.
fn requested_paths(paths: &[String]) -> Result<Vec<&str>, ToolError> {
    if !(MIN_FILES..=MAX_FILES).contains(&paths.len()) {
        return Err(invalid(format!(
            "pass {MIN_FILES} to {MAX_FILES} paths; got {}",
            paths.len()
        )));
    }
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for path in paths {
        if path.is_empty()
            || path.len() > MAX_PATH_BYTES
            || path.contains(['\\', ':'])
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(invalid(format!(
                "{path:?} is not a codebase-relative POSIX path"
            )));
        }
        if seen.insert(path.as_str()) {
            files.push(path.as_str());
        }
    }
    if files.len() < MIN_FILES {
        return Err(invalid(format!("pass at least {MIN_FILES} distinct paths")));
    }
    Ok(files)
}

/// Run `jobs` with at most [`MAX_IN_FLIGHT`] in flight and keep their order. The
/// first failure drops the set, which aborts the jobs that remain.
async fn bounded<T, F>(jobs: impl IntoIterator<Item = F>) -> Result<Vec<T>, ToolError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, ToolError>> + Send + 'static,
{
    let mut pending = jobs.into_iter().enumerate();
    let mut results = Vec::new();
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        while tasks.len() < MAX_IN_FLIGHT {
            let Some((index, job)) = pending.next() else {
                break;
            };
            results.push(None);
            tasks.spawn(async move { (index, job.await) });
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        let (index, result) = joined.map_err(|error| {
            ToolError::new(
                OP,
                FailureKind::Unavailable,
                format!("an affinity request task failed: {error}"),
            )
        })?;
        results[index] = Some(result?);
    }
    Ok(results.into_iter().flatten().collect())
}

/// The search text for one file and where it came from. Markdown headings come
/// first, then outline names, then the file name.
async fn describe(
    client: Client,
    codebase: Arc<str>,
    path: String,
) -> Result<(String, QuerySource), ToolError> {
    if is_markdown(&path)
        && let Some(root) = client.local_root().map(Path::to_path_buf)
    {
        let file = path.clone();
        let text = tokio::task::spawn_blocking(move || local_markdown(&root, &file))
            .await
            .ok()
            .flatten();
        if let Some(query) = text.as_deref().and_then(heading_query) {
            return Ok((query, QuerySource::Content));
        }
    }
    let url = format!(
        "/v1/codebases/{codebase}/graph/outline?path={}&includeBody=false",
        urlencode(&path)
    );
    match client.get::<api::FileOutline>(&url).await {
        Ok(outline) => {
            if let Some(query) = outline_query(&outline) {
                return Ok((query, QuerySource::Outline));
            }
        }
        Err(error) if outline_absent(&error) => {}
        Err(error) => return Err(ToolError::from_client(OP, &error)),
    }
    Ok((name_query(&path), QuerySource::Name))
}

/// An absent or loading outline falls back to the file name. Other failures are fatal.
fn outline_absent(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<ApiFailure>().is_some_and(|failure| {
            failure.is_loading() || (failure.status().as_u16() == 404 && failure.is_api_response())
        })
    })
}

fn is_markdown(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        ["md", "markdown", "mdx"]
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// Read at most [`MAX_MARKDOWN_BYTES`] of a checkout file. The capability
/// directory keeps the path inside the checkout.
fn local_markdown(root: &Path, path: &str) -> Option<String> {
    let directory = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority()).ok()?;
    let file = directory.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_MARKDOWN_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

enum Searched {
    Ranked(Vec<String>),
    Loading(String),
}

async fn search(
    client: Client,
    codebase: Arc<str>,
    path: String,
    query: String,
) -> Result<Searched, ToolError> {
    let body = api::SearchRequestBody {
        query: &query,
        top_k: TOP_K,
        codebase_id: Some(&codebase),
        codebase_ids: None,
        scope: None,
        domains: None,
        filters: None,
        kinds: None,
        prefer: None,
        granularity: None,
    };
    match client
        .post_read::<_, Vec<api::SearchHit>>("/v1/search", &body)
        .await
    {
        Ok(hits) => Ok(Searched::Ranked(ranked_paths(&hits))),
        Err(error)
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<ApiFailure>()
                    .is_some_and(ApiFailure::is_loading)
            }) =>
        {
            Ok(Searched::Loading(format!(
                "Search for {path} unavailable while the backend loads: {error:#}"
            )))
        }
        Err(error) => Err(ToolError::from_client(OP, &error)),
    }
}

/// Distinct result paths in rank order.
fn ranked_paths(hits: &[api::SearchHit]) -> Vec<String> {
    let mut seen = HashSet::new();
    hits.iter()
        .filter_map(|hit| hit.path.as_deref())
        .filter(|path| seen.insert(*path))
        .map(str::to_owned)
        .collect()
}

/// Weight `1/sqrt(rank)`: reciprocal rank makes profiles too peaked to overlap.
fn profile(ranked: &[String]) -> HashMap<&str, f64> {
    ranked
        .iter()
        .zip(1_u32..)
        .map(|(path, rank)| (path.as_str(), 1.0 / f64::from(rank).sqrt()))
        .collect()
}

fn round4(value: f64) -> f64 {
    ((value * 10_000.0).round() / 10_000.0).clamp(0.0, 1.0)
}

/// Cosine of two profiles without the pair's own paths.
fn shared(a: &HashMap<&str, f64>, b: &HashMap<&str, f64>, pair: [&str; 2]) -> f64 {
    let norm = |profile: &HashMap<&str, f64>| {
        profile
            .iter()
            .filter(|(path, _)| !pair.contains(path))
            .map(|(_, weight)| weight * weight)
            .sum::<f64>()
            .sqrt()
    };
    let (norm_a, norm_b) = (norm(a), norm(b));
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    let dot: f64 = a
        .iter()
        .filter(|(path, _)| !pair.contains(path))
        .filter_map(|(path, weight)| b.get(path).map(|other| weight * other))
        .sum();
    dot / (norm_a * norm_b)
}

/// Pairs with a direct hit or a shared profile at or above the threshold, as
/// `a < b` in byte order, sorted.
fn related_pairs<'a>(
    files: &[&'a str],
    profiles: &[Option<HashMap<&str, f64>>],
) -> Vec<AffinityPair<'a>> {
    let mut pairs = Vec::new();
    for (i, (first, first_profile)) in files.iter().zip(profiles).enumerate() {
        for (second, second_profile) in files.iter().zip(profiles).skip(i + 1) {
            let (a, b, profile_a, profile_b) = if first < second {
                (*first, *second, first_profile, second_profile)
            } else {
                (*second, *first, second_profile, first_profile)
            };
            let weight = |profile: &Option<HashMap<&str, f64>>, path: &str| {
                profile
                    .as_ref()
                    .and_then(|profile| profile.get(path).copied())
                    .unwrap_or(0.0)
            };
            let direct = round4(f64::midpoint(weight(profile_a, b), weight(profile_b, a)));
            let shared = match (profile_a, profile_b) {
                (Some(profile_a), Some(profile_b)) => round4(shared(profile_a, profile_b, [a, b])),
                _ => 0.0,
            };
            if direct > 0.0 || shared >= SHARED_THRESHOLD {
                pairs.push(AffinityPair {
                    a,
                    b,
                    direct,
                    shared,
                });
            }
        }
    }
    pairs.sort_by(|left, right| (left.a, left.b).cmp(&(right.a, right.b)));
    pairs
}

/// ATX headings of levels 1 to 4 outside fenced code blocks, as plain text.
fn markdown_headings(text: &str) -> Vec<String> {
    let mut headings = Vec::new();
    let mut open: Option<(char, usize)> = None;
    for line in text.lines() {
        if let Some((marker, length)) = open {
            if let Some((closing, closing_length, info)) = fence(line)
                && closing == marker
                && closing_length >= length
                && !info
            {
                open = None;
            }
            continue;
        }
        if let Some((marker, length, _)) = fence(line) {
            open = Some((marker, length));
        } else if let Some(heading) = atx_heading(line) {
            headings.push(plain_inline(heading));
        }
    }
    headings
}

/// The marker, the marker length, and whether an info string follows.
fn fence(line: &str) -> Option<(char, usize, bool)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let marker = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let length = rest.len() - rest.trim_start_matches(marker).len();
    if length < 3 {
        return None;
    }
    let info = rest[length..].trim();
    if marker == '`' && info.contains('`') {
        return None;
    }
    Some((marker, length, !info.is_empty()))
}

fn atx_heading(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let level = rest.len() - rest.trim_start_matches('#').len();
    if !(1..=4).contains(&level) {
        return None;
    }
    let body = &rest[level..];
    if !body.is_empty() && !body.starts_with([' ', '\t']) {
        return None;
    }
    let body = body.trim();
    let open = body.trim_end_matches('#');
    if open.is_empty() {
        return Some(open);
    }
    if open.len() < body.len() && open.ends_with([' ', '\t']) {
        return Some(open.trim_end());
    }
    Some(body)
}

/// Link and image text without targets, and no code, emphasis, or HTML marks.
fn plain_inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        let next = chars.get(index + 1).copied();
        match current {
            '\\' if next.is_some_and(|c| c.is_ascii_punctuation()) => {
                out.extend(next);
                index += 1;
            }
            ']' if next == Some('(') || next == Some('[') => {
                let closing = if next == Some('(') { ')' } else { ']' };
                index = skip_group(&chars, index + 1, closing);
            }
            '<' if next.is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!')
                && chars[index..].contains(&'>') =>
            {
                while chars[index] != '>' {
                    index += 1;
                }
            }
            '_' => {
                let inner = index > 0
                    && chars[index - 1].is_alphanumeric()
                    && next.is_some_and(char::is_alphanumeric);
                if inner {
                    out.push('_');
                }
            }
            '!' if next == Some('[') => {}
            '[' | ']' | '`' | '*' | '~' => {}
            _ => out.push(current),
        }
        index += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The index of the `closing` mark that ends the group opened at `start`.
fn skip_group(chars: &[char], start: usize, closing: char) -> usize {
    let opening = chars[start];
    let mut depth = 0_usize;
    for (index, c) in chars.iter().enumerate().skip(start) {
        if *c == opening && opening != closing {
            depth += 1;
        } else if *c == closing {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return index;
            }
        }
    }
    chars.len()
}

/// Headings without near-duplicates (same letters, case ignored) and without
/// headings of fewer than three letters.
fn heading_query(text: &str) -> Option<String> {
    let mut seen = HashSet::new();
    let terms = markdown_headings(text).into_iter().filter(|heading| {
        let letters: String = heading
            .chars()
            .filter(|c| c.is_alphabetic())
            .flat_map(char::to_lowercase)
            .collect();
        letters.chars().count() >= MIN_TERM_LETTERS && seen.insert(letters)
    });
    join_terms(terms)
}

/// Distinct entry symbol names without their qualifier.
fn outline_query(outline: &api::FileOutline) -> Option<String> {
    let mut seen = HashSet::new();
    let terms = outline
        .entries
        .iter()
        .filter_map(|entry| {
            entry
                .symbol
                .as_deref()
                .or(entry.qualified_symbol.as_deref())
        })
        .filter_map(|symbol| symbol.rsplit("::").next())
        .map(str::trim)
        .filter(|name| !name.is_empty() && seen.insert(*name))
        .map(str::to_owned);
    join_terms(terms)
}

/// The first [`MAX_TERMS`] terms joined with "; ", at most [`MAX_QUERY_CHARS`]
/// characters.
fn join_terms(terms: impl Iterator<Item = String>) -> Option<String> {
    let mut query = terms.take(MAX_TERMS).collect::<Vec<_>>().join("; ");
    if let Some((end, _)) = query.char_indices().nth(MAX_QUERY_CHARS) {
        query.truncate(end);
    }
    (!query.trim().is_empty()).then_some(query)
}

/// The humanized file stem plus its parent folder name.
fn name_query(path: &str) -> String {
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or(path);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let stem = if stem.is_empty() { file } else { stem };
    let mut query = humanize(stem);
    if let Some(parent) = parts.next() {
        let parent = humanize(parent);
        if !parent.is_empty() {
            if !query.is_empty() {
                query.push(' ');
            }
            query.push_str(&parent);
        }
    }
    if query.is_empty() {
        path.to_owned()
    } else {
        query
    }
}

/// Words split at separators and at lower-to-upper case changes.
fn humanize(name: &str) -> String {
    let mut spaced = String::with_capacity(name.len() + 8);
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if c.is_alphanumeric() {
            if c.is_uppercase() && previous.is_some_and(char::is_lowercase) {
                spaced.push(' ');
            }
            spaced.push(c);
        } else {
            spaced.push(' ');
        }
        previous = Some(c);
    }
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn summary(entries: &[AffinityFile<'_>], pairs: usize, loading: usize) -> String {
    let files = entries.len();
    let mut text = format!(
        "Semantic affinity: {files} files, {pairs} related {}.",
        if pairs == 1 { "pair" } else { "pairs" }
    );
    let completed = files - loading;
    if completed > 0 {
        let self_matches = entries.iter().filter(|entry| entry.self_match).count();
        let searches = if completed == 1 { "search" } else { "searches" };
        write!(
            text,
            " Input files returned: {self_matches} of {completed} completed {searches}."
        )
        .expect("writing to a String cannot fail");
    }
    let empty = entries.iter().filter(|entry| entry.is_empty()).count();
    if empty > 0 {
        let searches = if empty == 1 { "search" } else { "searches" };
        write!(text, " {empty} {searches} returned no result files.")
            .expect("writing to a String cannot fail");
    }
    if loading > 0 {
        write!(
            text,
            " {loading} {} unavailable while the backend loads.",
            if loading == 1 {
                "search was"
            } else {
                "searches were"
            }
        )
        .expect("writing to a String cannot fail");
    }
    text
}

#[cfg(test)]
mod tests;
