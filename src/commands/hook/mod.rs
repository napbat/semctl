//! `semctl hook` — the binary side of the Claude Code and Codex CLI plugin hooks.
//! Reads a hook event as JSON on stdin and, when the current repo maps to an
//! indexed codebase, emits `additionalContext` for the agent to fold into the turn.
//!
//! Both hosts send the same `snake_case` payload and accept
//! `hookSpecificOutput.additionalContext`; Codex additionally documents
//! `systemMessage` for `PreToolUse`, so that event emits both there. The main
//! input difference is the per-turn id: Codex uses `turn_id`, while Claude uses
//! `prompt_id`. The parser also accepts `PostCompact` for compatibility, while
//! the packaged hooks use `SessionStart(source=compact)` as the shared boundary.
//!
//! - **`SessionStart`** — a one-line orientation: semctx is available, how to
//!   route missing evidence between semctx and local context, accepted selectors
//!   for another checkout, and (only when needed) a cached, once-per-session
//!   instruction to tell the user about a newer CLI.
//! - **`UserPromptSubmit`** — gated, summary-style retrieval for fuzzy discovery
//!   prompts: search the repo's codebase and inject a compact candidate list
//!   (path + line range + symbol + score), not full chunk bodies. Exact graph or
//!   tool-shaped prompts are left for the model to route directly.
//! - **`PreToolUse`** — broad built-in search can emit balanced guidance about
//!   semctx's discovery strengths and valid local-tool cases. Semctx MCP calls
//!   silently record compliance; immediate reminders cool, then a new prompt,
//!   context reset, or bounded consecutive broad-search streak re-arms them.
//!
//! Contract: **never break a session.** An unindexed repo produces an opt-in
//! notice; every actual failure path — not logged in, server down, parse error —
//! produces no output and exits 0.
//! The only thing written to stdout is a well-formed hook-output JSON object.

use std::fmt::Write;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};

use crate::cli::Cli;
use crate::client::{self, Client, api};

mod availability;
mod escalation;
// Reachable from the mcp steering-tests drift guard, which pins the nudge copy
// to the live tool registry.
pub(crate) mod message;
mod scope;
mod sniffer;
mod state;

use scope::{SearchScope, search_scope};

#[derive(Debug, Args)]
pub struct HookArgs {}

/// The subset of the hook stdin payload we use. Every field defaults, so the
/// varying event shapes never fail the parse.
///
/// Claude Code sends these keys in **`snake_case`** (`hook_event_name`, `cwd`, …),
/// so the struct is `snake_case` too: a `camelCase` `rename_all` would bind
/// `hook_event_name` from a `hookEventName` key the real payload never contains,
/// so dispatch saw `""` and the hook silently no-op'd. The `alias`es keep a
/// camelCase host working too, belt-and-suspenders.
#[derive(Debug, Default, Deserialize)]
struct HookInput {
    #[serde(default, alias = "hookEventName")]
    hook_event_name: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    cwd: String,
    // Host-specific adapter identity. Absent for Claude/Codex; OMP sends
    // `"omp"` so guidance uses its marketplace-namespaced MCP tool names.
    #[serde(default)]
    host: String,
    // PreToolUse: the tool being called and its arguments.
    #[serde(default, alias = "toolName")]
    tool_name: String,
    #[serde(default, alias = "toolInput")]
    tool_input: serde_json::Value,
    // Session identity, for per-session nudge state and per-turn dedup.
    #[serde(default, alias = "sessionId")]
    session_id: String,
    // Per-turn dedup id, kept as two distinct fields — Claude sends `prompt_id`,
    // Codex sends `turn_id` — rather than serde-aliasing both onto one. A payload
    // carrying BOTH keys would be a serde duplicate-field error that silently
    // disables the hook; separate optional fields parse cleanly and `turn_key()`
    // picks whichever is present.
    #[serde(default, alias = "promptId")]
    prompt_id: String,
    #[serde(default, alias = "turnId")]
    turn_id: String,
    // SessionStart: Claude `startup` | `resume` | `clear` | `compact`; Codex sends
    // `startup` | `resume` here and splits compaction into its own PostCompact.
    #[serde(default)]
    source: String,
}

impl HookInput {
    /// The per-turn dedup key. Claude sends `prompt_id`, Codex sends `turn_id`;
    /// prefer `prompt_id` when present, else fall back to `turn_id`. Empty when a
    /// host sends neither (the caller then stays silent — no half-working dedup).
    fn turn_key(&self) -> &str {
        if self.prompt_id.is_empty() {
            &self.turn_id
        } else {
            &self.prompt_id
        }
    }

    fn is_codex(&self) -> bool {
        self.prompt_id.is_empty() && !self.turn_id.is_empty()
    }

    fn tool_name_style(&self) -> message::ToolNameStyle {
        if self.host.eq_ignore_ascii_case("omp") {
            message::ToolNameStyle::OmpMarketplace
        } else if self.is_codex() {
            message::ToolNameStyle::CodexPlugin
        } else {
            message::ToolNameStyle::ClaudePlugin
        }
    }
}

#[derive(Serialize)]
struct HookOutput {
    #[serde(rename = "systemMessage", skip_serializing_if = "Option::is_none")]
    system_message: Option<String>,
    #[serde(rename = "hookSpecificOutput", skip_serializing_if = "Option::is_none")]
    hook_specific_output: Option<HookSpecific>,
}

#[derive(Serialize)]
struct HookSpecific {
    #[serde(rename = "hookEventName")]
    hook_event_name: String,
    #[serde(rename = "additionalContext")]
    additional_context: String,
}

pub async fn run(_args: HookArgs, cli: &Cli) -> Result<()> {
    // Kill switch — keep the plugin installed but silence the hook.
    if std::env::var_os("SEMCTX_HOOK_DISABLE").is_some() {
        return Ok(());
    }
    let Some(input) = read_input() else {
        return Ok(());
    };
    let context = match input.hook_event_name.as_str() {
        "UserPromptSubmit" => user_prompt_context(cli, &input).await,
        "SessionStart" | "PostCompact" => {
            // A shrunk-context boundary. Reset the per-session nudge segment (so
            // stale drift pressure never survives it) BEFORE any context work and
            // regardless of availability; opportunistic cleanup too. Hosts use
            // SessionStart(source=clear|compact), and some configurations may also
            // send PostCompact — `resets_segment` accepts both.
            let store = state::Store::default_store();
            if resets_segment(&input.hook_event_name, &input.source) {
                store.reset(&input.session_id);
            }
            store.cleanup();
            // Orientation injects on SessionStart (both hosts). A bare PostCompact
            // emits nothing: Codex's PostCompact schema doesn't accept
            // additionalContext, and orientation re-establishes on the next prompt.
            if input.hook_event_name == "SessionStart" {
                session_start_context(cli, &input, &store).await
            } else {
                None
            }
        }
        "PreToolUse" => pretooluse_nudge(cli, &input).await,
        _ => None,
    };
    if let Some(text) = context {
        emit(&input, &text);
    }
    Ok(())
}

/// Whether an event starts a fresh nudge segment because the context genuinely
/// shrank — accumulated drift pressure should then reset. The normal shared path
/// is `SessionStart(source=clear|compact)`; `PostCompact` remains accepted for
/// compatibility. `run` consults this for both; the unit test pins it so
/// the dispatch can't silently stop resetting on either host (the JSON wiring
/// guard in the tests can't see this side).
fn resets_segment(event: &str, source: &str) -> bool {
    event == "PostCompact" || (event == "SessionStart" && matches!(source, "clear" | "compact"))
}

/// Per-prompt retrieval: run generic candidate search only for fuzzy discovery
/// prompts. Direct graph/tool requests and prompts that can rely on supplied
/// context remain model-routed, avoiding a generic search before a precise call.
async fn user_prompt_context(cli: &Cli, input: &HookInput) -> Option<String> {
    let top_k = env_parse::<usize>("SEMCTX_HOOK_TOP_K").unwrap_or(5);
    if top_k == 0 {
        return None;
    }
    let prompt = input.prompt.trim();
    // A global UserPromptSubmit hook fires on every message. Keep this path
    // high-precision: the skill and MCP metadata route exact operations, while
    // automatic retrieval is reserved for fuzzy code-navigation intent.
    if prompt_route(prompt) != PromptRoute::CandidateSearch {
        return None;
    }

    let (client, codebase) = connect(cli, &input.cwd).await?;
    let body = api::SearchRequestBody {
        query: prompt,
        top_k: u32::try_from(top_k).unwrap_or(u32::MAX),
        codebase_id: Some(&codebase),
        codebase_ids: None,
        scope: None,
        domains: None,
        filters: None,
        kinds: None,
        prefer: None,
        granularity: None,
    };
    let search = async {
        let hits: Vec<api::SearchHit> = client.post("/v1/search", &body).await.ok()?;
        let stale = crate::query::stale_paths(&client, &hits).await;
        Some((hits, stale))
    };
    let (hits, stale) =
        match tokio::time::timeout(Duration::from_secs(HOOK_SEARCH_TIMEOUT_SECS), search).await {
            Ok(Some(result)) => result,
            Ok(None) => {
                debug(format_args!("prompt search failed"));
                return None;
            }
            Err(_) => {
                debug(format_args!("prompt search timed out"));
                return None;
            }
        };
    let min_score = env_parse::<f32>("SEMCTX_HOOK_MIN_SCORE");
    let hits: Vec<&api::SearchHit> = hits
        .iter()
        .filter(|h| min_score.is_none_or(|m| h.score >= f64::from(m)))
        .take(top_k)
        .collect();
    if hits.is_empty() {
        return None;
    }

    let mut out = String::from(
        "semctl — candidate locations from the current index for this prompt. \
         Use them when additional repository evidence is needed. Existing fresh \
         context may already support the task; read a known local path directly \
         when current working-tree bytes matter:\n",
    );
    for h in hits {
        let loc = match (h.line_start, h.line_end) {
            (Some(s), Some(e)) => format!(":{s}-{e}"),
            (Some(s), None) => format!(":{s}"),
            _ => String::new(),
        };
        let path = h.path.as_deref().unwrap_or("(no path)");
        let freshness = if stale.contains(path) {
            "  ⚠ stale; read the local file"
        } else {
            ""
        };
        let sym = h
            .symbol
            .as_deref()
            .map(|s| format!("  {s}"))
            .unwrap_or_default();
        let lang = h
            .language
            .as_deref()
            .map(|l| format!("  ·  {l}"))
            .unwrap_or_default();
        writeln!(out, "- {path}{loc}{sym}{lang}  ({:.3}){freshness}", h.score).unwrap();
    }
    Some(out)
}

const HOOK_SEARCH_TIMEOUT_SECS: u64 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptRoute {
    /// Fuzzy repository discovery benefits from a generic candidate search.
    CandidateSearch,
    /// An exact graph/literal/symbol operation should become one precise MCP call.
    ModelRouted,
    /// The prompt does not justify automatic repository retrieval.
    None,
}

fn prompt_route(prompt: &str) -> PromptRoute {
    if prompt.split_whitespace().count() < 3 {
        return PromptRoute::None;
    }

    let normalized: String = prompt
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    let words: Vec<&str> = normalized.split_whitespace().collect();
    let has = |candidates: &[&str]| words.iter().any(|word| candidates.contains(word));

    let graph_operation = has(&[
        "calls",
        "callers",
        "calling",
        "reference",
        "references",
        "import",
        "imports",
        "implement",
        "implements",
        "trace",
        "flow",
        "flows",
        "subtypes",
        "supertypes",
    ]);
    let implementation_query =
        has(&["what", "which"]) && has(&["implementation", "implementations"]);
    let exhaustive_literal =
        has(&["all", "every"]) && has(&["occurrence", "occurrences", "literal", "literals"]);
    let definition_query = has(&["defined", "definition", "declaration"])
        || (prompt.contains('`') && has(&["find", "where"]));
    let exact_operation =
        graph_operation || implementation_query || exhaustive_literal || definition_query;
    if exact_operation {
        return PromptRoute::ModelRouted;
    }

    let supplied_context = has(&[
        "above", "below", "pasted", "provided", "supplied", "attached", "snippet",
    ]) || (has(&["this", "current"])
        && has(&["code", "body", "function", "method", "file", "content"]));
    if supplied_context {
        return PromptRoute::None;
    }

    let discovery = has(&["find", "locate", "search"]);
    let question = has(&["where", "who", "how", "what", "which"]);
    let code_subject = has(&[
        "code",
        "codebase",
        "repo",
        "repository",
        "function",
        "method",
        "type",
        "trait",
        "class",
        "module",
        "file",
        "symbol",
        "implementation",
        "implemented",
        "define",
        "server",
        "client",
        "cli",
        "hook",
        "hooks",
        "mcp",
        "tool",
        "tools",
        "skill",
        "skills",
    ]);
    if discovery || (question && code_subject) {
        PromptRoute::CandidateSearch
    } else {
        PromptRoute::None
    }
}

/// Cache a successful version lookup for this long. Every new agent session has
/// its own state and checks immediately; repeated resume/clear events within one
/// session reuse the result.
const UPDATE_CACHE_TTL_SECS: u64 = 6 * 60 * 60;
/// A version reminder is advisory and must never hold up agent startup.
const UPDATE_CHECK_TIMEOUT_SECS: u64 = 2;
/// Local smoke-test override. Keep this `None` in committed/release builds.
const FORCE_UPDATE_NOTICE_VERSION: Option<&str> = None;

/// Session-start context combines the normal codebase orientation with a
/// one-shot update instruction. The two checks are independent and concurrent:
/// an unindexed/logged-out repo can still report a CLI update, and a slow update
/// endpoint cannot delay the existing orientation path.
async fn session_start_context(
    cli: &Cli,
    input: &HookInput,
    store: &state::Store,
) -> Option<String> {
    let (orientation, update) = tokio::join!(
        session_orientation_context(cli, input),
        session_update_context(cli, input, store)
    );
    combine_context(orientation, update)
}

/// One-shot orientation: advertise semctx's discovery domain and the accepted
/// current/alternate checkout selectors, or require opt-in for an unindexed repo.
async fn session_orientation_context(cli: &Cli, input: &HookInput) -> Option<String> {
    match resolve_connection(cli, &input.cwd).await? {
        HookConnection::Indexed(_, _) => Some(indexed_orientation().to_string()),
        HookConnection::Unindexed(dir) => Some(unindexed_notice(&dir)),
    }
}

fn indexed_orientation() -> &'static str {
    "This repository is indexed by semctl. Begin with relevant, fresh evidence \
     already available in the conversation. Use the semctl MCP tools \
     (`search_codebase`, `find_definition`, `find_references`, `who_calls`, `imports`) \
     for repository discovery, unknown locations, cross-file relationships, symbol \
     graphs, and broad indexed searches. Use local file tools for current bytes at a \
     known path or range and for narrow, file-scoped checks. Omit `codebase` for this \
     checkout; for another indexed checkout, pass its immutable codebase ID or local \
     directory path — see the codebase-retrieval skill."
}

fn unindexed_notice(dir: &std::path::Path) -> String {
    format!(
        "This repository ({}) is not indexed by semctl. Do not index it automatically. Tell \
         the user it is unindexed and ask whether they want to opt in; only after they agree, \
         call the semctl `index_codebase` MCP tool. All codebase-scoped tools can instead \
         target another existing index by passing its codebase id or indexed directory path \
         in `codebase`.",
        dir.display()
    )
}

/// Return the cached update instruction at most once for this agent session.
/// Successful "already current" results are cached too, so repeated `SessionStart`
/// events add neither network traffic nor model context. Lookup failures stay
/// uncached and silent.
async fn session_update_context(
    cli: &Cli,
    input: &HookInput,
    store: &state::Store,
) -> Option<String> {
    if let Some(version) = FORCE_UPDATE_NOTICE_VERSION {
        return Some(update_notice(version));
    }

    if input.session_id.is_empty()
        || std::env::var("SEMCTX_HOOK_UPDATE_CHECK").as_deref() == Ok("0")
    {
        return None;
    }

    // SessionStart is normally serial, but use the same best-effort lock as
    // PreToolUse so parallel resume/startup events cannot both emit.
    let _lock = store.try_lock(&input.session_id)?;
    let mut state = store.load(&input.session_id);
    if state.update_notice_emitted {
        return None;
    }

    let now = state::now_secs();
    if update_cache_is_fresh(&state, now) {
        let notice = take_update_notice(&mut state);
        if notice.is_some() {
            store.save(&input.session_id, &state);
        }
        return notice;
    }

    let checked = tokio::time::timeout(
        Duration::from_secs(UPDATE_CHECK_TIMEOUT_SECS),
        crate::commands::upgrade::check_for_update_result(cli.server.as_deref()),
    )
    .await;
    let latest = match checked {
        Ok(Ok(version)) => version,
        Ok(Err(e)) => {
            debug(format_args!("update check failed: {e}"));
            return None;
        }
        Err(_) => {
            debug(format_args!("update check timed out"));
            return None;
        }
    };

    // `0` is the sentinel for "never checked"; a broken pre-epoch system clock
    // should not accidentally create a permanent fresh cache entry.
    state.update_checked_at = now.max(1);
    state.update_latest_version = latest.unwrap_or_default();
    let notice = take_update_notice(&mut state);
    store.save(&input.session_id, &state);
    notice
}

fn update_cache_is_fresh(state: &state::NudgeState, now: u64) -> bool {
    state.update_checked_at != 0
        && now >= state.update_checked_at
        && now - state.update_checked_at <= UPDATE_CACHE_TTL_SECS
}

fn take_update_notice(state: &mut state::NudgeState) -> Option<String> {
    if state.update_notice_emitted || state.update_latest_version.is_empty() {
        return None;
    }
    state.update_notice_emitted = true;
    Some(update_notice(&state.update_latest_version))
}

fn update_notice(latest: &str) -> String {
    format!(
        "A newer semctl CLI is available (v{}; this session is running v{}). \
         Tell the user to run `semctl upgrade`, then restart the agent session \
         to load the updated MCP server.",
        latest,
        env!("CARGO_PKG_VERSION")
    )
}

fn combine_context(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first}\n\n{second}")),
        (Some(context), None) | (None, Some(context)) => Some(context),
        (None, None) => None,
    }
}

/// Bound on `connect`'s resolve. It may shell out to git and hit the network on
/// the `SessionStart` / `UserPromptSubmit` path; a timeout drops the resolve future,
/// which (via `kill_on_drop`) kills any child git, so a pathological/hung
/// resolve can never stall the hook. Timed out → silent (never-break).
const CONNECT_TIMEOUT_SECS: u64 = 5;

enum HookConnection {
    Indexed(Client, String),
    Unindexed(PathBuf),
}

async fn resolve_connection(cli: &Cli, cwd: &str) -> Option<HookConnection> {
    let client = client::from_cli(cli).ok()?;
    let dir = if cwd.is_empty() {
        std::env::current_dir().ok()?
    } else {
        PathBuf::from(cwd)
    };
    let resolved = match tokio::time::timeout(
        Duration::from_secs(CONNECT_TIMEOUT_SECS),
        crate::codebase::resolve(&client, &dir),
    )
    .await
    {
        Ok(r) => r,
        Err(_timed_out) => {
            debug(format_args!(
                "connect: resolve timed out for {}",
                dir.display()
            ));
            return None;
        }
    };
    match resolved {
        Ok(Some(resolved)) => {
            debug(format_args!("codebase {} ({})", resolved.id, resolved.how));
            let id = resolved.id;
            // `resolve` lifts nested Git directories to the cached working-copy
            // root. Use that recorded root for the checkout identity header too;
            // hashing the hook's literal `repo/src` cwd would select a different,
            // nonexistent copy even though resolution found the right codebase.
            let local_root = crate::config::load()
                .ok()
                .and_then(|config| config.codebase_root(&id, Some(&dir)))
                .unwrap_or(dir);
            let client = client
                .with_codebase(id.clone())
                .with_local_root(Some(local_root));
            Some(HookConnection::Indexed(client, id))
        }
        Ok(None) => {
            debug(format_args!("repo {} not indexed", dir.display()));
            Some(HookConnection::Unindexed(dir))
        }
        Err(e) => {
            debug(format_args!("resolve failed: {e}"));
            None
        }
    }
}

/// Build an authenticated client + resolve the cwd's codebase id. `None`
/// (silently) for every can't/shouldn't-act case: no login, repo not indexed,
/// server unreachable, resolve timed out.
async fn connect(cli: &Cli, cwd: &str) -> Option<(Client, String)> {
    match resolve_connection(cli, cwd).await? {
        HookConnection::Indexed(client, id) => Some((client, id)),
        HookConnection::Unindexed(_) => None,
    }
}

/// `PreToolUse`: record semctx compliance silently, or emit balanced routing
/// guidance when broad built-in searching resumes. Single-file and outside-repo
/// searches stay silent; compliance cools immediate reminders and a bounded
/// broad-search streak re-arms them. Emissions remain deduped and availability-gated.
async fn pretooluse_nudge(cli: &Cli, input: &HookInput) -> Option<String> {
    if std::env::var_os("SEMCTX_NUDGE_DISABLE").is_some() {
        return None;
    }
    // Need both identity fields: `session_id` keys the per-session state, the
    // per-turn key (`turn_key`) keys the dedup — without it, parallel searches in
    // one turn could each nudge. Both are standard PreToolUse fields (Claude
    // `prompt_id`, Codex `turn_id`); if a host omits either, stay silent
    // (never-break, no half-working dedup). Gets its own debug line — the
    // likeliest host-compatibility gap.
    let turn = input.turn_key();
    if input.session_id.is_empty() || turn.is_empty() {
        debug(format_args!(
            "PreToolUse: missing identity (session_id={}, turn_key={}) — silent",
            !input.session_id.is_empty(),
            !turn.is_empty()
        ));
        return None;
    }

    // The compliance matcher has a state-only fast path: no availability probe,
    // message, or recursive nudge. A later broad-search streak re-arms guidance.
    if is_semctx_tool_name(&input.tool_name) {
        record_semctx_compliance(&input.session_id, turn);
        return None;
    }

    // Eligibility — local, no network. Non-search calls bail here cheaply.
    let Some(call) = sniffer::eligible_search(&input.tool_name, &input.tool_input) else {
        debug(format_args!(
            "PreToolUse {}: not an eligible search",
            input.tool_name
        ));
        return None;
    };
    debug(format_args!(
        "PreToolUse {}: eligible search",
        input.tool_name
    ));

    // Scope needs cwd/repo awareness, so keep the sniffer pure and decide here.
    // Known single-file work is a valid local operation and must not build drift
    // pressure; provably outside-repo searches are unrelated to this index.
    match search_scope(&input.tool_name, &input.tool_input, &input.cwd) {
        SearchScope::SingleFile => {
            debug(format_args!("nudge: single-file target — silent"));
            return None;
        }
        SearchScope::OutsideRepo => {
            debug(format_args!("nudge: target outside repo — silent"));
            return None;
        }
        SearchScope::BroadOrUnknown => {}
    }

    // Critical section under the per-session lock: read → decide → write.
    // Contended (parallel tool calls) → skip; the holder handles the nudge.
    let store = state::Store::default_store();
    let Some(_lock) = store.try_lock(&input.session_id) else {
        debug(format_args!("nudge: session lock contended — skip"));
        return None;
    };

    if compliance_suppresses(&store, &input.session_id, turn, load_compliance_rearm()) {
        return None;
    }
    // advance() logs its own specific silence reason (dedup / grace / cooldown / cap).
    let (mut st, tier) = advance(&store, &input.session_id, turn, &load_thresholds())?;
    debug(format_args!(
        "nudge: {:?} at n={} — checking availability",
        tier, st.eligible_count
    ));

    // Availability last (it may touch the network) — TTL-cached, strict:
    // logged out / not indexed / server down → silent. Updates the cache fields
    // on `st`, which `finalize` then persists regardless of the verdict.
    let available = availability::is_available_cached(cli, &input.cwd, &mut st).await;
    let (message, st) = finalize(
        st,
        tier,
        input.tool_name_style(),
        call.kind,
        call.pattern,
        turn,
        available,
    );
    store.save(&input.session_id, &st);
    match &message {
        Some(_) => debug(format_args!("nudge: emitting {tier:?}")),
        None => debug(format_args!(
            "nudge: semctl unavailable (logged out / not indexed / down) — silent"
        )),
    }
    message
}

const SEMCTX_CODEX_TOOL_PREFIX: &str = "mcp__semctx__";
const SEMCTX_CLAUDE_PLUGIN_TOOL_PREFIX: &str = "mcp__plugin_semctx_semctx__";
const SEMCTX_OMP_TOOL_PREFIX: &str = "mcp__semctx_semctx_";

fn is_semctx_tool_name(tool_name: &str) -> bool {
    tool_name.starts_with(SEMCTX_CODEX_TOOL_PREFIX)
        || tool_name.starts_with(SEMCTX_CLAUDE_PLUGIN_TOOL_PREFIX)
        || tool_name.starts_with(SEMCTX_OMP_TOOL_PREFIX)
}

fn record_semctx_compliance(session_id: &str, prompt_id: &str) {
    let store = state::Store::default_store();
    let Some(_lock) = store.try_lock(session_id) else {
        debug(format_args!("compliance: session lock contended — skip"));
        return;
    };
    let mut st = store.load(session_id);
    if st.record_semctx_use(prompt_id) {
        store.save(session_id, &st);
        debug(format_args!("compliance: recorded semctx use"));
    } else {
        debug(format_args!("compliance: already current"));
    }
}

fn compliance_suppresses(
    store: &state::Store,
    session_id: &str,
    prompt_id: &str,
    rearm_after: u32,
) -> bool {
    let mut st = store.load(session_id);
    match st.compliance_decision(prompt_id, rearm_after) {
        state::ComplianceDecision::Inactive => false,
        state::ComplianceDecision::Suppress => {
            store.save(session_id, &st);
            debug(format_args!(
                "nudge: semctx used this prompt; cooling broad search {}",
                st.broad_searches_after_semctx
            ));
            true
        }
        state::ComplianceDecision::Rearmed => {
            store.save(session_id, &st);
            debug(format_args!("nudge: compliance cooling re-armed"));
            false
        }
    }
}

fn load_compliance_rearm() -> u32 {
    env_parse("SEMCTX_NUDGE_REARM_BROAD").unwrap_or(3)
}

/// Finalize a fired decision against the availability verdict. Pure and
/// testable: **unavailable** → persist the advanced count/cache but don't fire
/// or remember the prompt (so the turn can re-evaluate once available);
/// **available** → increment `nudges_fired`, stamp `last_nudge_at_count`,
/// remember the prompt, and return the tailored message.
fn finalize(
    mut st: state::NudgeState,
    tier: escalation::Tier,
    tool_names: message::ToolNameStyle,
    kind: message::SearchKind,
    pattern: Option<&str>,
    prompt_id: &str,
    available: bool,
) -> (Option<String>, state::NudgeState) {
    if !available {
        return (None, st);
    }
    let message = match tier {
        escalation::Tier::One => message::tier1(tool_names),
        escalation::Tier::Two => message::tier2(tool_names, kind, st.eligible_count, pattern),
    };
    st.nudges_fired = st.nudges_fired.saturating_add(1);
    st.last_nudge_at_count = st.eligible_count;
    st.remember_prompt(prompt_id);
    (Some(message), st)
}

fn load_thresholds() -> escalation::Thresholds {
    escalation::Thresholds {
        grace: env_parse("SEMCTX_NUDGE_GRACE").unwrap_or(1),
        cooldown: env_parse("SEMCTX_NUDGE_COOLDOWN").unwrap_or(3),
        max: env_parse("SEMCTX_NUDGE_MAX").unwrap_or(4),
    }
}

/// The lock-guarded counting step, factored out so its dedup + increment +
/// escalation ordering is testable without a client or network. Assumes the
/// caller holds the session lock. Returns the advanced state and the tier to
/// fire, or `None` (already saving the advanced count) when we should stay
/// silent — including the per-turn dedup case, which does not advance the count.
fn advance(
    store: &state::Store,
    session_id: &str,
    prompt_id: &str,
    thresholds: &escalation::Thresholds,
) -> Option<(state::NudgeState, escalation::Tier)> {
    let mut st = store.load(session_id);

    // One nudge per user turn, however many parallel searches it fires.
    if st.already_nudged(prompt_id) {
        debug(format_args!("nudge: per-turn dedup — silent"));
        return None;
    }

    st.eligible_count = st.eligible_count.saturating_add(1);

    match escalation::decide(
        st.eligible_count,
        st.nudges_fired,
        st.last_nudge_at_count,
        thresholds,
    ) {
        escalation::Decision::Silent(reason) => {
            // Persist the advanced count so cooldown/tier progress next call.
            store.save(session_id, &st);
            debug(format_args!(
                "nudge: {} — silent at n={}",
                reason.label(),
                st.eligible_count
            ));
            None
        }
        escalation::Decision::Fire(tier) => Some((st, tier)),
    }
}

fn read_input() -> Option<HookInput> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok()?;
    serde_json::from_str(&buf).ok()
}

fn emit(input: &HookInput, context: &str) {
    use std::io::Write;
    let out = hook_output(input, context);
    if let Ok(s) = serde_json::to_string(&out) {
        // Ignore a broken pipe (Claude closing the hook's stdout early): a
        // `println!` would panic and exit nonzero, breaking the never-break
        // contract. A failed write just means no context this call.
        let _ = writeln!(std::io::stdout().lock(), "{s}");
    }
}

fn hook_output(input: &HookInput, context: &str) -> HookOutput {
    // Codex documents `systemMessage` for PreToolUse. Keep the additional-context
    // payload too because current Codex releases surface it to the model and
    // Claude uses it; host-specific contract tests pin both shapes.
    let codex_pretooluse = input.hook_event_name == "PreToolUse" && input.is_codex();
    HookOutput {
        system_message: codex_pretooluse.then(|| context.to_string()),
        hook_specific_output: Some(HookSpecific {
            hook_event_name: input.hook_event_name.clone(),
            additional_context: context.to_string(),
        }),
    }
}

/// Parse an env var into any `FromStr` type, `None` if unset or unparseable.
/// One helper for all the `SEMCTX_*` knobs (also reachable from the `availability`
/// submodule as `super::env_parse`), so there is a single parse path and no
/// truncating `as` casts at the call sites.
fn env_parse<T: FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

/// Opt-in diagnostics to stderr, compiled in **only** with the `hook-debug`
/// cargo feature — so release builds get the no-op below and there is no runtime
/// switch that could accidentally surface internal detail onto a Claude Code
/// session. The enabled variant uses a non-panicking write so a broken stderr
/// pipe can't break the session.
#[cfg(feature = "hook-debug")]
fn debug(args: std::fmt::Arguments) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "semctl hook: {args}");
}

#[cfg(not(feature = "hook-debug"))]
#[inline(always)]
fn debug(_args: std::fmt::Arguments) {}

#[cfg(test)]
mod tests;
