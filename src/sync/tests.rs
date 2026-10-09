//! The text and the wire shape of a sync milestone.

use std::path::PathBuf;

use super::SyncProgress;

/// `semctl index`, `sync_status`, and `semctl daemon status` all show this
/// text, so one test pins every milestone.
#[test]
fn each_milestone_has_one_plain_line() {
    for (progress, line) in [
        (SyncProgress::Preparing, "preparing index"),
        (
            SyncProgress::Scanning {
                root: PathBuf::from("repo"),
            },
            "scanning files in repo",
        ),
        (
            SyncProgress::Planning {
                files: 80,
                cached_files: 73,
            },
            "scanned 80 files (73 filter decisions reused) — checking for changes",
        ),
        (
            SyncProgress::Uploading {
                uploaded_files: 3,
                total_files: 8,
            },
            "uploading 3/8 files",
        ),
        (SyncProgress::Finalizing, "finalizing upload"),
    ] {
        assert_eq!(progress.to_string(), line);
    }
}

/// An older daemon and a newer client must agree on these tags.
#[test]
fn milestones_use_snake_case_tags_on_the_wire() {
    for (progress, wire) in [
        (SyncProgress::Preparing, serde_json::json!("preparing")),
        (
            SyncProgress::Scanning {
                root: PathBuf::from("/work/checkout"),
            },
            serde_json::json!({"scanning": {"root": "/work/checkout"}}),
        ),
        (
            SyncProgress::Planning {
                files: 80,
                cached_files: 73,
            },
            serde_json::json!({"planning": {"files": 80, "cached_files": 73}}),
        ),
        (
            SyncProgress::Uploading {
                uploaded_files: 3,
                total_files: 8,
            },
            serde_json::json!({"uploading": {"uploaded_files": 3, "total_files": 8}}),
        ),
        (SyncProgress::Finalizing, serde_json::json!("finalizing")),
    ] {
        assert_eq!(
            serde_json::to_value(&progress).expect("serialize the milestone"),
            wire
        );
        assert_eq!(
            serde_json::from_value::<SyncProgress>(wire).expect("decode the milestone"),
            progress
        );
    }
}
