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
//! - **`SessionStart`** — a one-line orientation: which codebase this repo maps
//!   to, a nudge to prefer the semctl MCP tools, and (only when needed) a cached,
//!   once-per-session instruction to tell the user about a newer CLI.
//! - **`UserPromptSubmit`** — gated, summary-style retrieval: for a non-trivial
//!   prompt, search the repo's codebase and inject a compact candidate list
//!   (path + line range + symbol + score), not full chunk bodies.
//! - **`PreToolUse`** — when the agent reaches for built-in `Grep`/`Glob` or a
//!   Bash/PowerShell `grep`/`rg`/`find`, and this repo is indexed, emit a firm
//!   but non-blocking reminder to prefer the semctl MCP tools. Escalates with
//!   reliance within a segment, deduped per turn, strictly gated on availability.
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
mod sniffer;
mod state;

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
        !self.turn_id.is_empty()
    }

    fn tool_name_style(&self) -> message::ToolNameStyle {
        if self.host.eq_ignore_ascii_case("omp") {
            message::ToolNameStyle::OmpMarketplace
        } else {
            message::ToolNameStyle::ClaudeCompatible
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

/// Per-prompt retrieval: skip trivial prompts, search the repo's codebase,
/// return a capped summary-style hit list.
async fn user_prompt_context(cli: &Cli, input: &HookInput) -> Option<String> {
    let top_k = env_parse::<usize>("SEMCTX_HOOK_TOP_K").unwrap_or(5);
    if top_k == 0 {
        return None;
    }
    let prompt = input.prompt.trim();
    // A global UserPromptSubmit hook fires on every message. Keep this path
    // high-precision: the skill and MCP metadata remain available for other
    // prompts, while automatic retrieval is reserved for code-navigation intent.
    if !prompt_wants_retrieval(prompt) {
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
         Pull current detail with the \
         semctl MCP tools (search_codebase / find_definition / find_references) \
         rather than re-searching:\n",
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

fn prompt_wants_retrieval(prompt: &str) -> bool {
    if prompt.split_whitespace().count() < 3 {
        return false;
    }
    if prompt.contains('`') {
        return true;
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

    let direct = has(&[
        "find",
        "locate",
        "search",
        "trace",
        "explain",
        "understand",
        "review",
    ]);
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
        "defined",
        "server",
        "client",
        "cli",
        "calls",
        "callers",
        "references",
        "imports",
        "flow",
        "hook",
        "hooks",
        "mcp",
        "tool",
        "tools",
        "skill",
        "skills",
    ]);
    direct || (question && code_subject)
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

/// One-shot orientation: name an indexed codebase, or tell the agent that the
/// repository is unindexed and indexing requires explicit user opt-in.
async fn session_orientation_context(cli: &Cli, input: &HookInput) -> Option<String> {
    let (client, codebase) = match resolve_connection(cli, &input.cwd).await? {
        HookConnection::Indexed(client, codebase) => (client, codebase),
        HookConnection::Unindexed(dir) => {
            return Some(unindexed_notice(&dir));
        }
    };
    // Best-effort: the codebase's slug for a friendlier label.
    let name = client
        .get::<api::CodebaseSummary>(&format!("/v1/codebases/{codebase}"))
        .await
        .ok()
        .map_or_else(|| codebase.clone(), |c| c.slug);
    Some(format!(
        "This repository is indexed by semctl as \"{name}\". For any \
         \"find / explain / where is / who imports\" question about this repo, prefer \
         the semctl MCP tools (search_codebase, find_definition, find_references, \
         imports, symbol_edges) over raw grep / read — see the codebase-retrieval skill.",
    ))
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
            let client = client.with_codebase(id.clone()).with_local_root(Some(dir));
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

/// `PreToolUse`: when the agent reaches for built-in `Grep`/`Glob` or a
/// Bash/PowerShell search, and semctl has this repo indexed, emit a firm but
/// non-blocking reminder to prefer the semctl tools. Escalates with reliance
/// within a segment; deduped per turn; strictly gated on availability.
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

    // Don't steer toward the index for a search provably outside the repo.
    if target_outside_repo(&input.tool_input, &input.cwd) {
        debug(format_args!("nudge: target outside repo — silent"));
        return None;
    }

    // Critical section under the per-session lock: read → decide → write.
    // Contended (parallel tool calls) → skip; the holder handles the nudge.
    let store = state::Store::default_store();
    let Some(_lock) = store.try_lock(&input.session_id) else {
        debug(format_args!("nudge: session lock contended — skip"));
        return None;
    };
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
        max: env_parse("SEMCTX_NUDGE_MAX").unwrap_or(12),
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

/// Best-effort: suppress the nudge when a search provably targets a path OUTSIDE
/// the repo. Covers the Grep/Glob `path` field and absolute-path operands inside
/// a Bash/PowerShell command. The repo root is the nearest `.git` ancestor of
/// `cwd` (so launching Claude in a subdirectory doesn't wrongly suppress an
/// in-repo search). Only suppresses when a path both canonicalizes AND lands
/// outside the root — when unsure, don't suppress.
fn target_outside_repo(tool_input: &serde_json::Value, cwd: &str) -> bool {
    let Some(root) = repo_root(cwd) else {
        return false;
    };
    // Grep/Glob: the explicit absolute `path` field.
    if let Some(path) = tool_input.get("path").and_then(|v| v.as_str()) {
        let p = std::path::Path::new(path);
        if p.is_absolute() && path_is_outside(p, &root) {
            return true;
        }
    }
    // Bash/PowerShell: any absolute-path operand that lands outside the repo
    // (e.g. `rg password C:\Users\me\Downloads`). Best-effort token scan; a
    // relative operand or one that can't be resolved is left to nudge.
    if let Some(cmd) = tool_input.get("command").and_then(|v| v.as_str()) {
        for tok in cmd.split_whitespace() {
            let tok = tok.trim_matches(|c| c == '"' || c == '\'');
            if is_abs_path_token(tok) && path_is_outside(std::path::Path::new(tok), &root) {
                return true;
            }
        }
    }
    false
}

/// The repo root for `cwd`: the nearest ancestor containing a `.git` entry (a
/// dir, or a file for worktrees/submodules), else `cwd` itself. `None` for an
/// empty cwd.
fn repo_root(cwd: &str) -> Option<PathBuf> {
    if cwd.is_empty() {
        return None;
    }
    let start = PathBuf::from(cwd);
    let mut cur: &std::path::Path = &start;
    loop {
        if cur.join(".git").exists() {
            return Some(cur.to_path_buf());
        }
        let Some(p) = cur.parent() else { break };
        cur = p;
    }
    Some(start)
}

/// Whether a token looks like an absolute path: POSIX `/…`, a rooted/UNC `\…`,
/// or a Windows drive `C:\…` / `C:/…`.
fn is_abs_path_token(tok: &str) -> bool {
    let b = tok.as_bytes();
    tok.starts_with('/')
        || tok.starts_with('\\')
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'/' || b[2] == b'\\'))
}

/// Whether `path` provably resolves outside `root`. Requires BOTH to
/// canonicalize (same prefix form for a valid `starts_with`); if either fails —
/// a non-existent path, a permissions error, or a Windows `\\?\` mismatch — we
/// cannot be sure, so it is treated as NOT outside (don't suppress).
fn path_is_outside(path: &std::path::Path, root: &std::path::Path) -> bool {
    match (std::fs::canonicalize(path), std::fs::canonicalize(root)) {
        (Ok(cp), Ok(cr)) => !cp.starts_with(&cr),
        _ => false,
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
mod tests {
    use super::*;

    // Claude Code sends the hook payload in snake_case (`hook_event_name`,
    // `tool_name`, `transcript_path`, …). A camelCase-only struct silently
    // fails to bind `hook_event_name`, so dispatch sees "" and the hook is a
    // no-op — which is exactly how the original bug hid behind the
    // never-break-a-session contract.
    #[test]
    fn parses_real_snake_case_payload() {
        let raw =
            r#"{"hook_event_name":"SessionStart","prompt":"how does auth work","cwd":"/repo"}"#;
        let input: HookInput = serde_json::from_str(raw).expect("payload parses");
        assert_eq!(input.hook_event_name, "SessionStart");
        assert_eq!(input.prompt, "how does auth work");
        assert_eq!(input.cwd, "/repo");
    }

    #[test]
    fn unindexed_startup_notice_requires_opt_in_and_names_the_tool() {
        let message = unindexed_notice(std::path::Path::new("/repo"));
        assert!(message.contains("not indexed"), "{message}");
        assert!(
            message.contains("Do not index it automatically"),
            "{message}"
        );
        assert!(
            message.contains("ask whether they want to opt in"),
            "{message}"
        );
        assert!(message.contains("`index_codebase`"), "{message}");
        assert!(
            message.contains("codebase id or indexed directory path"),
            "{message}"
        );
    }

    // Belt-and-suspenders: a camelCase payload (older/other hosts) still binds.
    #[test]
    fn still_accepts_camel_case_event_name() {
        let raw = r#"{"hookEventName":"UserPromptSubmit","cwd":"/repo"}"#;
        let input: HookInput = serde_json::from_str(raw).expect("payload parses");
        assert_eq!(input.hook_event_name, "UserPromptSubmit");
    }

    #[test]
    fn parses_omp_host_and_selects_marketplace_tool_names() {
        let raw = r#"{"host":"omp","hook_event_name":"PreToolUse","session_id":"s","prompt_id":"p1","cwd":"/repo","tool_name":"Grep","tool_input":{"pattern":"needle"}}"#;
        let input: HookInput = serde_json::from_str(raw).expect("OMP payload parses");
        assert_eq!(input.host, "omp");
        assert_eq!(input.turn_key(), "p1");
        assert_eq!(
            input.tool_name_style(),
            message::ToolNameStyle::OmpMarketplace
        );
    }

    // Codex sends the same snake_case shape as Claude but names the per-turn id
    // `turn_id` and its shell tool is `Bash` carrying `tool_input.command`. The
    // alias must bind turn_id to the dedup key, and the existing sniffer must
    // treat the Bash command as an eligible search — so one binary serves both.
    #[test]
    fn parses_codex_pretooluse_payload() {
        let raw = r#"{"hook_event_name":"PreToolUse","session_id":"s","turn_id":"t1","cwd":"/repo","tool_name":"Bash","tool_input":{"command":"rg needle ."}}"#;
        let input: HookInput = serde_json::from_str(raw).expect("codex payload parses");
        assert_eq!(input.hook_event_name, "PreToolUse");
        assert_eq!(input.prompt_id, "", "codex omits prompt_id");
        assert_eq!(input.turn_id, "t1");
        assert_eq!(
            input.turn_key(),
            "t1",
            "turn_id drives the dedup key when prompt_id is absent"
        );
        assert_eq!(input.tool_name, "Bash");
        assert!(
            sniffer::eligible_search(&input.tool_name, &input.tool_input).is_some(),
            "codex Bash `rg` is an eligible search"
        );
    }

    // Regression for the serde-alias footgun: a payload carrying BOTH `prompt_id`
    // and `turn_id` must still parse (distinct fields, not aliases of one) and
    // prefer `prompt_id`. Aliasing both onto one field was a duplicate-field parse
    // error that would have silently disabled the hook.
    #[test]
    fn both_prompt_id_and_turn_id_parse_without_error() {
        let raw = r#"{"hook_event_name":"PreToolUse","session_id":"s","prompt_id":"p","turn_id":"t","tool_name":"Bash","tool_input":{"command":"rg x"}}"#;
        let input: HookInput = serde_json::from_str(raw).expect("both-keys payload parses");
        assert_eq!(
            input.turn_key(),
            "p",
            "prompt_id wins when both are present"
        );
    }

    // Pins the segment-reset dispatch invariant for BOTH hosts. Unlike the
    // JSON-only wiring guard, dropping the PostCompact (or SessionStart
    // clear/compact) reset in `run` fails here.
    #[test]
    fn resets_segment_covers_both_hosts() {
        assert!(resets_segment("PostCompact", ""), "Codex compaction");
        assert!(resets_segment("SessionStart", "clear"), "Claude /clear");
        assert!(
            resets_segment("SessionStart", "compact"),
            "Claude compaction"
        );
        assert!(!resets_segment("SessionStart", "startup"));
        assert!(!resets_segment("SessionStart", "resume"));
        assert!(!resets_segment("PreToolUse", ""));
        assert!(!resets_segment("UserPromptSubmit", ""));
    }

    #[test]
    fn update_cache_rejects_missing_expired_and_future_timestamps() {
        let mut state = state::NudgeState::default();
        assert!(!update_cache_is_fresh(&state, 100));

        state.update_checked_at = 100;
        assert!(update_cache_is_fresh(&state, 100));
        assert!(update_cache_is_fresh(&state, 100 + UPDATE_CACHE_TTL_SECS));
        assert!(!update_cache_is_fresh(&state, 101 + UPDATE_CACHE_TTL_SECS));
        assert!(!update_cache_is_fresh(&state, 99));
    }

    #[test]
    fn update_notice_is_silent_when_current_and_one_shot_when_newer() {
        let mut current = state::NudgeState {
            update_checked_at: 100,
            ..Default::default()
        };
        assert!(take_update_notice(&mut current).is_none());
        assert!(!current.update_notice_emitted);

        let mut outdated = state::NudgeState {
            update_checked_at: 100,
            update_latest_version: "9.8.7".into(),
            ..Default::default()
        };
        let notice = take_update_notice(&mut outdated).expect("newer version surfaces");
        assert!(notice.contains("v9.8.7"));
        assert!(notice.contains("Tell the user"));
        assert!(notice.contains("semctl upgrade"));
        assert!(outdated.update_notice_emitted);
        assert!(
            take_update_notice(&mut outdated).is_none(),
            "same session stays silent after the first notice"
        );
    }

    #[test]
    fn session_context_combines_only_present_parts() {
        assert_eq!(combine_context(None, None), None);
        assert_eq!(
            combine_context(Some("orientation".into()), None).as_deref(),
            Some("orientation")
        );
        assert_eq!(
            combine_context(None, Some("update".into())).as_deref(),
            Some("update")
        );
        assert_eq!(
            combine_context(Some("orientation".into()), Some("update".into())).as_deref(),
            Some("orientation\n\nupdate")
        );
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    const T: escalation::Thresholds = escalation::Thresholds {
        grace: 1,
        cooldown: 3,
        max: 12,
    };

    /// A store rooted at a unique temp dir, auto-removed on drop.
    struct TempStore {
        store: state::Store,
        dir: std::path::PathBuf,
    }
    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn temp_store() -> TempStore {
        let dir = std::env::temp_dir().join(format!(
            "semctl-nudge-adv-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        TempStore {
            store: state::Store::with_dir(dir.clone()),
            dir,
        }
    }

    #[test]
    fn advance_grace_is_silent_but_counts() {
        let t = temp_store();
        assert!(advance(&t.store, "s1", "", &T).is_none()); // n=1 ≤ grace
        assert_eq!(t.store.load("s1").eligible_count, 1, "count persisted");
    }

    #[test]
    fn advance_fires_tier_one_then_two() {
        let t = temp_store();
        // Seed: one grace call already happened.
        t.store.save(
            "s1",
            &state::NudgeState {
                eligible_count: 1,
                ..Default::default()
            },
        );
        let (st, tier) = advance(&t.store, "s1", "", &T).expect("fires at n=2");
        assert_eq!(tier, escalation::Tier::One);
        assert_eq!(st.eligible_count, 2);

        // Seed just before the Tier-2 boundary (last nudge at n=2).
        t.store.save(
            "s1",
            &state::NudgeState {
                eligible_count: 4,
                nudges_fired: 1,
                last_nudge_at_count: 2,
                ..Default::default()
            },
        );
        let (_st, tier) = advance(&t.store, "s1", "", &T).expect("fires at n=5");
        assert_eq!(tier, escalation::Tier::Two);
    }

    #[test]
    fn advance_cooldown_is_silent_and_persists() {
        let t = temp_store();
        // Just fired at n=2; n=3 is inside the 3-call cooldown.
        t.store.save(
            "s1",
            &state::NudgeState {
                eligible_count: 2,
                nudges_fired: 1,
                last_nudge_at_count: 2,
                ..Default::default()
            },
        );
        assert!(advance(&t.store, "s1", "", &T).is_none());
        assert_eq!(
            t.store.load("s1").eligible_count,
            3,
            "count advanced under cooldown"
        );
    }

    #[test]
    fn advance_dedups_within_a_turn_without_counting() {
        let t = temp_store();
        let mut seeded = state::NudgeState {
            eligible_count: 4,
            ..Default::default()
        };
        seeded.remember_prompt("p1");
        t.store.save("s1", &seeded);
        // Same prompt_id → no nudge, and the count must not advance.
        assert!(advance(&t.store, "s1", "p1", &T).is_none());
        assert_eq!(
            t.store.load("s1").eligible_count,
            4,
            "dedup does not increment"
        );
    }

    #[test]
    fn finalize_unavailable_persists_count_but_does_not_fire() {
        let st = state::NudgeState {
            eligible_count: 5,
            nudges_fired: 1,
            ..Default::default()
        };
        let (msg, out) = finalize(
            st,
            escalation::Tier::Two,
            message::ToolNameStyle::ClaudeCompatible,
            message::SearchKind::Content,
            Some("x"),
            "p1",
            false,
        );
        assert!(msg.is_none(), "unavailable → no message");
        assert_eq!(out.nudges_fired, 1, "not incremented when unavailable");
        assert!(
            !out.already_nudged("p1"),
            "prompt not remembered when unavailable"
        );
        assert_eq!(
            out.eligible_count, 5,
            "advanced count preserved for re-evaluation"
        );
    }

    #[test]
    fn finalize_available_fires_and_records() {
        let st = state::NudgeState {
            eligible_count: 5,
            nudges_fired: 1,
            ..Default::default()
        };
        let (msg, out) = finalize(
            st,
            escalation::Tier::Two,
            message::ToolNameStyle::ClaudeCompatible,
            message::SearchKind::Content,
            Some("parse_config"),
            "p1",
            true,
        );
        let msg = msg.expect("available → fires");
        assert!(msg.contains("parse_config"), "tier-2 symbol copy");
        assert_eq!(out.nudges_fired, 2);
        assert_eq!(out.last_nudge_at_count, 5);
        assert!(
            out.already_nudged("p1"),
            "prompt remembered so the turn won't re-nudge"
        );
    }

    #[test]
    fn finalize_filename_kind_routes_to_file_tools_not_grep() {
        let st = state::NudgeState {
            eligible_count: 5,
            ..Default::default()
        };
        let (msg, _) = finalize(
            st,
            escalation::Tier::Two,
            message::ToolNameStyle::ClaudeCompatible,
            message::SearchKind::Filename,
            None,
            "p1",
            true,
        );
        let msg = msg.unwrap();
        assert!(msg.contains("mcp__semctx__list_files"));
        assert!(
            !msg.contains("mcp__semctx__grep"),
            "a filename nudge must not steer to grep"
        );
    }

    #[test]
    fn outside_repo_uses_git_root_and_scans_shell_operands() {
        use serde_json::json;
        let base = std::env::temp_dir().join(format!(
            "semctl-tor-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let repo = base.join("repo");
        let sub = repo.join("crate").join("src"); // Claude launched in a subdir
        let inrepo = repo.join("server"); // in-repo, but outside cwd
        let outside = base.join("elsewhere");
        for d in [
            repo.join(".git"),
            sub.clone(),
            inrepo.clone(),
            outside.clone(),
        ] {
            std::fs::create_dir_all(&d).unwrap();
        }
        let cwd = sub.to_str().unwrap();
        let abs = |p: &std::path::Path| p.to_str().unwrap().to_string();

        // relative / missing path → never suppress
        assert!(!target_outside_repo(&json!({}), cwd));
        assert!(!target_outside_repo(&json!({ "path": "src" }), cwd));
        // absolute in-repo path OUTSIDE cwd but inside the repo → not suppressed (#4)
        assert!(!target_outside_repo(&json!({ "path": abs(&inrepo) }), cwd));
        // absolute path outside the repo → suppressed
        assert!(target_outside_repo(&json!({ "path": abs(&outside) }), cwd));
        // shell search of an outside absolute path → suppressed (#7)
        assert!(target_outside_repo(
            &json!({ "command": format!("rg password {}", abs(&outside)) }),
            cwd
        ));
        // shell search of an in-repo absolute path → not suppressed
        assert!(!target_outside_repo(
            &json!({ "command": format!("rg foo {}", abs(&inrepo)) }),
            cwd
        ));
        // empty cwd → never suppress
        assert!(!target_outside_repo(&json!({ "path": abs(&outside) }), ""));

        let _ = std::fs::remove_dir_all(&base);
    }

    // Safety-critical: the nudge output must never carry a `permissionDecision`.
    // An `allow` on a Bash/PowerShell call would auto-approve an arbitrary
    // command; this locks the serialized shape so a future field can't regress it.
    #[test]
    fn hook_output_never_carries_permission_decision() {
        let out = HookOutput {
            system_message: Some("prefer semctl".into()),
            hook_specific_output: Some(HookSpecific {
                hook_event_name: "PreToolUse".into(),
                additional_context: "prefer semctl".into(),
            }),
        };
        let json = serde_json::to_string(&out).expect("serializes");
        assert!(
            !json.contains("permissionDecision"),
            "unexpected permissionDecision: {json}"
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["systemMessage"], "prefer semctl");
        let inner = v["hookSpecificOutput"]
            .as_object()
            .expect("hookSpecificOutput object");
        assert_eq!(inner.len(), 2, "exactly hookEventName + additionalContext");
        assert!(inner.contains_key("hookEventName"));
        assert!(inner.contains_key("additionalContext"));
    }

    #[test]
    fn pretooluse_output_preserves_each_hosts_contract() {
        let codex = HookInput {
            hook_event_name: "PreToolUse".into(),
            turn_id: "turn-1".into(),
            ..Default::default()
        };
        let claude = HookInput {
            hook_event_name: "PreToolUse".into(),
            prompt_id: "prompt-1".into(),
            ..Default::default()
        };
        let codex = serde_json::to_value(hook_output(&codex, "prefer semctl")).unwrap();
        let claude = serde_json::to_value(hook_output(&claude, "prefer semctl")).unwrap();

        assert_eq!(codex["systemMessage"], "prefer semctl");
        assert_eq!(
            codex["hookSpecificOutput"]["additionalContext"],
            "prefer semctl"
        );
        assert!(
            claude.get("systemMessage").is_none(),
            "Claude keeps its existing additional-context-only output"
        );
        assert_eq!(
            claude["hookSpecificOutput"]["additionalContext"],
            "prefer semctl"
        );
    }

    #[test]
    fn prompt_retrieval_gate_prefers_precision() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/plugins/semctx/skills/codebase-retrieval/evals.json"
        );
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let evals: serde_json::Value = serde_json::from_str(&raw).expect("valid eval JSON");
        assert_eq!(evals["schema_version"], 1);
        assert_eq!(
            evals["hosts"],
            serde_json::json!(["codex", "claude", "omp"])
        );

        for prompt in evals["hook_injection"]["should_search"]
            .as_array()
            .expect("should_search cases")
        {
            let prompt = prompt.as_str().expect("prompt string");
            assert!(prompt_wants_retrieval(prompt), "{prompt}");
        }
        for prompt in evals["hook_injection"]["should_not_search"]
            .as_array()
            .expect("should_not_search cases")
        {
            let prompt = prompt.as_str().expect("prompt string");
            assert!(!prompt_wants_retrieval(prompt), "{prompt}");
        }
    }

    // Drift guard for the cross-host hooks contract. Keep the PreToolUse matcher
    // in lockstep with the sniffer and keep every event bounded; this file is
    // shared by every plugin host that supports the common hook schema.
    #[test]
    fn shared_hooks_are_wired_and_match_the_sniffer() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/plugins/semctx/hooks/hooks.json"
        );
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let v: serde_json::Value = serde_json::from_str(&raw).expect("hooks.json is valid JSON");

        let pre = v["hooks"]["PreToolUse"]
            .as_array()
            .expect("PreToolUse block present");
        let entry = pre.first().expect("a PreToolUse entry");
        assert_eq!(
            entry["hooks"][0]["command"], "semctl hook",
            "PreToolUse → semctl hook"
        );

        let matcher = entry["matcher"].as_str().expect("matcher is a string");
        let mut got: Vec<&str> = matcher.split('|').collect();
        got.sort_unstable();
        let mut want: Vec<&str> = sniffer::HANDLED_TOOLS.to_vec();
        want.sort_unstable();
        assert_eq!(
            got, want,
            "hooks.json PreToolUse matcher drifted from sniffer::HANDLED_TOOLS"
        );

        for event in ["SessionStart", "UserPromptSubmit", "PreToolUse"] {
            let cmd = &v["hooks"][event][0]["hooks"][0]["command"];
            assert_eq!(cmd, "semctl hook", "{event} → semctl hook");
            assert!(
                v["hooks"][event][0]["hooks"][0]["timeout"]
                    .as_u64()
                    .is_some_and(|timeout| timeout <= 12),
                "{event} must have a bounded timeout"
            );
            assert!(
                v["hooks"][event][0]["hooks"][0]["statusMessage"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()),
                "{event} should explain its visible work"
            );
        }
        assert!(
            v["hooks"].get("PostCompact").is_none(),
            "SessionStart(source=compact) already restores context"
        );
    }
}
