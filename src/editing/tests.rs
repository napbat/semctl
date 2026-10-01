use std::fs;

use super::{
    EditHistory, HistoryFile, PLAN_SCHEMA_VERSION, apply_byte_edits, ensure_no_retained_recovery,
    hash, history_matches, lock_checkout, outcome_from_history, resolve_target,
};
use crate::client::api::ByteEdit;

#[test]
fn byte_edits_apply_in_reverse_without_offset_drift() {
    let edits = vec![
        ByteEdit {
            start: 0,
            end: 1,
            replacement: "AA".into(),
        },
        ByteEdit {
            start: 4,
            end: 6,
            replacement: "Z".into(),
        },
    ];
    assert_eq!(apply_byte_edits(b"abcdef", &edits, "x").unwrap(), b"AAbcdZ");
}

#[test]
fn overlapping_edits_are_rejected() {
    let edits = vec![
        ByteEdit {
            start: 1,
            end: 4,
            replacement: String::new(),
        },
        ByteEdit {
            start: 3,
            end: 5,
            replacement: String::new(),
        },
    ];
    assert!(apply_byte_edits(b"abcdef", &edits, "x").is_err());
}

#[test]
fn out_of_checkout_paths_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    assert!(resolve_target(temp.path(), "../escape.rs").is_err());
    assert!(resolve_target(temp.path(), "/escape.rs").is_err());
}

#[test]
fn checkout_transactions_share_one_lock_across_plans() {
    let directory = tempfile::tempdir().unwrap();
    let first = lock_checkout(directory.path(), "source-a").unwrap();
    let path = directory
        .path()
        .join(format!("checkout-{}.lock", hash(b"source-a")));
    let competing = crate::config::open_lock(&path).unwrap();
    assert!(matches!(
        competing.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    let independent = lock_checkout(directory.path(), "source-b").unwrap();
    // Other tests can fork a formatter while this descriptor is open. Unlock
    // explicitly so a fork's brief inherited copy cannot delay the assertion.
    first.unlock().unwrap();
    drop(first);
    competing.try_lock().unwrap();
    drop(independent);
}

#[test]
fn retained_hashes_recognize_duplicate_apply_and_undo_delivery() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let target = root.join("source.rs");
    fs::write(&target, b"after").unwrap();
    let mut history = EditHistory {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_id: "a".repeat(64),
        operation: "rename_symbol".into(),
        codebase_id: "cb".into(),
        source_identity: "checkout".into(),
        files: vec![HistoryFile {
            path: "source.rs".into(),
            preimage_hash: hash(b"before"),
            preimage_base64: String::new(),
            postimage_hash: hash(b"after"),
        }],
        undone: false,
    };

    assert!(history_matches(&root, &history, false).unwrap());
    let applied = outcome_from_history(&history, true, false, true);
    assert!(applied.already_applied);

    fs::write(&target, b"before").unwrap();
    history.undone = true;
    assert!(history_matches(&root, &history, true).unwrap());
    let undone = outcome_from_history(&history, false, true, true);
    assert!(undone.already_undone);
}

#[test]
fn retained_recovery_blocks_apply_and_undo_replay_even_when_hashes_match() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    let target = root.join("source.rs");
    fs::write(&target, b"after").unwrap();
    let mut history = EditHistory {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_id: "a".repeat(64),
        operation: "rename_symbol".into(),
        codebase_id: "cb".into(),
        source_identity: "checkout".into(),
        files: vec![HistoryFile {
            path: "source.rs".into(),
            preimage_hash: hash(b"before"),
            preimage_base64: String::new(),
            postimage_hash: hash(b"after"),
        }],
        undone: false,
    };
    let (_, backup) = super::sidecars(&target, &history.plan_id, 0);
    let recovery = backup.with_extension("edit");
    fs::create_dir(&recovery).unwrap();
    fs::write(recovery.join("preimage"), b"concurrent save").unwrap();
    for undone in [false, true] {
        history.undone = undone;
        fs::write(
            &target,
            if undone {
                b"before".as_slice()
            } else {
                b"after"
            },
        )
        .unwrap();
        assert!(history_matches(&root, &history, undone).unwrap());
        let error = ensure_no_retained_recovery(&root, &history).unwrap_err();
        assert!(error.to_string().contains(&recovery.display().to_string()));
    }
}

#[cfg(unix)]
#[test]
fn unix_backslashes_in_edit_paths_remain_literal() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    fs::write(root.join("source\\file.rs"), b"literal filename").unwrap();
    let (relative, target) = resolve_target(&root, "source\\file.rs").unwrap();
    assert_eq!(relative, "source\\file.rs");
    assert_eq!(target, root.join("source\\file.rs"));
}
