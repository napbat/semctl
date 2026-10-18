//! Optional native-inspector projection. Plain text remains the query's output.

use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

use crate::client::{Client, api};
use crate::engine::CoordinatorStatus;

use super::render::{HitContext, strip_verbatim_prefix};
use super::source::HitSource;

const MAX_DISPLAY_BYTES: usize = 32 * 1024;
const MAX_DISPLAY_HITS: usize = 32;

/// Native presentation is opt-in; command-line queries only produce plain text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryPresentation {
    Plain,
    Inspector,
}

/// One owner for model-facing text and its optional bounded presentation data.
#[derive(Debug)]
pub struct QueryOutput {
    pub text: String,
    pub(crate) display: Option<Value>,
}

impl QueryOutput {
    pub(super) fn new(text: String, display: Option<Value>) -> Self {
        Self { text, display }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Identity<'a> {
    codebase_id: Option<&'a str>,
    name: Option<&'a str>,
    local_root: Option<String>,
    copy: &'static str,
}

impl<'a> Identity<'a> {
    fn new(client: &'a Client, expose_root: bool) -> Self {
        Self {
            codebase_id: client.codebase_raw(),
            name: None,
            local_root: client
                .local_root()
                .filter(|_| expose_root)
                .map(|root| strip_verbatim_prefix(root.to_string_lossy().into_owned())),
            copy: if client.local_root().is_some() {
                "checkout"
            } else {
                "canonical"
            },
        }
    }
}

#[derive(Serialize)]
struct Display<'a> {
    version: u8,
    identity: Identity<'a>,
    #[serde(flatten)]
    data: Data<'a>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Data<'a> {
    Search {
        query: &'a str,
        hits: Vec<Hit<'a>>,
    },
    Source {
        path: &'a str,
        text: &'a str,
        language: Option<&'a str>,
        #[serde(rename = "sourceOrigin")]
        source_origin: &'a str,
        #[serde(rename = "contentHash")]
        content_hash: &'a str,
        encoding: &'a str,
        binary: bool,
        truncated: bool,
        #[serde(rename = "byteStart")]
        byte_start: i64,
        #[serde(rename = "byteEnd")]
        byte_end: i64,
        #[serde(rename = "fileSize")]
        file_size: i64,
        #[serde(rename = "lineStart")]
        line_start: Option<u32>,
        #[serde(rename = "lineEnd")]
        line_end: Option<u32>,
    },
    Batch {
        symbols: Vec<Symbol<'a>>,
    },
    Status {
        coordinator: Option<&'a CoordinatorStatus>,
        catalog: Option<Catalog>,
        #[serde(rename = "catalogError")]
        catalog_error: Option<String>,
        #[serde(rename = "jobId")]
        job_id: Option<&'a str>,
        job: Option<Job<'a>>,
        #[serde(rename = "jobError")]
        job_error: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Hit<'a> {
    path: Option<&'a str>,
    symbol: Option<&'a str>,
    qualified_name: Option<&'a str>,
    language: Option<&'a str>,
    kind: Option<&'a str>,
    score: Option<f64>,
    start_line: Option<u32>,
    end_line: Option<u32>,
    start_byte: Option<u32>,
    end_byte: Option<u32>,
    snippet: Option<&'a str>,
    snippet_start_line: Option<u32>,
    snippet_end_line: Option<u32>,
    snippet_start_byte: Option<u32>,
    snippet_end_byte: Option<u32>,
    focus_line: Option<u32>,
    focus_byte: Option<u32>,
    codebase_id: Option<&'a str>,
    binding: Binding<'a>,
}

#[derive(Serialize)]
struct Binding<'a> {
    state: &'static str,
    reason: Option<&'a str>,
}

impl<'a> Hit<'a> {
    fn new(hit: &'a api::SearchHit, source: Option<&'a HitSource>) -> Self {
        let binding = match source {
            Some(HitSource::Bound) => Binding {
                state: "bound",
                reason: None,
            },
            Some(HitSource::ChangedLocal) => Binding {
                state: "changed-local",
                reason: source.and_then(HitSource::warning),
            },
            Some(HitSource::IndexedRangeMismatch) => Binding {
                state: "range-mismatch",
                reason: source.and_then(HitSource::warning),
            },
            Some(HitSource::Unverified(reason)) => Binding {
                state: "unverified",
                reason: Some(reason),
            },
            None => Binding {
                state: "not-probed",
                reason: None,
            },
        };
        Self {
            path: hit.path.as_deref(),
            symbol: hit.symbol.as_deref(),
            qualified_name: hit.qualified_symbol.as_deref(),
            language: hit.language.as_deref(),
            kind: (!hit.kind.is_empty()).then_some(hit.kind.as_str()),
            score: hit.score.is_finite().then_some(hit.score),
            start_line: hit.line_start,
            end_line: hit.line_end,
            start_byte: hit.byte_start,
            end_byte: hit.byte_end,
            snippet: Some(&hit.snippet),
            snippet_start_line: hit.snippet_line_start,
            snippet_end_line: hit.snippet_line_end,
            snippet_start_byte: hit.snippet_byte_start,
            snippet_end_byte: hit.snippet_byte_end,
            focus_line: hit.focus_line,
            focus_byte: hit.focus_byte,
            codebase_id: hit.codebase_id.as_deref(),
            binding,
        }
    }
}

#[derive(Serialize)]
struct Symbol<'a> {
    symbol: &'a str,
    hits: Vec<Hit<'a>>,
}

#[derive(Serialize)]
struct Catalog {
    files: u32,
    bytes: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Job<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    files_to_embed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files_to_delete: Option<i64>,
    files_embedded: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    files_deleted: Option<i64>,
    files_failed: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_count: Option<i64>,
    error: Option<&'a str>,
    started_at: Option<&'a str>,
    completed_at: Option<&'a str>,
}

/// Stop serialization at the cap rather than allocating an oversized JSON copy.
struct BoundedBytes(Vec<u8>);

impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_DISPLAY_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("display exceeds byte limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn bounded(display: &Display<'_>) -> Option<Value> {
    let mut bytes = BoundedBytes(Vec::new());
    serde_json::to_writer(&mut bytes, display).ok()?;
    serde_json::from_slice(&bytes.0).ok()
}

pub(super) fn search(
    client: &Client,
    query: &str,
    hits: &[api::SearchHit],
    sources: &[HitSource],
    context: HitContext<'_>,
) -> Option<Value> {
    if query.len() > MAX_DISPLAY_BYTES
        || hits.len() > MAX_DISPLAY_HITS
        || hits.iter().any(|hit| hit.snippet.len() > MAX_DISPLAY_BYTES)
    {
        return None;
    }
    bounded(&Display {
        version: 1,
        identity: Identity::new(
            client,
            hits.iter().all(|hit| context.root_for(hit).is_some()),
        ),
        data: Data::Search {
            query,
            hits: hits
                .iter()
                .enumerate()
                .map(|(index, hit)| Hit::new(hit, sources.get(index)))
                .collect(),
        },
    })
}

pub(super) fn source(
    client: &Client,
    content: &api::FileContent,
    text: &str,
    language: Option<&str>,
    source_origin: &str,
) -> Option<Value> {
    // UTF-8 length is a cheap lower bound on JSON string size. Reject a large
    // body before serde scans it for escaping, not after allocating a Value.
    if text.len() > MAX_DISPLAY_BYTES {
        return None;
    }
    bounded(&Display {
        version: 1,
        identity: Identity::new(client, source_origin == "verified-local"),
        data: Data::Source {
            path: &content.path,
            text,
            language,
            source_origin,
            content_hash: &content.content_hash,
            encoding: &content.encoding,
            binary: content.is_binary,
            truncated: content.truncated,
            byte_start: content.byte_start,
            byte_end: content.byte_end,
            file_size: content.size,
            line_start: content.line_start,
            line_end: content.line_end,
        },
    })
}

pub(super) fn batch(client: &Client, symbols: &[api::SymbolHits]) -> Option<Value> {
    // Even empty symbol rows require 23 JSON bytes each, before separators.
    if symbols.len() > MAX_DISPLAY_BYTES / 23 {
        return None;
    }
    if symbols
        .iter()
        .try_fold(0usize, |total, symbol| total.checked_add(symbol.hits.len()))?
        > MAX_DISPLAY_HITS
    {
        return None;
    }
    if symbols.iter().any(|symbol| {
        symbol.symbol.len() > MAX_DISPLAY_BYTES
            || symbol
                .hits
                .iter()
                .any(|hit| hit.snippet.len() > MAX_DISPLAY_BYTES)
    }) {
        return None;
    }
    let selected = client.codebase_raw();
    let local = selected.is_some()
        && symbols.iter().flat_map(|symbol| &symbol.hits).all(|hit| {
            hit.codebase_id
                .as_deref()
                .is_none_or(|id| Some(id) == selected)
        });
    bounded(&Display {
        version: 1,
        identity: Identity::new(client, local),
        data: Data::Batch {
            symbols: symbols
                .iter()
                .map(|symbol| Symbol {
                    symbol: &symbol.symbol,
                    hits: symbol.hits.iter().map(|hit| Hit::new(hit, None)).collect(),
                })
                .collect(),
        },
    })
}

pub(super) fn status(
    client: &Client,
    checkout: Option<&CoordinatorStatus>,
    totals: &anyhow::Result<(u32, i64)>,
    job: Option<(&str, &anyhow::Result<api::JobStatus>)>,
) -> Option<Value> {
    // Never attach a coordinator root to a canonical or differently-bound copy.
    let coordinator = checkout.filter(|status| {
        status.codebase_id.as_deref() == client.codebase_raw()
            && client.codebase_raw().is_some()
            && client.local_root() == Some(status.root.as_path())
    });
    let job_status = job.and_then(|(_, result)| result.as_ref().ok());
    bounded(&Display {
        version: 1,
        identity: Identity::new(client, coordinator.is_some()),
        data: Data::Status {
            coordinator,
            catalog: totals
                .as_ref()
                .ok()
                .map(|&(files, bytes)| Catalog { files, bytes }),
            catalog_error: totals.as_ref().err().map(ToString::to_string),
            job_id: job.map(|(id, _)| id),
            job: job_status.map(|status| Job {
                files_to_embed: status.files_to_embed,
                files_to_delete: status.files_to_delete,
                files_embedded: status.files_embedded,
                files_deleted: status.files_deleted,
                files_failed: status.files_failed,
                chunk_count: status.chunk_count,
                error: status.error.as_deref(),
                started_at: status.started_at.as_deref(),
                completed_at: status.completed_at.as_deref(),
            }),
            job_error: job
                .and_then(|(_, result)| result.as_ref().err())
                .map(ToString::to_string),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(codebase: Option<&str>, snippet: &str) -> api::SearchHit {
        serde_json::from_value(serde_json::json!({
            "domainId": "code", "id": "hit", "score": 2.0,
            "path": "src/lib.rs", "codebaseId": codebase,
            "lineStart": 8, "lineEnd": 9, "byteStart": 40, "byteEnd": 60,
            "snippet": snippet, "focusLine": 9, "focusByte": 44,
            "snippetLineStart": 8, "snippetLineEnd": 9,
            "snippetByteStart": 40, "snippetByteEnd": 60
        }))
        .unwrap()
    }

    fn content() -> api::FileContent {
        serde_json::from_value(serde_json::json!({
            "path": "src/lib.rs", "contentHash": "pinned", "content": "",
            "encoding": "utf-8", "isBinary": false, "size": 60,
            "byteStart": 40, "byteEnd": 60, "lineStart": 8,
            "lineEnd": 9, "truncated": true
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn search_query_round_trips_exactly_without_extra_reads_or_plain_metadata() {
        use crate::client::stub::{Reply, Stub};
        use crate::query::{QueryPresentation, SearchOpts};

        let query = "  exact \"query\" \\\\ <tag>&[]\r\n\t\x1b[31m café 雪  ";
        for response in [
            "[]",
            r#"[{"domainId":"code","id":"hit","score":1.0,"path":"src/lib.rs","snippet":"fn sample() {}"}]"#,
        ] {
            for presentation in [QueryPresentation::Plain, QueryPresentation::Inspector] {
                let stub = Stub::serve(vec![Reply::ok(response)]).await;
                let client = Client::for_test_server(&stub.url).with_codebase("A".into());
                let output = crate::query::search(
                    &client,
                    query,
                    8,
                    &[],
                    &SearchOpts::default(),
                    presentation,
                )
                .await
                .unwrap();
                match presentation {
                    QueryPresentation::Inspector => {
                        let wire = serde_json::to_vec(&output.display.unwrap()).unwrap();
                        let display: Value = serde_json::from_slice(&wire).unwrap();
                        assert_eq!(display["query"].as_str(), Some(query));
                        assert_eq!(display["kind"], "search");
                    }
                    QueryPresentation::Plain => assert!(output.display.is_none()),
                }
                assert_eq!(stub.requests(), 1);
            }
        }
    }

    #[test]
    fn search_query_counts_toward_the_whole_serialized_display_cap() {
        let client = Client::for_test("A", None);
        let scope = HitContext::search(None, Some("A"), true);
        let hits = [hit(Some("A"), "source")];
        let empty = search(&client, "", &hits, &[], scope).unwrap();
        let overhead = serde_json::to_vec(&empty).unwrap().len();
        let exact = "x".repeat(MAX_DISPLAY_BYTES - overhead);
        let display = search(&client, &exact, &hits, &[], scope).unwrap();
        assert_eq!(display["query"].as_str(), Some(exact.as_str()));
        assert_eq!(
            serde_json::to_vec(&display).unwrap().len(),
            MAX_DISPLAY_BYTES
        );
        assert!(search(&client, &(exact + "x"), &hits, &[], scope).is_none());
        assert!(search(&client, &"x".repeat(MAX_DISPLAY_BYTES + 1), &[], &[], scope).is_none());
        assert!(
            search(
                &client,
                &"\n".repeat(MAX_DISPLAY_BYTES / 2),
                &hits,
                &[],
                scope
            )
            .is_none()
        );
    }

    #[test]
    fn byte_and_hit_caps_are_independent_and_all_or_nothing() {
        let client = Client::for_test("A", None);
        let scope = HitContext::search(None, Some("A"), true);
        let hits: Vec<_> = (0..MAX_DISPLAY_HITS).map(|_| hit(Some("A"), "")).collect();
        assert_eq!(
            search(&client, "query", &hits, &[], scope).unwrap()["hits"]
                .as_array()
                .unwrap()
                .len(),
            32
        );
        let too_many: Vec<_> = (0..=MAX_DISPLAY_HITS).map(|_| hit(Some("A"), "")).collect();
        assert!(search(&client, "query", &too_many, &[], scope).is_none());
        assert!(
            search(
                &client,
                "query",
                &[hit(Some("A"), &"x".repeat(MAX_DISPLAY_BYTES))],
                &[],
                scope
            )
            .is_none()
        );

        let content = content();
        let empty = source(&client, &content, "", Some("rs"), "content-plane").unwrap();
        let overhead = serde_json::to_vec(&empty).unwrap().len();
        let exact = "x".repeat(MAX_DISPLAY_BYTES - overhead);
        let display = source(&client, &content, &exact, Some("rs"), "content-plane").unwrap();
        assert_eq!(
            serde_json::to_vec(&display).unwrap().len(),
            MAX_DISPLAY_BYTES
        );
        assert!(
            source(
                &client,
                &content,
                &(exact + "x"),
                Some("rs"),
                "content-plane"
            )
            .is_none()
        );
        // Escaped and multibyte text count as serialized bytes, not characters.
        assert!(
            source(
                &client,
                &content,
                &"\n".repeat(MAX_DISPLAY_BYTES / 2),
                None,
                "content-plane"
            )
            .is_none()
        );
    }

    #[test]
    fn search_preserves_coordinates_and_binding_without_cross_copy_roots() {
        let root = tempfile::tempdir().unwrap();
        let client = Client::for_test("A", Some(root.path().to_path_buf()));
        let scope = HitContext::search(client.local_root(), Some("A"), true);
        let hits = [hit(Some("A"), "first\nsecond")];
        let bound = search(&client, "query", &hits, &[HitSource::Bound], scope).unwrap();
        assert!(bound["identity"]["localRoot"].is_string());
        assert_eq!(bound["hits"][0]["binding"]["state"], "bound");
        assert_eq!(bound["hits"][0]["startLine"], 8);
        assert_eq!(bound["hits"][0]["focusLine"], 9);
        assert_eq!(bound["hits"][0]["snippetStartByte"], 40);
        let states = [
            (HitSource::ChangedLocal, "changed-local"),
            (HitSource::IndexedRangeMismatch, "range-mismatch"),
            (
                HitSource::Unverified("probe budget exhausted".into()),
                "unverified",
            ),
        ];
        for (state, expected) in states {
            let display = search(&client, "query", &hits, &[state], scope).unwrap();
            assert_eq!(display["hits"][0]["binding"]["state"], expected);
            assert!(display["hits"][0]["binding"]["reason"].is_string());
        }
        let mixed = [hit(Some("A"), ""), hit(Some("B"), "")];
        assert!(
            search(&client, "query", &mixed, &[], scope).unwrap()["identity"]["localRoot"]
                .is_null()
        );
        let legacy = [hit(None, "")];
        let broad = HitContext::search(client.local_root(), Some("A"), false);
        assert!(
            search(&client, "query", &legacy, &[], broad).unwrap()["identity"]["localRoot"]
                .is_null()
        );
        let canonical = client.for_canonical();
        let display = search(
            &canonical,
            "query",
            &hits,
            &[],
            HitContext::search(None, Some("A"), true),
        )
        .unwrap();
        assert_eq!(display["identity"]["copy"], "canonical");
        assert!(display["identity"]["localRoot"].is_null());
        let content = content();
        assert!(source(&client, &content, "canonical bytes", None, "content-plane").unwrap()["identity"]["localRoot"].is_null());
        let local = source(
            &client,
            &content,
            "verified bytes\r\n",
            Some("rs"),
            "verified-local",
        )
        .unwrap();
        assert_eq!(local["text"], "verified bytes\r\n");
        assert_eq!(local["contentHash"], "pinned");
        assert_eq!(local["byteStart"], 40);
        assert!(local["identity"]["localRoot"].is_string());
        let mut incomplete = hit(Some("A"), "");
        incomplete.line_start = None;
        incomplete.snippet_line_start = None;
        incomplete.score = f64::NAN;
        let display = search(&client, "query", &[incomplete], &[], scope).unwrap();
        assert!(display["hits"][0]["startLine"].is_null());
        assert!(display["hits"][0]["snippetStartLine"].is_null());
        assert!(display["hits"][0]["score"].is_null());
    }

    #[test]
    fn batch_retains_misses_order_and_aggregate_bound_without_probing() {
        let client = Client::for_test("A", None);
        let mut symbols = vec![
            api::SymbolHits {
                symbol: "missing".into(),
                hits: vec![],
            },
            api::SymbolHits {
                symbol: "found".into(),
                hits: (0..32).map(|_| hit(Some("A"), "")).collect(),
            },
        ];
        let display = batch(&client, &symbols).unwrap();
        assert_eq!(display["symbols"][0]["symbol"], "missing");
        assert_eq!(display["symbols"][0]["hits"], serde_json::json!([]));
        assert_eq!(
            display["symbols"][1]["hits"][0]["binding"]["state"],
            "not-probed"
        );
        symbols.push(api::SymbolHits {
            symbol: "overflow".into(),
            hits: vec![hit(Some("A"), "")],
        });
        assert!(batch(&client, &symbols).is_none());
    }

    #[test]
    fn status_preserves_unknown_counters_failures_and_snapshot_copy_identity() {
        let root = tempfile::tempdir().unwrap();
        let client = Client::for_test("A", Some(root.path().to_path_buf()));
        let coordinator: CoordinatorStatus = serde_json::from_value(serde_json::json!({
            "root": root.path(), "codebase_id": "A", "leases": 1, "watcher": "active",
            "last_job_id": "job", "running": false, "pending_triggers": 0,
            "trigger_overflow": false, "last_outcome": null, "last_error": "upload failed",
            "first_index": null, "sync_progress": null
        }))
        .unwrap();
        let job = Ok(serde_json::from_value::<api::JobStatus>(serde_json::json!({
            "filesEmbedded": 7, "filesFailed": 2, "error": "embedding failed"
        }))
        .unwrap());
        let totals = Ok((100, 4096));
        let display = status(&client, Some(&coordinator), &totals, Some(("job", &job))).unwrap();
        assert_eq!(display["coordinator"]["last_error"], "upload failed");
        assert_eq!(display["catalog"]["files"], 100);
        assert_eq!(display["job"]["filesEmbedded"], 7);
        assert_eq!(display["job"]["filesFailed"], 2);
        assert!(display["job"].get("filesToEmbed").is_none());
        assert!(display["job"].get("filesDeleted").is_none());
        assert!(display["job"].get("chunkCount").is_none());
        let canonical = status(&client.for_canonical(), Some(&coordinator), &totals, None).unwrap();
        assert!(canonical["coordinator"].is_null());
        assert!(canonical["identity"]["localRoot"].is_null());
        let failed_totals = Err(anyhow::anyhow!("catalog unavailable"));
        let failed_job = Err(anyhow::anyhow!("job unavailable"));
        let display = status(&client, None, &failed_totals, Some(("job", &failed_job))).unwrap();
        assert!(display["catalog"].is_null());
        assert!(display["job"].is_null());
        assert_eq!(display["catalogError"], "catalog unavailable");
        assert_eq!(display["jobError"], "job unavailable");
    }
}
