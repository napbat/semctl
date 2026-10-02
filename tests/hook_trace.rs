use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Run one offline, logged-out `semctl hook` with tracing enabled.
async fn hook(directory: &Path, trace: &Path, input: Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_semctl"))
        .args(["--server", "http://127.0.0.1:9", "hook"])
        .current_dir(directory)
        .env("XDG_CONFIG_HOME", directory)
        .env("TMPDIR", directory)
        .env("TMP", directory)
        .env("TEMP", directory)
        .env_remove("SEMCTX_HOOK_DISABLE")
        .env_remove("SEMCTX_NUDGE_DISABLE")
        .env_remove("SEMCTX_TOKEN")
        .env("SEMCTX_HOOK_UPDATE_CHECK", "0")
        .env("SEMCTX_NUDGE_GRACE", "0")
        .env("SEMCTX_HOOK_TRACE", trace)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start hook");
    let mut stdin = child.stdin.take().expect("piped hook stdin");
    stdin
        .write_all(&serde_json::to_vec(&input).expect("serialize hook input"))
        .await
        .expect("write hook input");
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("hook timed out")
        .expect("wait for hook");
    assert!(status.success());
}

fn tool_event(directory: &Path, tool_name: &str, tool_input: &Value) -> Value {
    json!({
        "host": "omp",
        "hook_event_name": "PreToolUse",
        "session_id": "trace-session",
        "prompt_id": "prompt-1",
        "cwd": directory,
        "tool_name": tool_name,
        "tool_input": tool_input,
    })
}

#[tokio::test]
async fn trace_records_each_decision_without_private_input() {
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path();
    std::fs::create_dir(directory.join(".git")).unwrap();
    std::fs::create_dir(directory.join("src")).unwrap();
    std::fs::write(directory.join("src").join("main.rs"), "fn main() {}\n").unwrap();
    let trace = directory.join("trace.jsonl");

    hook(
        directory,
        &trace,
        json!({
            "host": "omp",
            "hook_event_name": "SessionStart",
            "session_id": "trace-session",
            "cwd": directory,
            "source": "startup",
        }),
    )
    .await;
    for (tool, input) in [
        (
            "Grep",
            json!({ "pattern": "secret_needle", "path": "src/main.rs" }),
        ),
        ("Read", json!({ "path": "src/main.rs" })),
        ("Glob", json!({ "pattern": "**/*.rs" })),
    ] {
        hook(directory, &trace, tool_event(directory, tool, &input)).await;
    }

    let text = std::fs::read_to_string(&trace).expect("read trace");
    assert!(
        !text.contains("secret_needle") && !text.contains("main.rs"),
        "the trace must not record tool input: {text}"
    );
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("trace line is JSON"))
        .collect();
    assert_eq!(
        records,
        vec![
            json!({ "event": "SessionStart", "host": "omp", "tool": "", "context": true }),
            json!({ "event": "PreToolUse", "host": "omp", "tool": "Grep", "outcome": "single_file", "context": false }),
            json!({ "event": "PreToolUse", "host": "omp", "tool": "Read", "outcome": "not_search", "context": false }),
            json!({ "event": "PreToolUse", "host": "omp", "tool": "Glob", "outcome": "unavailable", "context": false }),
        ]
    );
}
