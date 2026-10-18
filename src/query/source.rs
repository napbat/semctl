//! Content-plane requests and strong local source binding shared by reads and search.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::sync::MAX_FILE_BYTES;

use crate::client::{Client, api};

use super::render::HitContext;
use super::{ToolError, bound_codebase, urlencode};

/// One request owner: the content plane, not the catalog, enforces ready state.
pub(super) async fn fetch_source(
    client: &Client,
    path: &str,
    revision: Option<&str>,
    byte_range: Option<(u64, u64)>,
    line_range: Option<(u32, u32)>,
) -> Result<api::FileContent, ToolError> {
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
    client
        .get(&url)
        .await
        .map_err(|error| ToolError::from_client("read_source", &error))
}

/// A positive result binds only source bytes and supplied coordinates, never
/// symbol identity, graph generation, or the meaning of a focus token.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HitSource {
    Bound,
    ChangedLocal,
    IndexedRangeMismatch,
    Unverified(Arc<str>),
}

impl HitSource {
    pub(crate) fn warning(&self) -> Option<&'static str> {
        match self {
            Self::Bound => None,
            Self::ChangedLocal => Some(
                "local bytes differ from ready content-plane source; read the local file before using indexed locations",
            ),
            Self::IndexedRangeMismatch => Some(
                "indexed snippet/coordinates differ from ready source; use local Read/Grep for current locations",
            ),
            Self::Unverified(_) => Some(
                "source binding unavailable; indexed evidence only; use local Read/Grep for current locations",
            ),
        }
    }
}

// Search is an interactive read, not a catalog walk: bound concurrent per-file
// work independently of top_k, and leave half the hook's six-second envelope
// for search/connection/rendering. Budget misses preserve the hit as unverified.
const MAX_PROBE_PATHS: usize = 16;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Read one file once, never beyond a bounded budget, and keep filesystem I/O off
/// the async executor. A ready hash is not itself evidence that a search anchor
/// came from that revision; `bind_hit_at_lines` checks snippet coordinates too.
pub(crate) async fn hit_sources(
    client: &Client,
    hits: &Arc<Vec<api::SearchHit>>,
) -> Vec<HitSource> {
    let context = HitContext::search(client.local_root(), client.codebase_raw(), true);
    hit_sources_in_context(client, hits, context).await
}

pub(super) async fn hit_sources_in_context(
    client: &Client,
    hits: &Arc<Vec<api::SearchHit>>,
    context: HitContext<'_>,
) -> Vec<HitSource> {
    let mut tasks = tokio::task::JoinSet::new();
    let mut paths = HashMap::new();
    let mut sources = Vec::new();
    for hit in hits.iter() {
        let (Some(root), Some(path)) = (context.root_for(hit), hit.path.as_deref()) else {
            continue;
        };
        if paths.len() == MAX_PROBE_PATHS || paths.contains_key(path) {
            continue;
        }
        let index = sources.len();
        paths.insert(path, index);
        sources.push(None);
        let client = client.clone();
        let root = root.to_path_buf();
        let path = path.to_string();
        tasks.spawn(async move {
            let probe = tokio::time::timeout(
                PROBE_TIMEOUT,
                fetch_source(&client, &path, None, Some((0, 1)), None),
            )
            .await;
            let result = match probe {
                Ok(Ok(response)) => {
                    tokio::task::spawn_blocking(move || local_source(&root, &path, &response))
                        .await
                        .unwrap_or_else(|error| {
                            Err(LocalSourceError::Unverified(error.to_string().into()))
                        })
                }
                Ok(Err(error)) => Err(LocalSourceError::Unverified(error.to_string().into())),
                Err(error) => Err(LocalSourceError::Unverified(
                    format!("content-plane probe timed out: {error}").into(),
                )),
            };
            (index, result)
        });
    }
    let targets: Vec<_> = hits
        .iter()
        .map(|hit| {
            if context.root_for(hit).is_none() {
                HitTarget::NoCheckout
            } else if let Some(path) = hit.path.as_deref() {
                paths
                    .get(path)
                    .map_or(HitTarget::Budget, |&index| HitTarget::File(index))
            } else {
                HitTarget::NoPath
            }
        })
        .collect();
    drop(paths);
    let mut task_failure = None;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((index, source)) => sources[index] = Some(source),
            Err(error) => task_failure = Some(Arc::<str>::from(error.to_string())),
        }
    }
    let worker_hits = Arc::clone(hits);
    tokio::task::spawn_blocking(move || {
        classify_sources(&worker_hits, &targets, &sources, task_failure.as_ref())
    })
    .await
    .unwrap_or_else(|error| {
        let reason: Arc<str> = error.to_string().into();
        hits.iter()
            .map(|_| HitSource::Unverified(Arc::clone(&reason)))
            .collect()
    })
}

enum HitTarget {
    NoCheckout,
    NoPath,
    Budget,
    File(usize),
}

/// All source-sized comparison and coordinate work runs in the blocking worker.
fn classify_sources(
    hits: &[api::SearchHit],
    targets: &[HitTarget],
    sources: &[Option<Result<Vec<u8>, LocalSourceError>>],
    task_failure: Option<&Arc<str>>,
) -> Vec<HitSource> {
    let mut offsets = vec![Vec::new(); sources.len()];
    for (hit, target) in hits.iter().zip(targets) {
        if let HitTarget::File(index) = target {
            offsets[*index].extend(
                hit_coordinates(hit)
                    .into_iter()
                    .filter_map(|(offset, _)| offset),
            );
        }
    }
    let lines: Vec<_> = sources
        .iter()
        .zip(offsets)
        .map(|(source, offsets)| match source {
            Some(Ok(bytes)) => requested_lines(bytes, offsets),
            _ => Vec::new(),
        })
        .collect();
    hits.iter()
        .zip(targets)
        .map(|(hit, target)| {
            let index = match target {
                HitTarget::NoCheckout => {
                    return HitSource::Unverified(
                        "no selected local checkout for this hit's codebase".into(),
                    );
                }
                HitTarget::NoPath => return HitSource::Unverified("hit has no source path".into()),
                HitTarget::Budget => {
                    return HitSource::Unverified(
                        "per-search source probe budget exhausted".into(),
                    );
                }
                HitTarget::File(index) => *index,
            };
            match sources[index].as_ref() {
                Some(Ok(bytes)) => bind_hit_at_lines(hit, bytes, &lines[index]),
                Some(Err(LocalSourceError::Changed)) => HitSource::ChangedLocal,
                Some(Err(LocalSourceError::Unverified(reason))) => {
                    HitSource::Unverified(Arc::clone(reason))
                }
                None => HitSource::Unverified(
                    task_failure.map_or_else(|| Arc::from("source worker unavailable"), Arc::clone),
                ),
            }
        })
        .collect()
}

#[derive(Debug)]
enum LocalSourceError {
    Changed,
    Unverified(Arc<str>),
}

/// Strong full-file identity, not a catalog hash or a hash of the returned window.
fn local_source(
    root: &Path,
    path: &str,
    content: &api::FileContent,
) -> Result<Vec<u8>, LocalSourceError> {
    let unavailable =
        |error: std::io::Error| LocalSourceError::Unverified(error.to_string().into());
    if content.path != path || content.content_hash.is_empty() {
        return Err(LocalSourceError::Unverified(
            "content-plane path/hash identity missing or different".into(),
        ));
    }
    let root = std::fs::canonicalize(root).map_err(unavailable)?;
    let target = std::fs::canonicalize(root.join(path)).map_err(unavailable)?;
    if !target.starts_with(root) {
        return Err(LocalSourceError::Unverified(
            "source path escapes the selected checkout".into(),
        ));
    }
    let file = std::fs::File::open(target).map_err(unavailable)?;
    if !file.metadata().map_err(unavailable)?.is_file() {
        return Err(LocalSourceError::Unverified(
            "local source is not a regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(unavailable)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(LocalSourceError::Unverified(
            format!("local source exceeds the {MAX_FILE_BYTES}-byte indexing/verification budget")
                .into(),
        ));
    }
    if blake3::hash(&bytes).to_hex().as_str() != content.content_hash {
        return Err(LocalSourceError::Changed);
    }
    Ok(bytes)
}

pub(super) fn verified_local_window(
    root: &Path,
    path: &str,
    content: &api::FileContent,
) -> Option<String> {
    let bytes = local_source(root, path, content).ok()?;
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

fn hit_coordinates(hit: &api::SearchHit) -> [(Option<usize>, Option<u32>); 5] {
    [
        (
            hit.snippet_byte_start.map(|n| n as usize),
            hit.snippet_line_start,
        ),
        (
            hit.snippet_byte_end
                .and_then(|n| n.checked_sub(1))
                .map(|n| n as usize),
            hit.snippet_line_end,
        ),
        (hit.byte_start.map(|n| n as usize), hit.line_start),
        (
            hit.byte_end
                .and_then(|n| n.checked_sub(1))
                .map(|n| n as usize),
            hit.line_end,
        ),
        (hit.focus_byte.map(|n| n as usize), hit.focus_line),
    ]
}

/// Store only requested byte offsets, not every newline in a potentially large
/// file. Sorted offsets share one forward pass; newline bytes belong to the
/// preceding line, and the byte immediately after a newline starts the next.
fn requested_lines(bytes: &[u8], mut offsets: Vec<usize>) -> Vec<(usize, u32)> {
    offsets.retain(|&offset| offset < bytes.len());
    offsets.sort_unstable();
    offsets.dedup();
    let mut cursor = 0;
    let mut line = 1;
    offsets
        .into_iter()
        .map(|offset| {
            for &byte in &bytes[cursor..offset] {
                if byte == b'\n' {
                    line += 1;
                }
            }
            cursor = offset;
            (offset, line)
        })
        .collect()
}

fn bind_hit_at_lines(hit: &api::SearchHit, bytes: &[u8], lines: &[(usize, u32)]) -> HitSource {
    let (Some(start), Some(end)) = (hit.snippet_byte_start, hit.snippet_byte_end) else {
        return HitSource::Unverified("indexed snippet has no explicit byte range".into());
    };
    let (start, end) = (start as usize, end as usize);
    if start >= end || bytes.get(start..end) != Some(hit.snippet.as_bytes()) {
        return HitSource::IndexedRangeMismatch;
    }
    let (Some(location_start), Some(location_end)) = (hit.byte_start, hit.byte_end) else {
        return HitSource::Unverified(
            "snippet matches source but reported location has no byte range".into(),
        );
    };
    if location_start >= location_end || (location_end as usize) > bytes.len() {
        return HitSource::IndexedRangeMismatch;
    }
    for (offset, line) in hit_coordinates(hit) {
        match (offset, line) {
            (Some(offset), line) => {
                if offset >= bytes.len()
                    || line.is_some_and(|line| {
                        lines
                            .binary_search_by_key(&offset, |&(offset, _)| offset)
                            .ok()
                            .map(|index| lines[index].1)
                            != Some(line)
                    })
                {
                    return HitSource::IndexedRangeMismatch;
                }
            }
            (None, Some(_)) => {
                return HitSource::Unverified(
                    "indexed line coordinate has no byte coordinate".into(),
                );
            }
            (None, None) => {}
        }
    }
    if hit.line_start.is_none() || hit.line_end.is_none() {
        return HitSource::Unverified(
            "snippet matches source but reported location has no line range".into(),
        );
    }
    HitSource::Bound
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::stub::{Reply, Stub};
    use crate::query::render::render_hits_inner;

    fn bind_hit(hit: &api::SearchHit, bytes: &[u8]) -> HitSource {
        let offsets = hit_coordinates(hit)
            .into_iter()
            .filter_map(|(offset, _)| offset)
            .collect();
        bind_hit_at_lines(hit, bytes, &requested_lines(bytes, offsets))
    }

    #[test]
    fn sparse_requested_lines_preserve_crlf_blank_lines_utf8_and_end_bounds() {
        let bytes = "a\r\n\nβ\nz".as_bytes();
        assert_eq!(
            requested_lines(
                bytes,
                vec![7, 3, 0, 6, 4, 2, 1, 3, 5, bytes.len(), usize::MAX]
            ),
            vec![
                (0, 1),
                (1, 1),
                (2, 1),
                (3, 2),
                (4, 3),
                (5, 3),
                (6, 3),
                (7, 4)
            ],
        );
        assert!(requested_lines(b"", vec![0, 1]).is_empty());
    }

    #[test]
    fn same_file_hits_share_correct_coordinates_across_distinct_newline_boundaries() {
        let source = "a\r\n\nβ\nz";
        let specifications = [
            ("a\r\n", 0, 3, 1, 1, 0, 3, 1, 1, 2, 1),
            ("\nβ\n", 3, 7, 2, 3, 4, 6, 3, 3, 5, 3),
            ("z", 7, 8, 4, 4, 7, 8, 4, 4, 7, 4),
            ("z", 7, 8, 4, 4, 7, 8, 4, 4, 7, 3),
        ];
        let hits: Vec<_> = specifications
            .into_iter()
            .map(
                |(snippet, ss, se, sls, sle, start, end, ls, le, focus, fl)| {
                    let mut indexed = hit(snippet);
                    indexed.snippet_byte_start = Some(ss);
                    indexed.snippet_byte_end = Some(se);
                    indexed.snippet_line_start = Some(sls);
                    indexed.snippet_line_end = Some(sle);
                    indexed.byte_start = Some(start);
                    indexed.byte_end = Some(end);
                    indexed.line_start = Some(ls);
                    indexed.line_end = Some(le);
                    indexed.focus_byte = Some(focus);
                    indexed.focus_line = Some(fl);
                    indexed
                },
            )
            .collect();
        let targets: Vec<_> = hits.iter().map(|_| HitTarget::File(0)).collect();
        let sources = [Some(Ok(source.as_bytes().to_vec()))];
        assert_eq!(
            classify_sources(&hits, &targets, &sources, None),
            [
                HitSource::Bound,
                HitSource::Bound,
                HitSource::Bound,
                HitSource::IndexedRangeMismatch
            ],
        );
    }

    fn hit(snippet: &str) -> api::SearchHit {
        serde_json::from_value(serde_json::json!({
            "domainId": "code", "id": "old-index-hit", "score": 0.9,
            "codebaseId": "A", "path": "source.rs",
            "lineStart": 1, "lineEnd": 1, "byteStart": 0, "byteEnd": snippet.len(),
            "focusLine": 1, "focusByte": 3,
            "snippetLineStart": 1, "snippetLineEnd": 1,
            "snippetByteStart": 0, "snippetByteEnd": snippet.len(),
            "snippet": snippet, "symbol": "old_name", "language": "rust"
        }))
        .unwrap()
    }

    fn ready(source: &str) -> Reply {
        Reply::ok(&serde_json::json!({
            "path": "source.rs", "contentHash": blake3::hash(source.as_bytes()).to_hex().to_string(),
            "content": &source[..1], "encoding": "utf-8", "isBinary": false,
            "size": source.len(), "byteStart": 0, "byteEnd": 1, "truncated": true
        }).to_string())
    }

    async fn check(root: &Path, hits: &Arc<Vec<api::SearchHit>>, stub: &Stub) -> Vec<HitSource> {
        let client = Client::for_test_server(&stub.url).with_codebase("A".into());
        hit_sources_in_context(
            &client,
            hits,
            HitContext::search(Some(root), Some("A"), true),
        )
        .await
    }

    #[tokio::test]
    async fn pending_source_cannot_be_validated_by_a_current_catalog_hash() {
        let root = tempfile::tempdir().unwrap();
        let current = "fn new_name() {}\n";
        std::fs::write(root.path().join("source.rs"), current).unwrap();
        let catalog = serde_json::json!([{
            "path": "source.rs", "size": current.len(),
            "contentHash": blake3::hash(current.as_bytes()).to_hex().to_string()
        }]);
        let pending = Reply::Answer {
            status: "409 Conflict", headers: Vec::new(),
            body: r#"{"success":false,"errors":[{"code":"FileNotIndexed","message":"file source not indexed","details":{"status":"Pending"}}]}"#.into(),
        };
        let stub = Stub::serve(vec![pending, Reply::ok(&catalog.to_string())]).await;
        let hits = Arc::new(vec![hit("fn old_name() {}")]);
        let states = check(root.path(), &hits, &stub).await;
        assert!(
            matches!(&states[0], HitSource::Unverified(reason) if reason.contains("FileNotIndexed") && reason.contains("Pending")),
            "{states:?}"
        );
        let rendered = render_hits_inner(
            &hits,
            "none",
            HitContext::search(Some(root.path()), Some("A"), true),
            true,
            &states,
            true,
        );
        assert!(
            rendered.contains("FileNotIndexed") && rendered.contains("Pending"),
            "{rendered}"
        );
        assert!(rendered.contains("source.rs:1-1") && rendered.contains("fn old_name() {}"));
        assert!(!rendered.contains(current.trim()));
        assert_eq!(
            stub.requests(),
            1,
            "catalog hash must never certify a pending file"
        );
        assert!(
            stub.request_lines()[0].contains("/files/content?path=source.rs&byteStart=0&byteEnd=1")
        );
    }

    #[tokio::test]
    async fn ready_current_blob_does_not_certify_an_old_hit_at_wrong_coordinates() {
        let root = tempfile::tempdir().unwrap();
        let old = "fn old_name() {}";
        // The old name still exists: never search for it to relocate the hit.
        let current = format!("// inserted line\n{old}\n");
        std::fs::write(root.path().join("source.rs"), &current).unwrap();
        let stub = Stub::serve(vec![ready(&current)]).await;
        let hits = Arc::new(vec![hit(old), hit("// inserted line")]);
        let states = check(root.path(), &hits, &stub).await;
        assert_eq!(states[0], HitSource::IndexedRangeMismatch);
        assert_eq!(states[1], HitSource::Bound);
        assert_eq!(
            stub.requests(),
            1,
            "same-file hits share a hash and local read"
        );
        let rendered = render_hits_inner(
            &hits,
            "none",
            HitContext::search(Some(root.path()), Some("A"), true),
            true,
            &states,
            true,
        );
        let sections: Vec<_> = rendered.split("\n### ").skip(1).collect();
        assert!(sections[0].contains(old));
        assert!(sections[0].contains("source.rs:1-1"));
    }

    #[tokio::test]
    async fn changed_bytes_and_unavailable_bytes_are_distinct() {
        let root = tempfile::tempdir().unwrap();
        let indexed = "fn old_name() {}";
        std::fs::write(root.path().join("source.rs"), "fn local_edit() {}").unwrap();
        let stub = Stub::serve(vec![ready(indexed)]).await;
        let hits = Arc::new(vec![hit(indexed)]);
        let changed = check(root.path(), &hits, &stub).await;
        assert_eq!(changed, [HitSource::ChangedLocal]);
        std::fs::remove_file(root.path().join("source.rs")).unwrap();
        let missing = check(root.path(), &hits, &stub).await;
        assert!(matches!(missing[0], HitSource::Unverified(_)));
        let gateway = Stub::serve(vec![Reply::gateway("502 Bad Gateway")]).await;
        let failed = check(root.path(), &hits, &gateway).await;
        assert!(matches!(&failed[0], HitSource::Unverified(reason) if reason.contains("502")));
        for states in [changed, missing, failed] {
            let text = render_hits_inner(
                &hits,
                "none",
                HitContext::search(Some(root.path()), Some("A"), true),
                true,
                &states,
                true,
            );
            assert!(text.contains(indexed) && text.contains("source.rs:1-1"));
        }
    }

    #[tokio::test]
    async fn source_checks_do_not_use_a_local_root_for_another_codebase() {
        let root = tempfile::tempdir().unwrap();
        let mut remote = hit("fn old_name() {}");
        remote.codebase_id = Some("B".into());
        let stub = Stub::serve(vec![ready("fn old_name() {}")]).await;
        let states = check(root.path(), &Arc::new(vec![remote]), &stub).await;
        assert!(matches!(states[0], HitSource::Unverified(_)));
        assert_eq!(stub.requests(), 0);
    }

    #[test]
    fn matching_snippet_does_not_certify_wrong_or_incomplete_location_metadata() {
        let source = "fn old_name() {}";
        let mut indexed = hit(source);
        assert_eq!(bind_hit(&indexed, source.as_bytes()), HitSource::Bound);
        indexed.line_start = Some(2);
        assert_eq!(
            bind_hit(&indexed, source.as_bytes()),
            HitSource::IndexedRangeMismatch
        );
        indexed.line_start = Some(1);
        indexed.focus_line = Some(2);
        assert_eq!(
            bind_hit(&indexed, source.as_bytes()),
            HitSource::IndexedRangeMismatch
        );
        indexed.focus_line = Some(1);
        indexed.snippet_byte_start = None;
        assert!(matches!(
            bind_hit(&indexed, source.as_bytes()),
            HitSource::Unverified(_)
        ));
    }

    #[test]
    fn declaration_range_and_focus_can_extend_beyond_a_short_snippet() {
        let source = "#[allow(dead_code)]\nfn example() {\n    work();\n}\n";
        let mut indexed = hit("#[allow(dead_code)]");
        indexed.byte_end = Some(u32::try_from(source.len()).unwrap());
        indexed.line_end = Some(4);
        indexed.focus_byte = Some(u32::try_from(source.find("example").unwrap()).unwrap());
        indexed.focus_line = Some(2);
        assert_eq!(bind_hit(&indexed, source.as_bytes()), HitSource::Bound);
        indexed.focus_byte = Some(u32::try_from(source.len()).unwrap());
        indexed.focus_line = Some(5);
        assert_eq!(
            bind_hit(&indexed, source.as_bytes()),
            HitSource::IndexedRangeMismatch
        );
    }

    #[test]
    fn reference_location_is_independent_of_expanded_enclosing_snippet_bounds() {
        let source = "fn example() {\n    work();\n}\n";
        let mut indexed = hit(source);
        indexed.snippet_line_end = Some(3);
        indexed.byte_start = Some(u32::try_from(source.find("work").unwrap()).unwrap());
        indexed.byte_end = indexed.byte_start.map(|start| start + 4);
        indexed.line_start = Some(2);
        indexed.line_end = Some(2);
        indexed.focus_byte = indexed.byte_start;
        indexed.focus_line = Some(2);
        assert_eq!(bind_hit(&indexed, source.as_bytes()), HitSource::Bound);
    }

    #[test]
    fn strong_hash_verification_cannot_escape_the_selected_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        let source = "fn old_name() {}";
        std::fs::write(directory.path().join("outside.rs"), source).unwrap();
        let content = serde_json::from_value(serde_json::json!({
            "path": "../outside.rs", "contentHash": blake3::hash(source.as_bytes()).to_hex().to_string(),
            "content": "f", "encoding": "utf-8", "isBinary": false,
            "size": source.len(), "byteStart": 0, "byteEnd": 1, "truncated": true
        })).unwrap();
        assert!(matches!(
            local_source(&root, "../outside.rs", &content),
            Err(LocalSourceError::Unverified(_))
        ));
        assert!(verified_local_window(&root, "../outside.rs", &content).is_none());
    }
}
