//! Structural results retain source bytes and relationships in Markdown hosts.

use super::{current_sync_line, first_index_line, sync_status};
use crate::client::Client;
use crate::client::stub::{Reply, Stub};
use crate::engine::CoordinatorStatus;
use crate::engine::coordinator::WatcherState;
use crate::mcp::readiness::FirstIndexPhase;
use crate::query::render::{markdown_cell, render_fenced, render_outline, render_projects};
use crate::sync::SyncProgress;

fn fenced_body(text: &str) -> &str {
    let (fence, rest) = text.split_once('\n').expect("a fenced block has an opener");
    assert!(fence.len() >= 3 && fence.bytes().all(|byte| byte == b'`'));
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
fn each_phase_has_one_plain_line() {
    for (phase, line) in [
        (FirstIndexPhase::Registering, "first index: registering"),
        (FirstIndexPhase::Syncing, "first index: syncing"),
        (
            FirstIndexPhase::Embedding {
                job_id: Some("job-7".to_string()),
            },
            "first index: embedding (job job-7)",
        ),
        (
            FirstIndexPhase::Embedding { job_id: None },
            "first index: embedding",
        ),
        (FirstIndexPhase::Ready, "first index: ready"),
    ] {
        assert_eq!(first_index_line(&phase), line);
    }
}

#[test]
fn a_failed_first_index_names_the_reason_and_the_retry() {
    let phase = FirstIndexPhase::Failed {
        reason: "embedding job 7 failed".to_string(),
    };

    assert_eq!(
        first_index_line(&phase),
        "first index: failed — embedding job 7 failed; call index_codebase to retry"
    );
}

/// An agent reads the top of the answer first, and nothing waits for a first
/// index. The phase therefore comes straight after the codebase line.
#[tokio::test]
async fn sync_status_reports_every_phase_after_the_codebase_line() {
    let (_stub, client) = catalog_stub().await;
    for phase in [
        FirstIndexPhase::Registering,
        FirstIndexPhase::Syncing,
        FirstIndexPhase::Embedding {
            job_id: Some("job-7".to_string()),
        },
        FirstIndexPhase::Ready,
        FirstIndexPhase::Failed {
            reason: "embedding job 7 failed".to_string(),
        },
    ] {
        let text = sync_status(&client, Some(&checkout(Some(phase.clone()), None)))
            .await
            .expect("sync_status works while a first index is pending");
        let lines: Vec<&str> = fenced_body(&text).lines().collect();

        assert_eq!(lines[0], "codebase A", "{text}");
        assert_eq!(lines[1], first_index_line(&phase), "{text}");
        assert_eq!(lines[2], "local checkout watch: active", "{text}");
    }
}

#[tokio::test]
async fn sync_status_has_no_first_index_line_for_a_checkout_without_one() {
    let (_stub, client) = catalog_stub().await;

    let text = sync_status(&client, Some(&checkout(None, None)))
        .await
        .expect("sync_status answers");

    assert!(!text.contains("first index"), "{text}");
    assert_eq!(
        fenced_body(&text).lines().nth(1),
        Some("local checkout watch: active")
    );
}

#[test]
fn the_current_sync_line_names_the_milestone_in_the_shared_words() {
    assert_eq!(
        current_sync_line(&SyncProgress::Uploading {
            uploaded_files: 1200,
            total_files: 3607,
        }),
        "current sync: uploading 1200/3607 files"
    );
    assert_eq!(
        current_sync_line(&SyncProgress::Planning {
            files: 3607,
            cached_files: 3500,
        }),
        "current sync: scanned 3607 files (3500 filter decisions reused) — checking for changes"
    );
}

/// The phase says that the first index syncs. The next line says how far the
/// sync has come, so the two lines stay together.
#[tokio::test]
async fn sync_status_puts_the_current_sync_line_after_the_first_index_line() {
    let (_stub, client) = catalog_stub().await;
    let progress = SyncProgress::Uploading {
        uploaded_files: 1200,
        total_files: 3607,
    };

    let text = sync_status(
        &client,
        Some(&checkout(Some(FirstIndexPhase::Syncing), Some(progress))),
    )
    .await
    .expect("sync_status answers while a first index syncs");
    let lines: Vec<&str> = fenced_body(&text).lines().collect();

    assert_eq!(lines[0], "codebase A", "{text}");
    assert_eq!(lines[1], "first index: syncing", "{text}");
    assert_eq!(
        lines[2], "current sync: uploading 1200/3607 files",
        "{text}"
    );
    assert_eq!(lines[3], "local checkout watch: active", "{text}");
}

/// A re-sync of an indexed checkout has no first-index gate. Its progress line
/// then follows the codebase line.
#[tokio::test]
async fn sync_status_reports_a_running_resync_without_a_first_index() {
    let (_stub, client) = catalog_stub().await;

    let text = sync_status(
        &client,
        Some(&checkout(None, Some(SyncProgress::Finalizing))),
    )
    .await
    .expect("sync_status answers during a re-sync");
    let lines: Vec<&str> = fenced_body(&text).lines().collect();

    assert_eq!(lines[0], "codebase A", "{text}");
    assert_eq!(lines[1], "current sync: finalizing upload", "{text}");
    assert_eq!(lines[2], "local checkout watch: active", "{text}");
}

#[tokio::test]
async fn sync_status_has_no_current_sync_line_when_no_sync_runs() {
    let (_stub, client) = catalog_stub().await;

    let text = sync_status(&client, Some(&checkout(Some(FirstIndexPhase::Ready), None)))
        .await
        .expect("sync_status answers");

    assert!(!text.contains("current sync"), "{text}");
}

#[tokio::test]
async fn a_local_sync_failure_reports_stale_results_and_the_retry_delay() {
    let (_stub, client) = catalog_stub().await;
    let mut status = checkout(None, None);
    status.last_error = Some("sync plan: gateway unavailable".to_string());
    status.retry_after_secs = Some(10);

    let text = sync_status(&client, Some(&status)).await.unwrap();

    assert!(text.contains("last local sync failed: sync plan: gateway unavailable"));
    assert!(text.contains("indexed results may be stale"));
    assert!(text.contains("next local sync attempt in 10 s"));
    assert!(text.contains("no server job queued; the local sync failed"));
    assert!(!text.contains("may still be scanning"));
}

#[tokio::test]
async fn a_previous_completed_job_does_not_hide_a_local_sync_failure() {
    let stub = Stub::serve(vec![
        Reply::Answer {
            status: "200 OK",
            headers: Vec::new(),
            body: EMPTY_CATALOG_PAGE.to_string(),
        },
        Reply::ok(r#"{"id":"old-job","completedAt":"2026-10-01T00:00:00Z","filesEmbedded":0,"filesDeleted":0,"filesFailed":0,"error":null}"#),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".to_string());
    let mut status = checkout(None, None);
    status.last_job_id = Some("old-job".to_string());
    status.last_error = Some("upload failed".to_string());

    let text = sync_status(&client, Some(&status)).await.unwrap();

    assert!(text.contains("last local sync failed: upload failed"));
    assert!(text.contains("indexed results may be stale"));
    assert!(text.contains("old-job"));
    assert!(!text.contains("status could not be read"), "{text}");
}

#[tokio::test]
async fn a_missing_watcher_is_not_reported_as_active() {
    let (_stub, client) = catalog_stub().await;
    let mut status = checkout(None, None);
    status.watcher = WatcherState::Unavailable("watch limit reached".to_string());

    let text = sync_status(&client, Some(&status)).await.unwrap();

    assert!(text.contains("local checkout watch: not active (watch limit reached)"));
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
    let text = crate::query::read_source(
        &client,
        "remote/source.md",
        Some("pinned-hash"),
        Some((20, 80)),
        None,
    )
    .await
    .unwrap();
    let (metadata, body) = text.split_once("\n\n").unwrap();
    let metadata_block = format!("{metadata}\n");
    let metadata = fenced_body(&metadata_block);
    assert!(metadata.contains("pinned-hash"));
    assert!(metadata.contains("20-80 of 500"));
    assert!(metadata.contains("truncated"));
    assert_eq!(fenced_body(body), source);
}

#[tokio::test]
async fn sync_errors_cannot_hide_staleness_or_retry_information() {
    let (_stub, client) = catalog_stub().await;
    let mut status = checkout(None, None);
    let reason = "upload failed\n`````\n# literal diagnostic";
    status.last_error = Some(reason.to_string());
    status.retry_after_secs = Some(10);

    let text = sync_status(&client, Some(&status)).await.unwrap();
    let body = fenced_body(&text);
    assert!(body.contains(reason));
    assert!(body.contains("indexed results may be stale"));
    assert!(body.contains("next local sync attempt in 10 s"));
}
