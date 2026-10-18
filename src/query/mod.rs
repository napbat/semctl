//! Tool bodies. Each function takes the shared [`Client`] + typed args,
//! hits one REST endpoint, and renders the response as the text an MCP
//! host shows the model. A failed call returns a [`ToolError`], not text, so
//! every MCP tool and CLI command shares one typed failure surface.
//!
//! The code/graph endpoints are codebase-scoped: the MCP server is launched
//! for one codebase (`SEMCTX_CODEBASE`), and these build
//! `/v1/codebases/{id}/…` paths from it. `search` / `list_domains` don't need
//! a codebase.

use std::fmt::Write;

use crate::client::{Client, api};

mod advanced;
mod display;
mod error;
mod inspection;
mod render;
mod source;

pub use display::{QueryOutput, QueryPresentation};
pub use error::{FailureKind, ToolError};

#[cfg(test)]
use inspection::human_bytes;
pub use inspection::{
    batch_lookup, external_links, file_tree, imports, list_domains, list_files, list_projects,
    symbol_at_position, symbol_edges, sync_status,
};
pub(crate) use source::{HitSource, hit_sources};

pub use advanced::*;

pub use render::render_projects;
pub(crate) use render::render_source_diagnostic;
use render::{
    HitContext, hit_location, local_path, markdown_code, render_boundaries, render_compact,
    render_hits, render_hits_inner, render_outline,
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
    /// Server scope lens (`local`, `personal`, `organization`, `global`).
    pub scope: Option<String>,
    /// Explicit authorized codebase ids. Suppresses the current-codebase lens.
    pub codebase_ids: Vec<String>,
}

impl SearchOpts {
    pub(crate) fn normalized_scope(&self) -> Result<Option<&'static str>, ToolError> {
        let scope = self
            .scope
            .as_deref()
            .map(|scope| {
                normalize_scope(scope).ok_or_else(|| {
                    ToolError::new(
                        "search_codebase",
                        FailureKind::InvalidArgument,
                        "scope must be local, personal, organization, or global",
                    )
                })
            })
            .transpose()?;
        if scope.is_some() && !self.codebase_ids.is_empty() {
            return Err(ToolError::new(
                "search_codebase",
                FailureKind::InvalidArgument,
                "scope and codebase_ids are mutually exclusive",
            ));
        }
        Ok(scope)
    }
}

/// Cross-domain search, scoped to the launched codebase when one is set. The
/// server applies the kind filter, ranking bias, and symbol granularity; the
/// client sends the options, checks source binding, and renders.
pub async fn search(
    client: &Client,
    query: &str,
    top_k: u32,
    domains: &[String],
    opts: &SearchOpts,
    presentation: QueryPresentation,
) -> Result<QueryOutput, ToolError> {
    let scope = opts.normalized_scope()?;
    let body = api::SearchRequestBody {
        query,
        top_k,
        codebase_id: (opts.scope.is_none() && opts.codebase_ids.is_empty())
            .then(|| client.codebase().ok())
            .flatten(),
        codebase_ids: (!opts.codebase_ids.is_empty()).then(|| opts.codebase_ids.clone()),
        scope: scope.map(str::to_string),
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
    let hits = client
        .post_read::<_, Vec<api::SearchHit>>("/v1/search", &body)
        .await
        .map_err(|e| ToolError::from_client("search_codebase", &e))?;
    let hits = std::sync::Arc::new(hits);

    // Only exact source bytes and coordinates can bind an indexed hit locally.
    let context = HitContext::search(
        client.local_root(),
        client.codebase_raw(),
        opts.scope.is_none() && opts.codebase_ids.is_empty() && client.codebase_raw().is_some(),
    );
    if hits.is_empty() {
        return Ok(QueryOutput::new(
            "no results".to_string(),
            if presentation == QueryPresentation::Inspector {
                display::search(client, query, &hits, &[], context)
            } else {
                None
            },
        ));
    }
    let sources = source::hit_sources_in_context(client, &hits, context).await;
    let text = render_hits_inner(&hits, "no results", context, true, &sources, opts.expand);
    let display = if presentation == QueryPresentation::Inspector {
        display::search(client, query, &hits, &sources, context)
    } else {
        None
    };
    Ok(QueryOutput::new(text, display))
}

fn normalize_scope(scope: &str) -> Option<&'static str> {
    match scope.trim().to_ascii_lowercase().as_str() {
        "local" => Some("Local"),
        "personal" => Some("Personal"),
        "organization" | "organisation" | "org" => Some("Organization"),
        "global" => Some("Global"),
        _ => None,
    }
}

/// The codebase of `client`, or the `op` failure that says why none is bound.
fn bound_codebase<'a>(client: &'a Client, op: &'static str) -> Result<&'a str, ToolError> {
    client
        .codebase()
        .map_err(|error| ToolError::from_client(op, &error))
}

/// One-shot symbol neighbourhood (#4): the definition of `symbol` plus its
/// direct callers and callees. The server's `/graph/trace` endpoint composes and
/// classifies the neighbourhood next to the graph; the client renders it.
pub async fn trace(client: &Client, symbol: &str, depth: u32) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "trace")?;
    let url = format!(
        "/v1/codebases/{cb}/graph/trace?symbol={}&depth={depth}",
        urlencode(symbol)
    );
    let t = client
        .get::<api::TraceResult>(&url)
        .await
        .map_err(|e| ToolError::from_client("trace", &e))?;
    if t.definition.is_empty() {
        return Ok(near_miss(client, symbol, "definition").await);
    }
    let root = client.local_root();

    let mut out = format!("## Definition of {}\n\n", markdown_code(symbol));
    out.push_str(&render_hits(&t.definition, "", root, false));

    write!(out, "\n## Callers ({})\n\n", t.callers.len()).expect("writing to a String cannot fail");
    out.push_str(&render_compact(&t.callers, "  (none)", root));
    write!(out, "\n## Callees ({})\n\n", t.callees.len()).expect("writing to a String cannot fail");
    out.push_str(&render_compact(&t.callees, "  (none)", root));
    Ok(out)
}

/// Symbol-graph: definitions of `symbol` in the codebase.
pub async fn find_definition(client: &Client, symbol: &str) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "find_definition")?;
    let path = format!(
        "/v1/codebases/{cb}/graph/definitions?symbol={}",
        urlencode(symbol)
    );
    let hits = client
        .get::<Vec<api::SearchHit>>(&path)
        .await
        .map_err(|e| ToolError::from_client("find_definition", &e))?;
    if hits.is_empty() {
        return Ok(near_miss(client, symbol, "definition").await);
    }
    Ok(render_hits(&hits, "", client.local_root(), false))
}

/// Symbol-graph: references to `symbol` in the codebase.
pub async fn find_references(
    client: &Client,
    symbol: &str,
    namespace: Option<&str>,
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "find_references")?;
    let mut path = format!(
        "/v1/codebases/{cb}/graph/references?symbol={}",
        urlencode(symbol)
    );
    if let Some(namespace) = namespace.filter(|value| !value.is_empty()) {
        write!(path, "&referenceNamespace={}", urlencode(namespace))
            .expect("writing to a String cannot fail");
    }
    let hits = client
        .get::<Vec<api::SearchHit>>(&path)
        .await
        .map_err(|e| ToolError::from_client("find_references", &e))?;
    if hits.is_empty() {
        return Ok(near_miss(client, symbol, "references").await);
    }
    Ok(render_hits(&hits, "", client.local_root(), false))
}

/// Graph: incoming callers of `symbol` — the definitions that call it (the
/// inverse `calls` edge).
pub async fn who_calls(client: &Client, symbol: &str) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "who_calls")?;
    let path = format!(
        "/v1/codebases/{cb}/graph/who-calls?symbol={}",
        urlencode(symbol)
    );
    let hits = client
        .get::<Vec<api::SearchHit>>(&path)
        .await
        .map_err(|e| ToolError::from_client("who_calls", &e))?;
    Ok(render_hits(
        &hits,
        &format!("nothing calls `{symbol}`"),
        client.local_root(),
        false,
    ))
}

/// Graph: the types implementing `symbol` (a trait/interface) — the reverse
/// `implements` edge.
pub async fn implementations_of(client: &Client, symbol: &str) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "implementations_of")?;
    let path = format!(
        "/v1/codebases/{cb}/graph/implementations?symbol={}",
        urlencode(symbol)
    );
    let hits = client
        .get::<Vec<api::SearchHit>>(&path)
        .await
        .map_err(|e| ToolError::from_client("implementations_of", &e))?;
    Ok(render_hits(
        &hits,
        &format!("no implementations of `{symbol}`"),
        client.local_root(),
        false,
    ))
}

/// Graph: a shortest call chain from `from` to `to` — the chunks along one path
/// of `calls` edges, in order.
pub async fn call_path(client: &Client, from: &str, to: &str) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "call_path")?;
    let path = format!(
        "/v1/codebases/{cb}/graph/call-path?from={}&to={}",
        urlencode(from),
        urlencode(to)
    );
    let hits = client
        .get::<Vec<api::SearchHit>>(&path)
        .await
        .map_err(|e| ToolError::from_client("call_path", &e))?;
    Ok(render_hits(
        &hits,
        &format!("no call path from `{from}` to `{to}`"),
        client.local_root(),
        false,
    ))
}

/// Flow: the external boundaries a value entering from `from` flows out to
/// (forward, inter-procedural value flow). Boundaries are the un-indexed callees
/// the corpus crosses.
pub async fn reaches(client: &Client, from: &str) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "reaches")?;
    let path = format!(
        "/v1/codebases/{cb}/graph/flow/reaches?from={}",
        urlencode(from)
    );
    let boundaries = client
        .get::<Vec<String>>(&path)
        .await
        .map_err(|e| ToolError::from_client("reaches", &e))?;
    Ok(render_boundaries(
        &boundaries,
        &format!("no boundary flow reached from `{from}`"),
    ))
}

/// Flow: the external boundaries whose entering value reaches `to` (backward,
/// inter-procedural value flow) — the dual of [`reaches`].
pub async fn flows_into(client: &Client, to: &str) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "flows_into")?;
    let path = format!("/v1/codebases/{cb}/graph/flow/into?to={}", urlencode(to));
    let boundaries = client
        .get::<Vec<String>>(&path)
        .await
        .map_err(|e| ToolError::from_client("flows_into", &e))?;
    Ok(render_boundaries(
        &boundaries,
        &format!("nothing flows into `{to}`"),
    ))
}

/// Flow: the function chunks a value flows through from boundary `from` to
/// boundary `to` — the inter-procedural flow witness.
pub async fn flows_between(client: &Client, from: &str, to: &str) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "flows_between")?;
    let path = format!(
        "/v1/codebases/{cb}/graph/flow/between?from={}&to={}",
        urlencode(from),
        urlencode(to)
    );
    let hits = client
        .get::<Vec<api::SearchHit>>(&path)
        .await
        .map_err(|e| ToolError::from_client("flows_between", &e))?;
    Ok(render_hits(
        &hits,
        &format!("no value flow from `{from}` to `{to}`"),
        client.local_root(),
        false,
    ))
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
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "grep")?;
    let mut url = format!(
        "/v1/codebases/{cb}/grep?pattern={}&regex={regex}&ignoreCase={ignore_case}&max={max}",
        urlencode(pattern)
    );
    if let Some(p) = path.filter(|p| !p.is_empty()) {
        write!(url, "&path={}", urlencode(p)).expect("writing to a String cannot fail");
    }
    let matches = client
        .get::<Vec<api::GrepMatch>>(&url)
        .await
        .map_err(|e| ToolError::from_client("grep", &e))?;
    if matches.is_empty() {
        return Ok(format!("no matches for `{pattern}`"));
    }
    let root = client.local_root();
    let mut out = String::new();
    if let Some(file) = single_file_target(path, &matches)
        && root.is_some_and(|root| root.join(file).is_file())
    {
        out.push_str(
            "note: for one local file, host Grep is current; this searched the indexed snapshot.\n",
        );
    }
    for m in &matches {
        writeln!(
            out,
            "{}:{}: {}",
            local_path(root, &m.path),
            m.line_number,
            m.line.trim_end()
        )
        .expect("writing to a String cannot fail");
    }
    writeln!(
        out,
        "({} match{})",
        matches.len(),
        if matches.len() == 1 { "" } else { "es" }
    )
    .expect("writing to a String cannot fail");
    Ok(out)
}

/// The one file a grep targeted: `path` names a file (not a directory), and
/// every match is in one file whose path ends with it.
fn single_file_target<'a>(path: Option<&str>, matches: &'a [api::GrepMatch]) -> Option<&'a str> {
    let filter = path.filter(|p| !p.is_empty() && !p.ends_with('/'))?;
    let file = matches.first()?.path.as_str();
    (file.ends_with(filter) && matches.iter().all(|m| m.path == file)).then_some(file)
}

/// A file's table of contents — every indexed chunk (kind, symbol, line range)
/// in source order. Cheaper than reading the file when you only want its shape.
pub async fn file_outline(
    client: &Client,
    path: &str,
    max_depth: Option<u32>,
    kinds: &[String],
    include_body: bool,
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "file_outline")?;
    let mut url = format!(
        "/v1/codebases/{cb}/graph/outline?path={}&includeBody={include_body}",
        urlencode(path)
    );
    if let Some(depth) = max_depth {
        write!(url, "&maxDepth={depth}").expect("writing to a String cannot fail");
    }
    for kind in kinds {
        write!(url, "&kinds={}", urlencode(kind)).expect("writing to a String cannot fail");
    }
    let outline = client
        .get::<api::FileOutline>(&url)
        .await
        .map_err(|e| ToolError::from_client("file_outline", &e))?;
    if outline.entries.is_empty() {
        return Ok(format!(
            "{} is indexed but has no chunks",
            markdown_code(path)
        ));
    }
    Ok(render_outline(&outline, client.local_root()))
}

/// Chunks overlapping an inclusive line range in a file — "grow context around
/// a hit". Pass a search/definition hit's line range (widened to taste) to pull
/// the neighbouring chunks in source order.
pub async fn expand_chunk(
    client: &Client,
    path: &str,
    line_start: u32,
    line_end: u32,
) -> Result<String, ToolError> {
    let cb = bound_codebase(client, "expand_chunk")?;
    let url = format!(
        "/v1/codebases/{cb}/graph/expand?path={}&lineStart={line_start}&lineEnd={line_end}",
        urlencode(path)
    );
    let hits = client
        .get::<Vec<api::SearchHit>>(&url)
        .await
        .map_err(|e| ToolError::from_client("expand_chunk", &e))?;
    Ok(render_hits(
        &hits,
        &format!("no indexed chunks overlap {path}:{line_start}-{line_end}"),
        client.local_root(),
        false,
    ))
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
        codebase_ids: None,
        scope: None,
        domains: None,
        filters: None,
        kinds: None,
        prefer: None,
        granularity: None,
    };
    let Ok(hits) = client
        .post_read::<_, Vec<api::SearchHit>>("/v1/search", &body)
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
            writeln!(out, "  `{sym}`  {}", hit_location(h, root))
                .expect("writing to a String cannot fail");
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
            _ => write!(out, "%{b:02X}").expect("writing to a String cannot fail"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{SearchOpts, bound_codebase, human_bytes, normalize_prefer, single_file_target};
    use crate::client::{Client, api};

    fn at(path: &str) -> api::GrepMatch {
        api::GrepMatch {
            path: path.into(),
            line_number: 1,
            line: String::new(),
        }
    }

    #[test]
    fn a_grep_is_single_file_only_when_its_path_names_the_one_matched_file() {
        let one = [
            at("crates/a/src/simplify.rs"),
            at("crates/a/src/simplify.rs"),
        ];
        assert_eq!(
            single_file_target(Some("a/src/simplify.rs"), &one),
            Some("crates/a/src/simplify.rs")
        );
        // A directory filter, no filter, or matches in two files are broad.
        assert_eq!(single_file_target(Some("crates/a/"), &one), None);
        assert_eq!(single_file_target(Some("crates/a/src"), &one), None);
        assert_eq!(single_file_target(None, &one), None);
        let two = [at("x/simplify.rs"), at("y/simplify.rs")];
        assert_eq!(single_file_target(Some("simplify.rs"), &two), None);
    }

    const CORRECT_THE_ARGUMENT: &str = "next: correct the argument and call again.";

    #[test]
    fn an_unknown_search_scope_is_an_invalid_argument() {
        let opts = SearchOpts {
            scope: Some("typo".into()),
            ..SearchOpts::default()
        };
        let message = opts.normalized_scope().unwrap_err().to_string();
        assert!(message.starts_with("search_codebase failed: scope must be"));
        assert!(message.ends_with(CORRECT_THE_ARGUMENT));
    }

    #[test]
    fn a_scope_with_explicit_codebase_ids_is_an_invalid_argument() {
        let opts = SearchOpts {
            scope: Some("local".into()),
            codebase_ids: vec!["A".into()],
            ..SearchOpts::default()
        };
        let message = opts.normalized_scope().unwrap_err().to_string();
        assert!(message.contains("mutually exclusive"));
        assert!(message.ends_with(CORRECT_THE_ARGUMENT));
    }

    #[test]
    fn a_missing_codebase_binding_names_the_failed_operation() {
        let client = Client::for_test("", None);
        let message = bound_codebase(&client, "trace").unwrap_err().to_string();
        assert!(message.starts_with("trace failed: no codebase for this directory"));
        assert!(message.ends_with("next: use local Read/Grep for this request."));
    }

    #[test]
    fn total_source_bytes_are_human_readable_without_losing_exactness() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn normalize_prefer_maps_to_server_enum_names() {
        assert_eq!(normalize_prefer("code"), Some("Code"));
        assert_eq!(normalize_prefer("  CODE "), Some("Code"));
        assert_eq!(normalize_prefer("docs"), Some("Docs"));
        assert_eq!(normalize_prefer("doc"), Some("Docs"));
        assert_eq!(normalize_prefer("nonsense"), None);
    }
}
