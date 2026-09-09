use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const SERVER_INSTRUCTIONS: &str = include_str!("../src/mcp/docs/instructions/server.md");

async fn hook(directory: &Path, session: &str, event: &str, source: &str) -> Option<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_semctl"))
        .args(["--server", "http://127.0.0.1:9", "hook"])
        .current_dir(directory)
        .env("XDG_CONFIG_HOME", directory)
        .env("TMPDIR", directory)
        .env("TMP", directory)
        .env("TEMP", directory)
        .env_remove("SEMCTX_HOOK_DISABLE")
        .env_remove("SEMCTX_NUDGE_DISABLE")
        .env("SEMCTX_HOOK_UPDATE_CHECK", "0")
        .env("SEMCTX_HOOK_TOP_K", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start hook");
    let input = json!({
        "hook_event_name": event,
        "source": source,
        "session_id": session,
        "turn_id": "turn",
        "cwd": directory,
        "prompt": "Explain this supplied text.",
        "tool_name": "mcp__semctx__grep",
        "tool_input": { "pattern": "needle" }
    });
    let mut stdin = child.stdin.take().expect("piped hook stdin");
    stdin
        .write_all(&serde_json::to_vec(&input).expect("serialize hook input"))
        .await
        .expect("write hook input");
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("hook timed out")
        .expect("wait for hook");
    assert!(output.status.success());
    if output.stdout.is_empty() {
        return None;
    }
    let output: Value = serde_json::from_slice(&output.stdout).expect("parse hook output");
    assert_eq!(output["hookSpecificOutput"]["hookEventName"], event);
    Some(
        output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("hook context")
            .to_string(),
    )
}

async fn assert_manual_copies(
    directory: &Path,
    session: &str,
    event: &str,
    source: &str,
    expected: usize,
) {
    let context = hook(directory, session, event, source).await;
    assert_eq!(
        context
            .as_deref()
            .unwrap_or_default()
            .matches(SERVER_INSTRUCTIONS)
            .count(),
        expected,
        "{event} ({source}) must deliver the expected number of manual copies"
    );
}

#[tokio::test]
async fn startup_emits_the_manual_once_without_a_server_connection() {
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path();
    assert_manual_copies(directory, "session", "SessionStart", "startup", 1).await;
    for (event, source) in [
        ("SessionStart", "startup"),
        ("SessionStart", "resume"),
        ("UserPromptSubmit", ""),
        ("PreToolUse", ""),
        ("PreToolUse", ""),
        ("UserPromptSubmit", ""),
    ] {
        assert_manual_copies(directory, "session", event, source, 0).await;
    }
    assert_manual_copies(directory, "other-session", "SessionStart", "startup", 1).await;
}

#[tokio::test]
async fn context_resets_restore_the_manual_once() {
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path();
    // Prompt delivery also covers a session whose startup hook was missed.
    assert_manual_copies(directory, "session", "UserPromptSubmit", "", 1).await;
    for source in ["clear", "compact"] {
        assert_manual_copies(directory, "session", "SessionStart", source, 1).await;
        assert_manual_copies(directory, "session", "UserPromptSubmit", "", 0).await;
    }
    assert_eq!(hook(directory, "session", "PostCompact", "").await, None);
    assert_eq!(hook(directory, "session", "PreToolUse", "").await, None);
    assert_manual_copies(directory, "session", "UserPromptSubmit", "", 1).await;
    assert_manual_copies(directory, "session", "UserPromptSubmit", "", 0).await;
}

#[tokio::test]
async fn hooks_without_session_identity_do_not_repeat_the_manual() {
    let directory = tempfile::tempdir().unwrap();
    for event in ["SessionStart", "UserPromptSubmit", "PreToolUse"] {
        assert_manual_copies(directory.path(), "", event, "startup", 0).await;
    }
}
