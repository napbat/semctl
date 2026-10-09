//! `index_codebase` never waits for a first index.
//!
//! The engine of these tests counts reconciles instead of performing them, so
//! a first index stays open until a test finishes its gate. A call that waited
//! for the gate would therefore hit the timeout of the test.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;

use super::{FirstIndexPhase, first_index_report};
use crate::client::Client;
use crate::mcp::tests::{first_index, server};
use crate::mcp::tool_types::IndexCodebaseArgs;
use crate::mcp::{CallBudget, McpServer, initial_gate_for_path};

/// Longer than any call without a wait needs. A call that waits for the open
/// first index never ends inside it.
const NO_WAIT: Duration = Duration::from_secs(1);

/// A checkout on disk, named as `index_codebase` names it.
async fn checkout() -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temporary checkout");
    let dir = crate::codebase::working_copy_root(temp.path()).await;
    (temp, dir)
}

fn session() -> McpServer {
    server(Client::for_test("codebase", None), "launch", false)
}

fn call_for(dir: &Path) -> IndexCodebaseArgs {
    IndexCodebaseArgs {
        path: Some(dir.display().to_string()),
    }
}

/// Register `dir` through a registration that succeeds at once.
async fn start(server: &McpServer, dir: &Path) -> Result<String, crate::query::ToolError> {
    server
        .start_first_index(dir, |_, _| async { Ok("codebase-1".to_string()) })
        .await
}

#[tokio::test]
async fn a_new_first_index_returns_once_the_codebase_is_registered() {
    let (_temp, dir) = checkout().await;
    let server = session();

    let text = tokio::time::timeout(NO_WAIT, start(&server, &dir))
        .await
        .expect("the call must not wait for the first index")
        .expect("the codebase registers");

    assert!(
        text.starts_with("first index started\ncodebase codebase-1\npath "),
        "{text}"
    );
    assert!(
        text.contains("call sync_status every 10 to 15 seconds"),
        "{text}"
    );
    assert!(text.contains("local Read/Grep"), "{text}");
    let gate = initial_gate_for_path(&server.shared.leases, &dir)
        .await
        .expect("the session holds the first-index gate");
    assert_eq!(gate.outcome().await, None, "the first index is still open");
    assert_eq!(gate.phase(None).await, FirstIndexPhase::Syncing);
}

/// `sync_status` reads the coordinator that owns the checkout. It must find
/// the phase of a first index that is still open, and it must not wait.
#[tokio::test]
async fn the_checkout_status_carries_the_phase_of_the_open_first_index() {
    let (_temp, dir) = checkout().await;
    let server = session();
    start(&server, &dir).await.expect("the codebase registers");
    let client = Client::for_test("codebase-1", Some(dir.clone()));

    let status = tokio::time::timeout(NO_WAIT, server.checkout_status(&client))
        .await
        .expect("status must not wait for the first index")
        .expect("the session holds the checkout");

    assert_eq!(status.first_index, Some(FirstIndexPhase::Syncing));
}

#[tokio::test]
async fn a_second_call_reports_the_first_index_in_progress_without_waiting() {
    let (_temp, dir) = checkout().await;
    let server = session();
    start(&server, &dir).await.expect("the codebase registers");

    let text = tokio::time::timeout(
        NO_WAIT,
        server.index_checkout(call_for(&dir), &CallBudget::unbounded()),
    )
    .await
    .expect("a second call must not wait for the first index")
    .expect("a first index in progress is not an error");

    assert!(
        text.starts_with("first index already in progress (syncing)\ncodebase codebase-1\npath "),
        "{text}"
    );
    assert!(
        text.contains("call sync_status every 10 to 15 seconds"),
        "{text}"
    );
}

/// Two calls race for one checkout. The one that does not win the claim must
/// not register a second codebase, and it must not wait for the winner.
#[tokio::test]
async fn a_call_that_loses_the_registration_claim_reports_progress() {
    let (_temp, dir) = checkout().await;
    let server = session();
    let gate = first_index(&server, "codebase", &dir).await;
    assert!(
        gate.claim_registration().await,
        "another call holds the claim"
    );

    let text = tokio::time::timeout(
        NO_WAIT,
        server.start_first_index(&dir, |_, _| async {
            Err(anyhow::anyhow!("a second registration must not run"))
        }),
    )
    .await
    .expect("the call must not wait for the other registration")
    .expect("the other call's first index is not an error");

    assert!(
        text.starts_with("first index already in progress (registering)\npath "),
        "{text}"
    );
    assert!(text.contains("call sync_status"), "{text}");
}

#[tokio::test]
async fn a_completed_first_index_is_reported_complete() {
    let (_temp, dir) = checkout().await;
    let server = session();
    let gate = first_index(&server, "codebase", &dir).await;
    gate.register_codebase("codebase-1".into()).await;
    gate.finish(Ok(())).await;

    let text = tokio::time::timeout(
        NO_WAIT,
        server.index_checkout(call_for(&dir), &CallBudget::unbounded()),
    )
    .await
    .expect("a completed first index needs no wait")
    .expect("a completed first index is not an error");

    assert!(
        text.starts_with("first index complete\ncodebase codebase-1\npath "),
        "{text}"
    );
    assert!(text.ends_with("retrieval tools are available"), "{text}");
}

/// A failed first index stays retryable: the call does not answer from the
/// failed gate, so it goes on to start a new first index.
#[tokio::test]
async fn a_failed_first_index_is_not_reported_so_the_call_retries() {
    let (_temp, dir) = checkout().await;
    let server = session();
    let gate = first_index(&server, "codebase", &dir).await;

    assert!(
        server.earlier_first_index(&dir).await.is_some(),
        "an open first index answers the call"
    );

    gate.finish(Err("embedding job 7 failed".into())).await;

    assert!(
        server.earlier_first_index(&dir).await.is_none(),
        "a failed first index must fall through to a retry"
    );
}

#[tokio::test]
async fn a_failed_registration_is_a_tool_error_and_fails_the_gate() {
    let (_temp, dir) = checkout().await;
    let server = session();

    let error = server
        .start_first_index(&dir, |_, _| async {
            Err(anyhow::anyhow!("the server is unreachable"))
        })
        .await
        .expect_err("a failed registration is an error");

    let message = error.to_string();
    assert!(message.starts_with("index_codebase failed: "), "{message}");
    assert!(message.contains("the server is unreachable"), "{message}");
    let gate = initial_gate_for_path(&server.shared.leases, &dir)
        .await
        .expect("the session holds the first-index gate");
    assert!(
        matches!(gate.outcome().await, Some(Err(reason)) if reason.contains("unreachable")),
        "the gate must report the failure to every later call"
    );
}

#[test]
fn a_call_that_finds_a_failed_first_index_reports_it_with_the_retry_step() {
    let phase = FirstIndexPhase::Failed {
        reason: "embedding job 7 failed".to_string(),
    };

    let message = first_index_report(&phase, Path::new("/work/checkout"), Some("codebase-1"))
        .expect_err("a failed first index is an error")
        .to_string();

    assert!(
        message.starts_with(
            "index_codebase failed: first index failed for /work/checkout \
             (codebase codebase-1): embedding job 7 failed\n"
        ),
        "{message}"
    );
    assert!(
        message.ends_with("next: call index_codebase for this path to retry the first index."),
        "{message}"
    );
}
