//! Tests of the first-index wait that a call budget bounds.
//!
//! No test here uses a socket, so each runs on a paused clock: the wait ends
//! at the exact time the rule names, and the test takes no real time.

use std::path::Path;
use std::time::Duration;

use tokio::time::Instant;

use super::{
    FIRST_INDEX_WAIT, FirstIndexPhase, InitialIndexGate, InitialIndexState, NotReady,
    ready_for_codebases,
};
use crate::client::Client;
use crate::mcp::CallBudget;
use crate::mcp::tests::{first_index, server};

/// The clock rounds a timer up to the next millisecond. A wait that ends in this
/// window ended at the time the rule names.
const TIMER_SLACK: Duration = Duration::from_millis(50);

fn bounded(seconds: u64) -> CallBudget {
    CallBudget::until(Instant::now() + Duration::from_secs(seconds))
}

#[tokio::test(start_paused = true)]
async fn a_pending_first_index_is_reported_pending_after_the_short_wait() {
    let server = server(Client::for_test("codebase", None), "launch", false);
    let root = Path::new("pending-checkout");
    let _gate = first_index(&server, "codebase", root).await;
    let started = Instant::now();

    let error = server
        .await_initial_path("search_codebase", root, &bounded(25))
        .await
        .expect_err("the first index is still running");

    let waited = started.elapsed();
    assert!(
        waited >= FIRST_INDEX_WAIT && waited <= FIRST_INDEX_WAIT + TIMER_SLACK,
        "the wait must end after {FIRST_INDEX_WAIT:?}, not at the call deadline: {waited:?}"
    );
    let text = error.to_string();
    assert!(
        text.starts_with(
            "search_codebase failed: the first index of this codebase is still running\n"
        ),
        "{text}"
    );
    assert!(
        text.ends_with(
            "next: call sync_status to follow the first index, and use local Read/Grep until it \
             completes."
        ),
        "{text}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_short_wait_ends_before_a_deadline_that_is_nearer() {
    let server = server(Client::for_test("codebase", None), "launch", false);
    let root = Path::new("pending-checkout");
    let _gate = first_index(&server, "codebase", root).await;
    let started = Instant::now();

    let error = server
        .await_initial_path("search_codebase", root, &bounded(3))
        .await
        .expect_err("the first index is still running");

    // Three seconds minus the margin that keeps the wait from ending at the
    // deadline itself.
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(2500)
            && waited <= Duration::from_millis(2500) + TIMER_SLACK,
        "{waited:?}"
    );
    assert!(
        error.to_string().contains("is still running"),
        "the wait's own error must win over the deadline: {error}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_finished_first_index_is_ready_without_waiting() {
    let server = server(Client::for_test("codebase", None), "launch", false);
    let root = Path::new("ready-checkout");
    let gate = first_index(&server, "codebase", root).await;
    gate.finish(Ok(())).await;
    let started = Instant::now();

    server
        .await_initial_path("search_codebase", root, &bounded(25))
        .await
        .expect("a finished first index is ready");

    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn a_failed_first_index_is_reported_as_failed_and_not_as_pending() {
    let server = server(Client::for_test("codebase", None), "launch", false);
    let root = Path::new("failed-checkout");
    let gate = first_index(&server, "codebase", root).await;
    gate.finish(Err("embedding job 7 failed".into())).await;
    let started = Instant::now();

    let error = server
        .await_initial_path("search_codebase", root, &bounded(25))
        .await
        .expect_err("the first index failed");

    assert_eq!(started.elapsed(), Duration::ZERO);
    assert!(
        error
            .to_string()
            .starts_with("search_codebase failed: initial index failed — embedding job 7 failed\n"),
        "{error}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unbounded_call_waits_for_the_first_index_however_long_it_takes() {
    let server = server(Client::for_test("codebase", None), "launch", false);
    let root = Path::new("slow-checkout");
    let gate = first_index(&server, "codebase", root).await;
    let waiting = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .await_initial_path("rename_symbol", root, &CallBudget::unbounded())
                .await
        }
    });

    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert!(
        !waiting.is_finished(),
        "an unbounded call must keep waiting"
    );

    gate.finish(Ok(())).await;
    waiting
        .await
        .expect("the waiting task")
        .expect("the first index succeeded");
}

#[tokio::test(start_paused = true)]
async fn a_scoped_wait_reports_a_pending_first_index_after_one_short_wait() {
    let server = server(Client::for_test("codebase", None), "launch", false);
    let _gate = first_index(&server, "codebase", Path::new("pending-checkout")).await;
    let started = Instant::now();

    let outcome = ready_for_codebases(&server.shared.leases, &[], &bounded(25)).await;

    assert_eq!(outcome.err(), Some(NotReady::Pending));
    let waited = started.elapsed();
    assert!(
        waited >= FIRST_INDEX_WAIT && waited <= FIRST_INDEX_WAIT + TIMER_SLACK,
        "{waited:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_scoped_wait_serves_a_finished_index_and_reports_a_failed_one() {
    let server = server(Client::for_test("codebase", None), "launch", false);
    let gate = first_index(&server, "codebase", Path::new("checkout")).await;
    gate.finish(Ok(())).await;

    let ready = ready_for_codebases(&server.shared.leases, &[], &bounded(25))
        .await
        .expect("a finished first index is ready");
    assert_eq!(ready.len(), 1);
    drop(ready);

    let other = first_index(&server, "other", Path::new("other-checkout")).await;
    other.finish(Err("embedding failed".into())).await;
    let outcome = ready_for_codebases(&server.shared.leases, &[], &bounded(25)).await;
    assert_eq!(
        outcome.err(),
        Some(NotReady::Failed(
            "initial index failed — embedding failed".into()
        ))
    );
}

fn snapshot(
    codebase: Option<&str>,
    polling: bool,
    result: Option<Result<(), String>>,
) -> InitialIndexState {
    InitialIndexState {
        registering: true,
        polling,
        codebase_id: codebase.map(str::to_string),
        result,
    }
}

fn embedding(job: Option<&str>) -> FirstIndexPhase {
    FirstIndexPhase::Embedding {
        job_id: job.map(str::to_string),
    }
}

fn failed(reason: &str) -> FirstIndexPhase {
    FirstIndexPhase::Failed {
        reason: reason.to_string(),
    }
}

/// The phase is a pure function of one gate snapshot and the coordinator's
/// last job, so each row below is one state that a first index can be in.
#[test]
fn each_gate_state_maps_to_one_phase() {
    let cases = [
        // No codebase yet, whether or not a caller claimed the registration.
        (
            InitialIndexState::default(),
            None,
            FirstIndexPhase::Registering,
        ),
        (
            snapshot(None, false, None),
            None,
            FirstIndexPhase::Registering,
        ),
        // A codebase and no poll: the scan and the upload.
        (
            snapshot(Some("A"), false, None),
            None,
            FirstIndexPhase::Syncing,
        ),
        // A job of an earlier attempt is not the job of this upload.
        (
            snapshot(Some("A"), false, None),
            Some("earlier-job"),
            FirstIndexPhase::Syncing,
        ),
        // A poll claimed: the server embeds the files.
        (
            snapshot(Some("A"), true, None),
            Some("job-1"),
            embedding(Some("job-1")),
        ),
        (snapshot(Some("A"), true, None), None, embedding(None)),
        // A result ends the first index, whatever the other fields say.
        (
            snapshot(Some("A"), true, Some(Ok(()))),
            Some("job-1"),
            FirstIndexPhase::Ready,
        ),
        (
            snapshot(None, false, Some(Err("registration failed".into()))),
            None,
            failed("registration failed"),
        ),
        (
            snapshot(Some("A"), true, Some(Err("embedding failed".into()))),
            Some("job-1"),
            failed("embedding failed"),
        ),
    ];

    for (state, last_job_id, expected) in cases {
        assert_eq!(FirstIndexPhase::of(&state, last_job_id), expected);
    }
}

#[tokio::test]
async fn a_gate_reports_each_phase_as_the_first_index_advances() {
    let gate = InitialIndexGate::pending();
    assert_eq!(gate.phase(None).await, FirstIndexPhase::Registering);
    assert_eq!(gate.codebase().await, None);

    gate.register_codebase("A".into()).await;
    assert_eq!(gate.phase(None).await, FirstIndexPhase::Syncing);
    assert_eq!(gate.codebase().await.as_deref(), Some("A"));

    assert!(gate.claim_poll().await);
    assert_eq!(gate.phase(Some("job-1")).await, embedding(Some("job-1")));

    gate.finish(Ok(())).await;
    assert_eq!(gate.phase(Some("job-1")).await, FirstIndexPhase::Ready);
}

/// The phase never waits. A tool call that asks for it while the first index
/// runs must come back at once.
#[tokio::test(start_paused = true)]
async fn asking_for_the_phase_does_not_wait_for_the_first_index() {
    let gate = InitialIndexGate::pending();

    let phase = tokio::time::timeout(Duration::from_millis(1), gate.phase(None))
        .await
        .expect("the phase is read without waiting");

    assert_eq!(phase, FirstIndexPhase::Registering);
}
