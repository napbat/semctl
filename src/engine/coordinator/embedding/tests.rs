use super::*;
use crate::client::stub::{Reply, Stub};

fn job(completed: bool, failed: i64) -> api::JobStatus {
    serde_json::from_value(serde_json::json!({
        "filesToEmbed": 10, "filesEmbedded": 3, "filesFailed": failed,
        "startedAt": "started", "completedAt": completed.then_some("completed"),
    }))
    .expect("valid job fixture")
}

#[test]
fn current_job_evidence_preserves_real_counts_and_requires_terminal_success() {
    let progress = Arc::new(ProgressSlot::new());
    let observer = progress.observe_job("job".to_string());
    assert_eq!(progress.embedding().unwrap().state, EmbeddingState::Queued);
    assert_eq!(observer.observed("job", &job(false, 0)), None);
    let status = progress.embedding().unwrap();
    assert_eq!(status.state, EmbeddingState::Running);
    assert_eq!(status.files_embedded, Some(3));
    assert_eq!(status.files_to_embed, Some(10));
    assert_eq!(observer.observed("job", &job(true, 0)), Some(Ok(())));
    assert_eq!(
        progress.embedding().unwrap().state,
        EmbeddingState::Completed
    );
}

#[test]
fn superseded_completion_and_failure_cannot_replace_the_current_job() {
    let progress = Arc::new(ProgressSlot::new());
    let old = progress.observe_job("old-job".to_string());
    let current = progress.observe_job("current-job".to_string());
    old.finished(&Ok(()));
    old.finished(&Err("late old failure".to_string()));
    let status = progress.embedding().unwrap();
    assert_eq!(status.job_id, "current-job");
    assert_eq!(status.state, EmbeddingState::Queued);
    assert_eq!(status.error, None);
    current.finished(&Ok(()));
    assert_eq!(
        progress.embedding().unwrap().state,
        EmbeddingState::Completed
    );
    // A reused server id still belongs to a new observation generation.
    let _new = progress.observe_job("current-job".to_string());
    current.finished(&Ok(()));
    assert_eq!(progress.embedding().unwrap().state, EmbeddingState::Queued);
}

#[test]
fn partial_failure_and_invalid_progress_are_not_completion() {
    let progress = Arc::new(ProgressSlot::new());
    let observer = progress.observe_job("job".to_string());
    assert!(observer.observed("job", &job(true, 2)).unwrap().is_err());
    let status = progress.embedding().unwrap();
    assert_eq!(status.state, EmbeddingState::Failed);
    assert_eq!(status.files_failed, Some(2));
    assert!(status.error.unwrap().contains("2 failed file(s)"));
    for malformed in [
        (-1, Some(10), 0),
        (11, Some(10), 0),
        (3, Some(-1), 0),
        (3, None, -1),
    ] {
        let mut invalid = job(true, 0);
        (
            invalid.files_embedded,
            invalid.files_to_embed,
            invalid.files_failed,
        ) = malformed;
        assert!(initial_job_result("job", &invalid).unwrap().is_err());
    }
    let mut unknown_total = job(true, 0);
    unknown_total.files_to_embed = None;
    assert_eq!(initial_job_result("job", &unknown_total), Some(Ok(())));
}

#[test]
fn unavailable_observation_retains_its_diagnostic_and_does_not_keep_the_owner_alive() {
    let progress = Arc::new(ProgressSlot::new());
    let observer = progress.observe_job("job".to_string());
    observer.update(|status| status.state = EmbeddingState::Unavailable);
    observer.finished(&Err("job status transport failed".to_string()));
    let status = progress.embedding().unwrap();
    assert_eq!(status.state, EmbeddingState::Unavailable);
    assert_eq!(status.error.as_deref(), Some("job status transport failed"));
    let weak = Arc::downgrade(&progress);
    drop(progress);
    assert!(weak.upgrade().is_none());
    observer.finished(&Ok(()));
}

#[test]
fn embedding_evidence_round_trips_without_making_legacy_fields_required() {
    let progress = Arc::new(ProgressSlot::new());
    let observer = progress.observe_job("job".to_string());
    let _ = observer.observed("job", &job(false, 0));
    let status = progress.embedding().unwrap();
    let wire = serde_json::to_value(&status).expect("serialize job evidence");
    assert_eq!(wire["state"], "running");
    assert_eq!(wire["files_embedded"], 3);
    let decoded: EmbeddingStatus = serde_json::from_value(wire).expect("deserialize job evidence");
    assert_eq!(decoded, status);
}

#[tokio::test]
async fn the_existing_job_waiter_publishes_real_progress_and_stops_at_terminal_completion() {
    let stub = Stub::serve(vec![
        Reply::ok(r#"{"filesToEmbed":10,"filesEmbedded":3,"filesFailed":0,"startedAt":"started"}"#),
        Reply::ok(r#"{"filesToEmbed":10,"filesEmbedded":10,"filesFailed":0,"startedAt":"started","completedAt":"completed"}"#),
    ]).await;
    let client = Client::for_test_server(&stub.url);
    let progress = Arc::new(ProgressSlot::new());
    let observer = progress.observe_job("job".to_string());
    assert_eq!(
        wait_for_embedding_job(&client, "job", &observer).await,
        Ok(())
    );
    let status = progress.embedding().unwrap();
    assert_eq!(status.state, EmbeddingState::Completed);
    assert_eq!(status.files_embedded, Some(10));
    assert_eq!(status.files_to_embed, Some(10));
    assert_eq!(stub.requests(), 2);
}

#[tokio::test]
async fn a_terminal_partial_server_job_retains_its_failure_and_never_reports_completion() {
    let stub = Stub::serve(vec![Reply::ok(
        r#"{"filesToEmbed":10,"filesEmbedded":8,"filesFailed":2,"completedAt":"completed"}"#,
    )])
    .await;
    let client = Client::for_test_server(&stub.url);
    let progress = Arc::new(ProgressSlot::new());
    let observer = progress.observe_job("job".to_string());
    let result = wait_for_embedding_job(&client, "job", &observer).await;
    assert!(result.is_err());
    observer.finished(&result);
    let status = progress.embedding().unwrap();
    assert_eq!(status.state, EmbeddingState::Failed);
    assert_eq!(status.files_failed, Some(2));
    assert!(status.error.unwrap().contains("2 failed file(s)"));
    assert_eq!(stub.requests(), 1);
}
