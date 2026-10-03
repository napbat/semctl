//! Opt-in record of hook decisions for host-integration tests and debugging.
//!
//! When `SEMCTX_HOOK_TRACE` names a file, each `semctl hook` run appends one
//! JSON line to it. A record holds the event, the host, the tool name, the
//! `PreToolUse` decision, and whether the hook emitted context. A record never
//! holds prompt text, tool input, or context text, so a trace file cannot leak
//! private source.

use std::io::Write;

use serde::Serialize;

use super::HookInput;

const TRACE_ENV: &str = "SEMCTX_HOOK_TRACE";

/// The decision that one `PreToolUse` event reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ToolOutcome {
    /// `SEMCTX_NUDGE_DISABLE` is set.
    Disabled,
    /// The host sent no session id or no turn id.
    MissingIdentity,
    /// The call is a semctx MCP tool. The hook recorded compliance.
    SemctxUse,
    /// The call is not a built-in search.
    NotSearch,
    /// The search targets one file.
    SingleFile,
    /// The search targets a path outside the repository.
    OutsideRepo,
    /// A parallel hook process holds the session lock.
    LockContended,
    /// Recent semctx use suppresses the reminder.
    ComplianceCooled,
    /// The hook counted the search. Dedup, grace, cooldown, or the cap kept it silent.
    Counted,
    /// A reminder was due, but semctl is logged out, unindexed, or unreachable.
    Unavailable,
    /// The hook emitted a reminder.
    Nudged,
}

#[derive(Serialize)]
struct Record<'a> {
    event: &'a str,
    host: &'a str,
    tool: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<ToolOutcome>,
    context: bool,
}

/// Append one record when tracing is enabled. Tracing is best effort: a trace
/// failure must not break the agent session, so write errors are discarded.
pub(super) fn record(input: &HookInput, outcome: Option<ToolOutcome>, context: bool) {
    let Some(path) = std::env::var_os(TRACE_ENV).filter(|path| !path.is_empty()) else {
        return;
    };
    let record = Record {
        event: &input.hook_event_name,
        host: &input.host,
        tool: &input.tool_name,
        outcome,
        context,
    };
    let Ok(mut line) = serde_json::to_vec(&record) else {
        return;
    };
    line.push(b'\n');
    // One append write per record keeps parallel hook processes from
    // interleaving partial lines.
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(&line));
}
