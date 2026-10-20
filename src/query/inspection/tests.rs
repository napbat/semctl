//! Structural results retain source bytes and relationships in Markdown hosts.

use super::{
    JobPhase, current_sync_line, first_index_line, job_phase, render_job, sync_status, sync_summary,
};
use crate::client::Client;
use crate::client::stub::{Reply, Stub};
use crate::engine::CoordinatorStatus;
use crate::engine::coordinator::WatcherState;
use crate::mcp::readiness::FirstIndexPhase;
use crate::query::render::{
    markdown_cell, markdown_code, render_fenced, render_outline, render_projects,
};
use crate::sync::SyncProgress;

fn fenced_body(text: &str) -> &str {
    let (opener, rest) = text.split_once('\n').expect("a fenced block has an opener");
    let fence = &opener[..opener.bytes().take_while(|byte| *byte == b'`').count()];
    assert!(fence.len() >= 3);
    let body = rest
        .strip_suffix(&format!("{fence}\n"))
        .expect("the matching closer terminates the block");
    assert!(
        !body.lines().any(|line| {
            let line = line.trim();
            line.len() >= fence.len() && line.bytes().all(|byte| byte == b'`')
        }),
        "body delimiters cannot terminate the block early"
    );
    body
}

fn first_fenced_block(text: &str) -> &str {
    let mut start = 0;
    for opener in text.split_inclusive('\n') {
        let fence_len = opener.bytes().take_while(|byte| *byte == b'`').count();
        if fence_len >= 3 {
            let fence = &opener[..fence_len];
            let mut end = start + opener.len();
            for line in text[end..].split_inclusive('\n') {
                end += line.len();
                if line.trim_end_matches(['\r', '\n']) == fence {
                    return &text[start..end];
                }
            }
            panic!("missing matching fence closer: {text}");
        }
        start += opener.len();
    }
    panic!("missing fenced block: {text}");
}

#[tokio::test]
async fn batch_results_keep_unmatched_and_multiple_locations_attributed() {
    let results = serde_json::json!([
        {"symbol": "missing|symbol", "hits": []},
        {"symbol": "lookup", "hits": [
            {"domainId": "code", "id": "first", "score": 0.0,
             "path": "z|source.rs", "lineStart": 7, "lineEnd": 9, "snippet": ""},
            {"domainId": "code", "id": "second", "score": 0.0,
             "path": "a.rs", "lineStart": 12, "lineEnd": 15, "snippet": ""}
        ]},
        {"symbol": "last", "hits": [
            {"domainId": "code", "id": "third", "score": 0.0,
             "path": "b.rs", "lineStart": 30, "lineEnd": 31, "snippet": ""}
        ]}
    ]);
    for references in [false, true] {
        let stub = Stub::serve(vec![Reply::ok(&results.to_string())]).await;
        let client = Client::for_test_server(&stub.url).with_codebase("A".into());
        let output = super::batch_lookup(
            &client,
            &["missing|symbol".into(), "lookup".into(), "last".into()],
            references,
            crate::query::QueryPresentation::Inspector,
        )
        .await
        .unwrap();
        let display = output.display.as_ref().unwrap();
        assert_eq!(display["symbols"][0]["symbol"], "missing|symbol");
        assert_eq!(display["symbols"][0]["hits"], serde_json::json!([]));
        assert_eq!(
            display["symbols"][1]["hits"][0]["binding"]["state"],
            "not-probed"
        );
        let text = output.text;
        let kind = if references {
            "references"
        } else {
            "definitions"
        };
        let summary = text.lines().next().unwrap();
        assert!(summary.contains("3 symbols"), "{text}");
        assert!(summary.contains(&format!("3 {kind}")), "{text}");
        assert!(summary.contains("1 unmatched"), "{text}");

        let missing = markdown_cell("missing|symbol");
        let missing_row = text.lines().find(|line| line.contains(&missing)).unwrap();
        assert!(missing_row.contains(&format!("No {kind}")), "{text}");
        let mut previous = text.find(&missing).unwrap();
        for (symbol, location) in [
            ("lookup", "z|source.rs:7-9"),
            ("lookup", "a.rs:12-15"),
            ("last", "b.rs:30-31"),
        ] {
            let location = markdown_cell(location);
            let row = text.lines().find(|line| line.contains(&location)).unwrap();
            assert!(row.contains(&markdown_cell(symbol)), "{row}");
            let position = text.find(&location).unwrap();
            assert!(position > previous, "server order must survive: {text}");
            previous = position;
        }
    }
}

#[test]
fn a_new_first_index_attempt_supersedes_an_older_job_until_embedding() {
    let mut job = completed_job();
    job.error = Some("older attempt failed".into());
    for phase in [FirstIndexPhase::Registering, FirstIndexPhase::Syncing] {
        let status = checkout(Some(phase), None);
        let summary = sync_summary(Some(&status), Some(&job), false);
        assert!(summary.contains("not ready"), "{summary}");
        assert!(!summary.contains("Server indexing failed"), "{summary}");
    }
    let status = checkout(
        Some(FirstIndexPhase::Embedding {
            job_id: Some("current-job".into()),
        }),
        None,
    );
    assert!(sync_summary(Some(&status), Some(&job), false).contains("Server indexing failed"));
}

const EMPTY_CATALOG_PAGE: &str = r#"{"success":true,"data":[],"page":0,"pageSize":1000,"total":0}"#;

fn checkout(
    first_index: Option<FirstIndexPhase>,
    sync_progress: Option<SyncProgress>,
) -> CoordinatorStatus {
    CoordinatorStatus {
        root: "checkout".into(),
        codebase_id: Some("A".to_string()),
        leases: 1,
        watcher: WatcherState::Active,
        last_job_id: None,
        running: sync_progress.is_some(),
        pending_triggers: 0,
        trigger_overflow: false,
        last_outcome: None,
        last_error: None,
        first_index,
        sync_progress,
        retry_after_secs: None,
        embedding: None,
    }
}

async fn catalog_stub() -> (Stub, Client) {
    let stub = Stub::serve(vec![Reply::Answer {
        status: "200 OK",
        headers: Vec::new(),
        body: EMPTY_CATALOG_PAGE.to_string(),
    }])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".to_string());
    (stub, client)
}

#[test]
fn shared_progress_preserves_counters_without_inventing_totals() {
    for progress in [
        SyncProgress::Preparing,
        SyncProgress::Scanning {
            root: "checkout".into(),
        },
        SyncProgress::Planning {
            files: 3607,
            cached_files: 3500,
        },
        SyncProgress::Uploading {
            uploaded_files: 1200,
            total_files: 3607,
        },
        SyncProgress::Finalizing,
    ] {
        assert!(current_sync_line(&progress).ends_with(&progress.to_string()));
    }
}

#[tokio::test]
async fn pending_first_index_phases_remain_visible_without_a_server_job() {
    let (_stub, client) = catalog_stub().await;
    for phase in [
        FirstIndexPhase::Registering,
        FirstIndexPhase::Syncing,
        FirstIndexPhase::Embedding {
            job_id: Some("job-7".into()),
        },
        FirstIndexPhase::Embedding { job_id: None },
    ] {
        let text = sync_status(
            &client,
            Some(&checkout(Some(phase.clone()), None)),
            crate::query::QueryPresentation::Plain,
        )
        .await
        .expect("status queries succeed while indexing is pending")
        .text;
        assert!(text.lines().next().unwrap().contains("not ready"), "{text}");
        assert!(text.contains(&first_index_line(&phase)), "{text}");
        assert!(!text.starts_with("```"), "{text}");
    }
}

#[tokio::test]
async fn running_upload_counters_are_visible_in_the_compact_preview() {
    let (_stub, client) = catalog_stub().await;
    let text = sync_status(
        &client,
        Some(&checkout(
            Some(FirstIndexPhase::Syncing),
            Some(SyncProgress::Uploading {
                uploaded_files: 1200,
                total_files: 3607,
            }),
        )),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    let preview = text.lines().next().unwrap();
    assert!(preview.contains("1200/3607"), "{text}");
    assert!(preview.contains("running"), "{text}");
    assert!(!preview.contains('%'), "{text}");
}

#[tokio::test]
async fn a_running_resync_does_not_gain_a_first_index_gate() {
    let (_stub, client) = catalog_stub().await;
    let text = sync_status(
        &client,
        Some(&checkout(None, Some(SyncProgress::Finalizing))),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    assert!(
        text.contains(&SyncProgress::Finalizing.to_string()),
        "{text}"
    );
    assert!(!text.contains("First index:"), "{text}");
}

#[tokio::test]
async fn a_failed_first_index_keeps_its_raw_reason_and_explicit_retry() {
    let (_stub, client) = catalog_stub().await;
    let reason = "embedding failed\n`````\n# literal reason";
    let text = sync_status(
        &client,
        Some(&checkout(
            Some(FirstIndexPhase::Failed {
                reason: reason.into(),
            }),
            None,
        )),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .expect("a successful status query can report indexing failure")
    .text;
    assert!(text.lines().next().unwrap().contains("failed"), "{text}");
    assert_eq!(
        fenced_body(first_fenced_block(&text)),
        format!("{reason}\n")
    );
    assert!(
        text.lines().next().unwrap().contains("embedding failed"),
        "the compact preview exposes the first-index diagnostic: {text}"
    );
    assert!(text.contains("`index_codebase`"), "{text}");
    assert!(text.contains("not ready"), "{text}");
}

#[tokio::test]
async fn a_local_sync_failure_reports_stale_results_and_the_retry_delay() {
    let (_stub, client) = catalog_stub().await;
    let mut status = checkout(None, None);
    status.last_error = Some("sync plan: gateway unavailable".into());
    status.retry_after_secs = Some(10);
    let text = sync_status(
        &client,
        Some(&status),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    assert!(text.lines().next().unwrap().contains("failed"), "{text}");
    assert!(
        text.contains(status.last_error.as_deref().unwrap()),
        "{text}"
    );
    assert!(text.contains("may be stale"), "{text}");
    assert!(text.contains("next local sync attempt in 10 s"), "{text}");
    assert!(
        text.contains("No server job queued; the local sync failed"),
        "{text}"
    );
}

fn completed_job() -> crate::client::api::JobStatus {
    serde_json::from_value(serde_json::json!({
        "completedAt": "2026-10-01T00:00:00Z",
        "filesEmbedded": 0, "filesDeleted": 0, "filesFailed": 0
    }))
    .unwrap()
}

#[test]
fn job_failures_keep_raw_diagnostics() {
    let mut job: crate::client::api::JobStatus = serde_json::from_value(serde_json::json!({
        "filesToEmbed": 10, "filesToDelete": 2, "filesEmbedded": 4,
        "filesDeleted": 2, "filesFailed": 3, "startedAt": "started"
    }))
    .unwrap();
    assert_eq!(job_phase(&job), JobPhase::Failed);
    let diagnostic = "\r\n```text\r\n# raw diagnostic\r\n";
    job.error = Some(diagnostic.into());
    let text = render_job("job`\n# hidden", &job);
    assert!(text.contains(&format!("\n````text\n{diagnostic}````\n")));
    assert!(!text.contains("\n# hidden"));
}

#[test]
fn job_phases_keep_queued_running_and_completed_distinct() {
    let mut job: crate::client::api::JobStatus = serde_json::from_value(serde_json::json!({
        "filesEmbedded": 0, "filesFailed": 0
    }))
    .unwrap();
    assert_eq!(job_phase(&job), JobPhase::Pending);
    job.started_at = Some("started".into());
    assert_eq!(job_phase(&job), JobPhase::Running);
    job.completed_at = Some("completed".into());
    assert_eq!(job_phase(&job), JobPhase::Done);
}

#[test]
fn missing_job_counts_are_unknown_and_only_explicit_zero_plan_is_no_work() {
    let missing = completed_job();
    assert_eq!(missing.files_to_embed, None);
    assert_eq!(missing.files_to_delete, None);
    assert_eq!(missing.files_deleted, Some(0));
    let text = render_job("missing-plan", &missing);
    assert!(text.contains("planned embed unknown"), "{text}");
    assert!(text.contains("planned delete unknown"), "{text}");
    assert!(!text.contains("no planned work"), "{text}");

    let mut zero: crate::client::api::JobStatus = serde_json::from_value(serde_json::json!({
        "completedAt": "completed",
        "filesEmbedded": 0,
        "filesFailed": 0,
        "filesToEmbed": 0,
        "filesToDelete": 0
    }))
    .unwrap();
    assert_eq!(zero.files_to_embed, Some(0));
    assert_eq!(zero.files_to_delete, Some(0));
    assert_eq!(zero.files_deleted, None);
    let text = render_job("zero-plan", &zero);
    assert!(text.contains("deleted unknown"), "{text}");
    assert!(text.contains("no planned work"), "{text}");
    zero.files_to_delete = None;
    assert!(!render_job("partial-plan", &zero).contains("no planned work"));
}

#[test]
fn terminal_job_counts_are_named_evidence_not_a_completion_ratio() {
    let mut job: crate::client::api::JobStatus = serde_json::from_value(serde_json::json!({
        "filesToEmbed": 7,
        "filesEmbedded": 1,
        "filesFailed": 0,
        "startedAt": "started",
        "completedAt": "completed"
    }))
    .unwrap();
    assert_eq!(job_phase(&job), JobPhase::Done);
    let text = render_job("job", &job);
    assert!(text.contains("embedded 1"), "{text}");
    assert!(text.contains("planned embed 7"), "{text}");
    assert!(!text.contains("1/7"), "{text}");
    assert!(!text.contains("processed"), "{text}");
    assert!(!text.contains("no planned work"), "{text}");
    job.completed_at = None;
    assert_eq!(job_phase(&job), JobPhase::Running);
}

#[test]
fn local_failure_and_active_retry_take_precedence_over_previous_jobs() {
    let mut job = completed_job();
    let mut status = checkout(Some(FirstIndexPhase::Ready), None);
    status.last_error = Some("scan access denied".into());
    assert!(sync_summary(Some(&status), Some(&job), false).contains("Local sync failed"));
    status.first_index = Some(FirstIndexPhase::Failed {
        reason: "no usable index".into(),
    });
    assert!(sync_summary(Some(&status), Some(&job), false).contains("First index failed"));
    status.first_index = None;
    status.last_error = None;
    status.running = true;
    job.error = Some("previous job failed".into());
    assert!(sync_summary(Some(&status), Some(&job), false).contains("Local sync running"));
}

#[test]
fn pending_and_failed_jobs_never_count_as_completed_indexing() {
    let mut job = completed_job();
    job.files_failed = 1;
    assert!(sync_summary(None, Some(&job), false).contains("failed"));
    job.files_failed = 0;
    job.error = Some("worker failed".into());
    assert!(sync_summary(None, Some(&job), false).contains("failed"));
    job.error = None;
    job.completed_at = None;
    assert!(sync_summary(None, Some(&job), false).contains("queued"));
    job.started_at = Some("2026-10-01T00:00:00Z".into());
    assert!(sync_summary(None, Some(&job), false).contains("running"));
    job.completed_at = Some("2026-10-01T00:01:00Z".into());
    let mut status = checkout(Some(FirstIndexPhase::Ready), None);
    status.pending_triggers = 1;
    assert!(sync_summary(Some(&status), Some(&job), false).contains("pending"));
    status.pending_triggers = 0;
    status.trigger_overflow = true;
    assert!(sync_summary(Some(&status), Some(&job), false).contains("pending"));
}

#[test]
fn completion_does_not_claim_checkout_freshness_and_missing_state_is_unknown() {
    let job = completed_job();
    assert!(sync_summary(None, Some(&job), false).contains("freshness not established"));
    assert!(sync_summary(None, Some(&job), true).contains("unknown"));
    assert!(sync_summary(None, None, false).contains("unknown"));
}

#[tokio::test]
async fn a_previous_completed_job_does_not_hide_a_local_sync_failure() {
    let stub = Stub::serve(vec![
        Reply::Answer {
            status: "200 OK", headers: Vec::new(), body: EMPTY_CATALOG_PAGE.into(),
        },
        Reply::ok(r#"{"id":"old-job","completedAt":"2026-10-01T00:00:00Z","filesEmbedded":0,"filesDeleted":0,"filesFailed":0,"error":null}"#),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let mut status = checkout(None, None);
    status.last_job_id = Some("old-job".into());
    status.last_error = Some("upload failed".into());
    let text = sync_status(
        &client,
        Some(&status),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    assert!(
        text.lines().next().unwrap().contains("Local sync failed"),
        "{text}"
    );
    assert!(text.contains("upload failed"), "{text}");
    assert!(text.contains("may be stale"), "{text}");
    assert!(text.contains("old-job"), "{text}");
    assert!(!text.contains("status could not be read"), "{text}");
}

#[tokio::test]
async fn a_missing_watcher_is_not_reported_as_active() {
    let (_stub, client) = catalog_stub().await;
    let mut status = checkout(None, None);
    status.watcher = WatcherState::Unavailable("watch limit reached".into());
    let text = sync_status(
        &client,
        Some(&status),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    assert!(text.contains("not active (watch limit reached)"), "{text}");
    assert!(
        !text.lines().next().unwrap().contains("completed"),
        "{text}"
    );
}

#[tokio::test]
async fn catalog_failure_is_unknown_not_an_empty_catalog() {
    let stub = Stub::serve(vec![Reply::Answer {
        status: "503 Service Unavailable",
        headers: Vec::new(),
        body: "gateway unavailable".into(),
    }])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let output = sync_status(&client, None, crate::query::QueryPresentation::Inspector)
        .await
        .unwrap();
    let display = output.display.as_ref().unwrap();
    assert!(display["catalog"].is_null());
    assert!(
        display["catalogError"]
            .as_str()
            .unwrap()
            .contains("gateway unavailable")
    );
    let text = output.text;
    assert!(text.lines().next().unwrap().contains("unknown"), "{text}");
    assert!(!text.contains("0 files"), "{text}");
    assert!(text.contains("`sync_status` again"), "{text}");
}

#[tokio::test]
async fn unreadable_server_job_is_unknown_not_successful_or_failed() {
    let stub = Stub::serve(vec![
        Reply::Answer {
            status: "200 OK",
            headers: Vec::new(),
            body: EMPTY_CATALOG_PAGE.into(),
        },
        Reply::Answer {
            status: "503 Service Unavailable",
            headers: Vec::new(),
            body: "job endpoint unavailable".into(),
        },
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let mut status = checkout(Some(FirstIndexPhase::Ready), None);
    status.last_job_id = Some("job-7".into());
    let text = sync_status(
        &client,
        Some(&status),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    assert!(text.lines().next().unwrap().contains("unknown"), "{text}");
    assert!(text.contains("job-7"), "{text}");
    assert!(text.contains("status could not be read"), "{text}");
}

#[tokio::test]
async fn no_op_job_does_not_replace_persisted_catalog_totals() {
    let stub = Stub::serve(vec![
        Reply::Answer {
            status: "200 OK", headers: Vec::new(),
            body: r#"{"success":true,"data":[{"path":"source.rs","size":2048}],"page":0,"pageSize":1000,"total":1}"#.into(),
        },
        Reply::ok(r#"{"completedAt":"2026-10-01T00:00:00Z","filesToEmbed":0,"filesToDelete":0,"filesEmbedded":0,"filesDeleted":0,"filesFailed":0,"chunkCount":91234567}"#),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let mut status = checkout(None, None);
    status.last_job_id = Some("no-op".into());
    let text = sync_status(
        &client,
        Some(&status),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    assert!(text.contains("1 files"), "{text}");
    assert!(text.contains("2048 bytes"), "{text}");
    assert!(text.contains("no-op"), "{text}");
    assert!(text.contains("91234567"), "{text}");
}

#[tokio::test]
async fn active_local_work_never_borrows_a_previous_jobs_progress() {
    let stub = Stub::serve(vec![
        Reply::Answer {
            status: "200 OK",
            headers: Vec::new(),
            body: EMPTY_CATALOG_PAGE.into(),
        },
        Reply::ok(r#"{"filesToEmbed":73,"filesEmbedded":41,"filesFailed":0,"completedAt":"old-completion","error":"old worker failure"}"#),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let mut status = checkout(
        None,
        Some(SyncProgress::Uploading {
            uploaded_files: 2,
            total_files: 5,
        }),
    );
    status.root = "parent/project".into();
    status.last_job_id = Some("previous-job".into());
    let text = sync_status(
        &client,
        Some(&status),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    let preview = text.lines().next().unwrap();
    assert!(preview.starts_with(&markdown_code("project")), "{text}");
    assert!(preview.contains("2/5"), "{text}");
    assert!(!preview.contains("embedded 41"), "{text}");
    assert!(!preview.contains("planned embed 73"), "{text}");
    assert!(text.contains("embedded 41"), "{text}");
    assert!(text.contains("planned embed 73"), "{text}");
    assert!(text.contains("old worker failure"), "{text}");
    assert!(text.contains("old-completion"), "{text}");
    assert!(text.contains("parent/project"), "{text}");
    assert!(text.contains(&markdown_code("A")), "{text}");
}

#[tokio::test]
async fn a_failed_server_job_exposes_its_diagnostic_before_completion_evidence() {
    let reason = format!("{}\r\n```\r\nraw server detail\r\n", "failure".repeat(40));
    let job = serde_json::json!({
        "filesToEmbed": 12,
        "filesEmbedded": 8,
        "filesFailed": 4,
        "startedAt": "started-evidence",
        "completedAt": "completed-evidence",
        "error": reason,
    });
    let stub = Stub::serve(vec![
        Reply::Answer {
            status: "200 OK",
            headers: Vec::new(),
            body: EMPTY_CATALOG_PAGE.into(),
        },
        Reply::ok(&job.to_string()),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let mut status = checkout(Some(FirstIndexPhase::Ready), None);
    status.last_job_id = Some("failed-job".into());
    let text = sync_status(
        &client,
        Some(&status),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    let preview = text.split_once("\n\n").unwrap().0;
    assert!(preview.contains("failed"), "{text}");
    assert!(preview.contains(&"failure".repeat(20)), "{text}");
    assert!(!preview.contains("completed-evidence"), "{text}");
    assert!(text.contains("started-evidence"), "{text}");
    assert!(text.contains("completed-evidence"), "{text}");
    assert_eq!(fenced_body(first_fenced_block(&text)), reason);
}

#[test]
fn fenced_structures_retain_whitespace_and_embedded_delimiters() {
    for body in [
        "src/\n  child/\n    ```\n  sibling.rs\n",
        "\r\n  fn example() {\r\n    `````\r\n    **literal**\t\r\n  }\r\n",
        "",
        "last line without a newline",
    ] {
        let mut text = String::new();
        render_fenced(&mut text, body, None);
        let rendered_body = fenced_body(&text);
        assert!(rendered_body.starts_with(body), "{text}");
        assert_eq!(
            rendered_body.len() - body.len(),
            usize::from(!body.ends_with('\n')),
            "only the closing-fence separator may be added"
        );
    }
}

#[test]
fn table_values_cannot_add_columns_or_rows() {
    for value in ["left|right", r"left\|right", "|", "``|\n# hidden", r"\\|`"] {
        let cell = markdown_cell(value);
        assert!(!cell.contains(['\n', '\r']));
        for (index, character) in cell.char_indices() {
            if character == '|' {
                let preceding_slashes = cell[..index]
                    .chars()
                    .rev()
                    .take_while(|character| *character == '\\')
                    .count();
                assert_eq!(preceding_slashes, 1, "{cell}");
            }
        }
        assert_eq!(cell.matches('|').count(), value.matches('|').count());
    }
}

#[test]
fn outline_bodies_retain_exact_source_and_explicit_nesting() {
    let source = "\r\n    fn nested() {\r\n      ```\r\n      **literal**\r\n    }\r\n";
    let outline: crate::client::api::FileOutline = serde_json::from_value(serde_json::json!({
        "path": "src/outline.rs",
        "entries": [{
            "chunkId": "nested",
            "lineStart": 4,
            "lineEnd": 8,
            "kind": "function",
            "qualifiedSymbol": "Outer::nested|name",
            "depth": 2,
            "body": source
        }]
    }))
    .unwrap();

    let text = render_outline(&outline, None);
    let (summary, body) = text.split_once("\n### ").unwrap();
    assert!(summary.contains("Outer::nested"));
    assert!(summary.contains(&markdown_cell("Outer::nested|name")));
    assert!(summary.lines().any(|line| line.starts_with("| 2 | 4-8 |")));
    let (_, block) = body.split_once("\n\n").unwrap();
    assert_eq!(fenced_body(block.strip_suffix('\n').unwrap()), source);
}

#[test]
fn project_fields_remain_attached_to_their_project() {
    let graph: crate::client::api::ProjectGraph = serde_json::from_value(serde_json::json!({
        "groups": [{
            "kind": "workspace",
            "name": "root",
            "root": ".",
            "children": ["one", "two"]
        }],
        "projects": [
            {"kind": "rust", "name": "one|crate", "root": "src/one", "configFile": "src/one/Cargo.toml", "fileCount": 7},
            {"kind": "rust", "name": "two", "root": "src/two", "configFile": null, "fileCount": 3}
        ]
    }))
    .unwrap();
    let text = render_projects(&graph);
    for project in &graph.projects {
        let row = text
            .lines()
            .find(|line| line.contains(&markdown_cell(&project.name)))
            .unwrap();
        assert!(row.contains(&markdown_cell(&project.root)));
        assert!(row.contains(&project.file_count.to_string()));
        if let Some(config) = &project.config_file {
            assert!(row.contains(&markdown_cell(config)));
        }
    }
}

#[tokio::test]
async fn file_tree_retains_sibling_and_parent_relationships() {
    let stub = Stub::serve(vec![Reply::ok(
        r#"[{"name":"src","path":"src","isDirectory":true,"children":[{"name":"nested","path":"src/nested","isDirectory":true,"children":[{"name":"```","path":"src/nested/```","isDirectory":false}]},{"name":"sibling.rs","path":"src/sibling.rs","isDirectory":false}]},{"name":"root.rs","path":"root.rs","isDirectory":false}]"#,
    )])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".to_string());
    let text = super::file_tree(&client).await.unwrap();
    assert_eq!(
        fenced_body(&text),
        "src/\n  nested/\n    ```\n  sibling.rs\nroot.rs\n"
    );
}

#[tokio::test]
async fn source_windows_keep_original_bytes_and_truncation_identity() {
    let source = "\r\n    **literal**\t\r\n`````\r\n# not a heading\r\n";
    let stub = Stub::serve(vec![Reply::ok(
        &serde_json::json!({
            "path": "remote/source.md",
            "contentHash": "pinned-hash",
            "content": source,
            "encoding": "utf-8",
            "isBinary": false,
            "size": 500,
            "byteStart": 20,
            "byteEnd": 80,
            "truncated": true
        })
        .to_string(),
    )])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".to_string());
    let output = crate::query::read_source(
        &client,
        "remote/source.md",
        Some("pinned-hash"),
        Some((20, 80)),
        None,
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap();
    assert!(
        output.display.is_none(),
        "plain queries do not construct inspector metadata"
    );
    let text = output.text;
    assert!(text.contains("pinned-hash"), "{text}");
    assert!(text.contains("20-80 of 500"), "{text}");
    assert!(text.contains("truncated"), "{text}");
    assert_eq!(fenced_body(first_fenced_block(&text)), source);
}

#[tokio::test]
async fn sync_errors_cannot_hide_staleness_or_retry_information() {
    let (_stub, client) = catalog_stub().await;
    let mut status = checkout(None, None);
    let reason = "upload failed\n`````\n# literal diagnostic";
    status.last_error = Some(reason.to_string());
    status.retry_after_secs = Some(10);

    let text = sync_status(
        &client,
        Some(&status),
        crate::query::QueryPresentation::Plain,
    )
    .await
    .unwrap()
    .text;
    assert_eq!(
        fenced_body(first_fenced_block(&text))
            .strip_suffix('\n')
            .expect("a diagnostic without a terminal newline needs a closing-fence separator"),
        reason
    );
    assert!(text.contains("may be stale"), "{text}");
    assert!(text.contains("next local sync attempt in 10 s"), "{text}");
}
