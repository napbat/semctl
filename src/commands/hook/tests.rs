use super::*;

// Claude Code sends the hook payload in snake_case (`hook_event_name`,
// `tool_name`, `transcript_path`, …). A camelCase-only struct silently
// fails to bind `hook_event_name`, so dispatch sees "" and the hook is a
// no-op — which is exactly how the original bug hid behind the
// never-break-a-session contract.
#[test]
fn parses_real_snake_case_payload() {
    let raw = r#"{"hook_event_name":"SessionStart","prompt":"how does auth work","cwd":"/repo"}"#;
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

#[test]
fn indexed_orientation_keeps_discovery_signal_without_slugs() {
    let message = indexed_orientation();
    assert!(message.starts_with("This repository is indexed by semctl."));
    assert!(message.contains("`search_codebase`"));
    assert!(message.contains("repository discovery"));
    assert!(message.contains("Omit `codebase` for this checkout"));
    assert!(message.contains("immutable codebase ID or local directory path"));
    assert!(!message.contains("indexed by semctl as"));
    assert!(!message.contains("slug"));
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

#[test]
fn claude_prompt_id_selects_plugin_scoped_tool_names() {
    let raw = r#"{"hook_event_name":"PreToolUse","session_id":"s","prompt_id":"p1","cwd":"/repo","tool_name":"Bash","tool_input":{"command":"rg needle ."}}"#;
    let input: HookInput = serde_json::from_str(raw).expect("Claude payload parses");
    assert_eq!(input.turn_key(), "p1");
    assert_eq!(
        input.tool_name_style(),
        message::ToolNameStyle::ClaudePlugin
    );
}

#[test]
fn recognizes_each_hosts_semctx_tool_namespace() {
    assert!(is_semctx_tool_name("mcp__semctx__search_codebase"));
    assert!(is_semctx_tool_name(
        "mcp__plugin_semctx_semctx__search_codebase"
    ));
    assert!(is_semctx_tool_name("mcp__semctx_semctx_find_definition"));
    assert!(!is_semctx_tool_name("Grep"));
    assert!(!is_semctx_tool_name("mcp__other__search_codebase"));
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
    assert_eq!(input.tool_name_style(), message::ToolNameStyle::CodexPlugin);
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
    assert!(
        !input.is_codex(),
        "prompt_id precedence also controls host-specific output"
    );
    assert_eq!(
        input.tool_name_style(),
        message::ToolNameStyle::ClaudePlugin
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
    max: 4,
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
fn compliance_cools_immediate_searches_then_rearms() {
    let t = temp_store();
    let mut st = state::NudgeState::default();
    st.remember_prompt("p1");
    st.record_semctx_use("p1");
    t.store.save("s1", &st);

    assert!(compliance_suppresses(&t.store, "s1", "p1", 3));
    assert!(compliance_suppresses(&t.store, "s1", "p1", 3));
    assert!(!compliance_suppresses(&t.store, "s1", "p1", 3));

    assert!(
        advance(&t.store, "s1", "p1", &T).is_none(),
        "normal grace applies after the compliance streak"
    );
    let (st, tier) = advance(&t.store, "s1", "p1", &T).expect("fourth later search re-nudges");
    assert_eq!(tier, escalation::Tier::One);
    assert_eq!(st.eligible_count, 2);
}

#[test]
fn new_prompt_rearms_compliance_with_normal_grace() {
    let t = temp_store();
    let mut st = state::NudgeState::default();
    st.record_semctx_use("p1");
    t.store.save("s1", &st);

    assert!(!compliance_suppresses(&t.store, "s1", "p2", 3));
    assert!(advance(&t.store, "s1", "p2", &T).is_none());
    assert_eq!(t.store.load("s1").eligible_count, 1);
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
        message::ToolNameStyle::CodexPlugin,
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
        message::ToolNameStyle::CodexPlugin,
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
        message::ToolNameStyle::CodexPlugin,
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

struct ScopeFixture {
    base: PathBuf,
    repo: PathBuf,
    cwd: PathBuf,
    inrepo_dir: PathBuf,
    inrepo_file: PathBuf,
    outside_file: PathBuf,
}

impl Drop for ScopeFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn scope_fixture() -> ScopeFixture {
    let base = std::env::temp_dir().join(format!(
        "semctl-scope-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let repo = base.join("repo");
    let cwd = repo.join("crate").join("src"); // host launched in a subdir
    let inrepo_dir = repo.join("server");
    let outside_dir = base.join("elsewhere");
    for dir in [
        repo.join(".git"),
        cwd.clone(),
        inrepo_dir.clone(),
        outside_dir.clone(),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
    }
    let inrepo_file = repo.join("server.rs");
    let outside_file = outside_dir.join("notes.txt");
    for file in [
        cwd.join("lib.rs"),
        inrepo_file.clone(),
        outside_file.clone(),
    ] {
        std::fs::write(file, "needle").unwrap();
    }
    ScopeFixture {
        base,
        repo,
        cwd,
        inrepo_dir,
        inrepo_file,
        outside_file,
    }
}

#[test]
fn search_scope_distinguishes_broad_single_file_and_outside_targets() {
    use serde_json::json;
    let fixture = scope_fixture();
    let cwd = fixture.cwd.to_str().unwrap();
    let repo = fixture.repo.clone();
    let inrepo_dir = fixture.inrepo_dir.clone();
    let inrepo_file = fixture.inrepo_file.clone();
    let outside_file = fixture.outside_file.clone();
    let abs = |path: &std::path::Path| path.to_str().unwrap().to_string();

    assert_eq!(
        search_scope("Grep", &json!({}), cwd),
        SearchScope::BroadOrUnknown
    );
    assert_eq!(
        search_scope("Grep", &json!({ "path": "missing.rs" }), cwd),
        SearchScope::BroadOrUnknown
    );
    assert_eq!(
        search_scope("Grep", &json!({ "path": abs(&inrepo_dir) }), cwd),
        SearchScope::BroadOrUnknown
    );
    assert_eq!(
        search_scope("Grep", &json!({ "path": "lib.rs" }), cwd),
        SearchScope::SingleFile
    );
    assert_eq!(
        search_scope("Grep", &json!({ "path": abs(&inrepo_file) }), cwd),
        SearchScope::SingleFile
    );
    assert_eq!(
        search_scope("Grep", &json!({ "path": abs(&outside_file) }), cwd),
        SearchScope::OutsideRepo
    );
    assert_eq!(
        search_scope(
            "Bash",
            &json!({ "command": format!("rg needle {}", abs(&inrepo_file)) }),
            cwd,
        ),
        SearchScope::SingleFile
    );
    assert_eq!(
        search_scope(
            "Bash",
            &json!({ "command": format!("rg -n needle {}", abs(&inrepo_file)) }),
            cwd,
        ),
        SearchScope::SingleFile
    );
    assert_eq!(
        search_scope(
            "Bash",
            &json!({
                "command": format!("rg --glob '*.rs' {}", abs(&outside_file))
            }),
            cwd,
        ),
        SearchScope::BroadOrUnknown
    );
    assert_eq!(
        search_scope(
            "Bash",
            &json!({ "command": format!("rg needle {}", abs(&outside_file)) }),
            cwd,
        ),
        SearchScope::OutsideRepo
    );
    assert_eq!(
        search_scope(
            "Bash",
            &json!({ "command": format!("rg needle {} | sort", abs(&inrepo_file)) }),
            cwd,
        ),
        SearchScope::BroadOrUnknown
    );
    assert_eq!(
        search_scope(
            "Glob",
            &json!({ "path": abs(&repo), "pattern": "server.rs" }),
            cwd,
        ),
        SearchScope::SingleFile
    );
    assert_eq!(
        search_scope(
            "Glob",
            &json!({ "path": abs(&repo), "pattern": "*.rs" }),
            cwd,
        ),
        SearchScope::BroadOrUnknown
    );
    assert_eq!(
        search_scope("Grep", &json!({ "path": abs(&outside_file) }), ""),
        SearchScope::BroadOrUnknown
    );
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
fn prompt_retrieval_gate_routes_generic_and_precise_intents() {
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
        assert_eq!(
            prompt_route(prompt),
            PromptRoute::CandidateSearch,
            "{prompt}"
        );
    }
    for prompt in evals["hook_injection"]["model_routed"]
        .as_array()
        .expect("model_routed cases")
    {
        let prompt = prompt.as_str().expect("prompt string");
        assert_eq!(prompt_route(prompt), PromptRoute::ModelRouted, "{prompt}");
    }
    for prompt in evals["hook_injection"]["should_not_search"]
        .as_array()
        .expect("should_not_search cases")
    {
        let prompt = prompt.as_str().expect("prompt string");
        assert_eq!(prompt_route(prompt), PromptRoute::None, "{prompt}");
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
    assert_eq!(pre.len(), 2, "search routing + silent semctx compliance");
    let search_entry = &pre[0];
    assert_eq!(
        search_entry["hooks"][0]["command"], "semctl hook",
        "PreToolUse search → semctl hook"
    );

    let matcher = search_entry["matcher"]
        .as_str()
        .expect("search matcher is a string");
    let mut got: Vec<&str> = matcher.split('|').collect();
    got.sort_unstable();
    let mut want: Vec<&str> = sniffer::HANDLED_TOOLS.to_vec();
    want.sort_unstable();
    assert_eq!(
        got, want,
        "hooks.json search matcher drifted from sniffer::HANDLED_TOOLS"
    );

    let compliance_entry = &pre[1];
    assert_eq!(
        compliance_entry["matcher"],
        "mcp__semctx__.*|mcp__plugin_semctx_semctx__.*"
    );
    assert_eq!(compliance_entry["hooks"][0]["command"], "semctl hook");
    assert_eq!(compliance_entry["hooks"][0]["timeout"], 2);
    assert!(
        compliance_entry["hooks"][0].get("statusMessage").is_none(),
        "compliance recording stays invisible"
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
