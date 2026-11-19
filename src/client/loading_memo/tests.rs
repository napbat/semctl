use std::time::Duration;

use tokio::time::Instant;

use super::LoadingKind::{File, Graph};
use super::{CodebaseRequest, LoadingMemo, MAX_MEMO_ENTRIES};

#[test]
fn only_projection_routes_establish_loading_readiness() {
    for (path, kind) in [
        ("/v1/codebases/a/graph/definitions", Some(Graph)),
        ("/v1/codebases/a/edits/rename", Some(Graph)),
        ("/v1/codebases/a/files/content?path=x", Some(File)),
        ("v1/codebases/a?copy=b", None),
        ("/v1/codebases/a/files/tree", None),
        ("/v1/codebases/a/versions", None),
    ] {
        assert_eq!(
            CodebaseRequest::from_path(path),
            Some(CodebaseRequest { id: "a", kind })
        );
    }
    for path in [
        "/v1/codebases?sourceId=a",
        "/v1/codebases//graph/definitions",
        "/v1/search?path=/v1/codebases/a",
    ] {
        assert_eq!(CodebaseRequest::from_path(path), None);
    }
}

#[test]
fn a_loading_entry_expires_at_the_end_of_its_window() {
    let memo = LoadingMemo::default();
    let now = Instant::now();
    let window = Duration::from_secs(15);
    memo.record("a", Graph, now, window);

    assert!(memo.contains("a", Graph, now + window - Duration::from_millis(1)));
    assert!(!memo.contains("a", Graph, now + window));
}

#[test]
fn a_full_memo_evicts_the_first_entry_to_expire() {
    let memo = LoadingMemo::default();
    let now = Instant::now();
    memo.record("old", Graph, now, Duration::from_secs(15));
    for id in 1..MAX_MEMO_ENTRIES {
        memo.record(&id.to_string(), Graph, now, Duration::from_secs(30));
    }
    memo.record("new", Graph, now, Duration::from_secs(15));

    assert!(!memo.contains("old", Graph, now));
    assert!(memo.contains("new", Graph, now));
    assert_eq!(memo.lock().len(), MAX_MEMO_ENTRIES);
}
