//! `sync_status` text for the phases of a first index.
//!
//! The tests use a loopback stub that answers every request with an empty
//! catalog page, so no test needs a server or a network.

use super::{first_index_line, sync_status};
use crate::client::Client;
use crate::client::stub::{Reply, Stub};
use crate::mcp::readiness::FirstIndexPhase;

const EMPTY_CATALOG_PAGE: &str = r#"{"success":true,"data":[],"page":0,"pageSize":1000,"total":0}"#;

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
        let text = sync_status(&client, None, true, Some(&phase))
            .await
            .expect("sync_status works while a first index is pending");
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines[0], "codebase A", "{text}");
        assert_eq!(lines[1], first_index_line(&phase), "{text}");
        assert_eq!(lines[2], "local checkout watch: active", "{text}");
    }
}

#[tokio::test]
async fn sync_status_has_no_first_index_line_for_a_checkout_without_one() {
    let (_stub, client) = catalog_stub().await;

    let text = sync_status(&client, None, true, None)
        .await
        .expect("sync_status answers");

    assert!(!text.contains("first index"), "{text}");
    assert_eq!(text.lines().nth(1), Some("local checkout watch: active"));
}
