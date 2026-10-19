//! Optional native-inspector projection. Plain text remains the query's output.

use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

use crate::client::{Client, api};
use crate::engine::CoordinatorStatus;

use super::render::{HitContext, strip_verbatim_prefix};
use super::source::HitSource;

mod graph;
mod overview;
pub(super) use graph::{GraphInput, project_graph};
pub(super) use overview::project_overview;

const MAX_DISPLAY_BYTES: usize = 32 * 1024;
const MAX_DISPLAY_HITS: usize = 32;

/// Native presentation is opt-in; command-line queries only produce plain text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryPresentation {
    Plain,
    Inspector,
}

/// One owner for model-facing text and its optional bounded presentation data.
#[derive(Debug, Serialize)]
pub struct QueryOutput {
    pub text: String,
    pub(crate) display: Option<Value>,
}

impl QueryOutput {
    pub(super) fn new(text: String, display: Option<Value>) -> Self {
        Self { text, display }
    }

    /// Finalize search freshness after the query without bypassing the display cap.
    pub(crate) fn set_index_warning(&mut self, warning: Option<String>) {
        if let Some(mut display) = self.display.take() {
            display["indexWarning"] = warning.map_or(Value::Null, Value::String);
            let mut limit = BoundedWriter {
                inner: io::sink(),
                written: 0,
                max: MAX_DISPLAY_BYTES,
            };
            if serde_json::to_writer(&mut limit, &display).is_ok() {
                self.display = Some(display);
            }
        }
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
    Graph {
        #[serde(flatten)]
        graph: graph::Graph<'a>,
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
    enclosing_symbol: Option<&'a str>,
    is_write: Option<bool>,
    reference_namespace: Option<&'a str>,
    reference_kind: Option<&'a str>,
    external_target: Option<&'a str>,
    project: Option<&'a str>,
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
            enclosing_symbol: hit.enclosing_symbol.as_deref(),
            is_write: hit.is_write,
            reference_namespace: hit.reference_namespace.as_deref(),
            reference_kind: hit.reference_kind.as_deref(),
            external_target: hit.external_target.as_deref(),
            project: None,
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

    fn graph(hit: &'a api::SearchHit) -> Self {
        Self {
            score: None,
            ..Self::new(hit, None)
        }
    }

    fn symbol_search(hit: &'a api::SymbolSearchHit) -> Self {
        Self {
            path: Some(&hit.path),
            symbol: Some(&hit.name),
            qualified_name: Some(&hit.qualified_name),
            enclosing_symbol: None,
            is_write: None,
            reference_namespace: None,
            reference_kind: None,
            external_target: None,
            project: hit.project.as_deref(),
            language: Some(&hit.language),
            kind: Some(&hit.kind),
            score: Some(f64::from(hit.score)),
            start_line: Some(hit.line_start),
            end_line: Some(hit.line_end),
            start_byte: Some(hit.byte_start),
            end_byte: Some(hit.byte_end),
            snippet: None,
            snippet_start_line: None,
            snippet_end_line: None,
            snippet_start_byte: None,
            snippet_end_byte: None,
            focus_line: None,
            focus_byte: None,
            codebase_id: Some(&hit.codebase_id),
            binding: Binding {
                state: "not-probed",
                reason: None,
            },
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

/// Enforce the same encoded-byte cap during construction and late metadata additions.
struct BoundedWriter<W> {
    inner: W,
    written: usize,
    max: usize,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.max.saturating_sub(self.written) {
            return Err(io::Error::other("display exceeds byte limit"));
        }
        let written = self.inner.write(bytes)?;
        self.written += written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn bounded(display: &Display<'_>) -> Option<Value> {
    let mut bytes = BoundedWriter {
        inner: Vec::new(),
        written: 0,
        max: MAX_DISPLAY_BYTES,
    };
    serde_json::to_writer(&mut bytes, display).ok()?;
    serde_json::from_slice(&bytes.inner).ok()
}

// String byte counts reject oversized input before allocating hit vectors or
// scanning strings for JSON escaping. The bounded writer enforces the exact cap.
fn search_hit_bytes(hit: &api::SearchHit) -> usize {
    [
        hit.path.as_deref(),
        hit.symbol.as_deref(),
        hit.qualified_symbol.as_deref(),
        hit.enclosing_symbol.as_deref(),
        hit.reference_namespace.as_deref(),
        hit.reference_kind.as_deref(),
        hit.external_target.as_deref(),
        hit.language.as_deref(),
        hit.codebase_id.as_deref(),
        Some(hit.kind.as_str()),
        Some(hit.snippet.as_str()),
    ]
    .into_iter()
    .flatten()
    .fold(0usize, |total, text| total.saturating_add(text.len()))
}

fn selected_copy(client: &Client, codebase: Option<&str>) -> bool {
    let selected = client.codebase_raw();
    selected.is_some() && codebase.is_none_or(|id| Some(id) == selected)
}

pub(super) fn search(
    client: &Client,
    query: &str,
    hits: &[api::SearchHit],
    sources: &[HitSource],
    context: HitContext<'_>,
) -> Option<Value> {
    if hits.len() > MAX_DISPLAY_HITS
        || hits.iter().fold(query.len(), |total, hit| {
            total.saturating_add(search_hit_bytes(hit))
        }) > MAX_DISPLAY_BYTES
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

pub(super) fn lookup(client: &Client, symbol: &str, hits: &[api::SearchHit]) -> Option<Value> {
    if hits.is_empty()
        || hits.len() > MAX_DISPLAY_HITS
        || hits.iter().fold(symbol.len(), |total, hit| {
            total.saturating_add(search_hit_bytes(hit))
        }) > MAX_DISPLAY_BYTES
    {
        return None;
    }
    bounded(&Display {
        version: 1,
        identity: Identity::new(
            client,
            hits.iter()
                .all(|hit| selected_copy(client, hit.codebase_id.as_deref())),
        ),
        data: Data::Batch {
            symbols: vec![Symbol {
                symbol,
                hits: hits.iter().map(Hit::graph).collect(),
            }],
        },
    })
}

pub(super) fn symbol_search(
    client: &Client,
    query: &str,
    hits: &[api::SymbolSearchHit],
) -> Option<Value> {
    if hits.is_empty() || hits.len() > MAX_DISPLAY_HITS {
        return None;
    }
    let bytes = hits.iter().fold(query.len(), |total, hit| {
        [
            Some(hit.codebase_id.as_str()),
            Some(hit.path.as_str()),
            Some(hit.name.as_str()),
            Some(hit.qualified_name.as_str()),
            Some(hit.kind.as_str()),
            hit.project.as_deref(),
            Some(hit.language.as_str()),
        ]
        .into_iter()
        .flatten()
        .fold(total, |total, text| total.saturating_add(text.len()))
    });
    if bytes > MAX_DISPLAY_BYTES {
        return None;
    }
    bounded(&Display {
        version: 1,
        identity: Identity::new(
            client,
            hits.iter()
                .all(|hit| selected_copy(client, Some(&hit.codebase_id))),
        ),
        data: Data::Batch {
            symbols: vec![Symbol {
                symbol: query,
                hits: hits.iter().map(Hit::symbol_search).collect(),
            }],
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
    let bytes = symbols.iter().fold(0usize, |total, symbol| {
        symbol
            .hits
            .iter()
            .fold(total.saturating_add(symbol.symbol.len()), |total, hit| {
                total.saturating_add(search_hit_bytes(hit))
            })
    });
    if bytes > MAX_DISPLAY_BYTES {
        return None;
    }
    let local = symbols
        .iter()
        .flat_map(|symbol| &symbol.hits)
        .all(|hit| selected_copy(client, hit.codebase_id.as_deref()));
    bounded(&Display {
        version: 1,
        identity: Identity::new(client, local),
        data: Data::Batch {
            symbols: symbols
                .iter()
                .map(|symbol| Symbol {
                    symbol: &symbol.symbol,
                    hits: symbol.hits.iter().map(Hit::graph).collect(),
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
mod tests;
