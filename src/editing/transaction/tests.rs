use std::fs;
use std::io::{Seek as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use base64::Engine as _;

use super::{Boundary, POSTIMAGE, PREIMAGE, ROLLBACK, Transaction, recovery_required};
use crate::editing::{
    BASE64, EditHistory, HistoryFile, PLAN_SCHEMA_VERSION, PreparedFile, hash, paths, prepare_undo,
    sidecars,
};

struct Fixture {
    files: Vec<PreparedFile>,
    // Drop capabilities before TempDir removes the tree on Windows.
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new(undo: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        let checkout = paths::Checkout::open(&root).unwrap();
        let mut files = Vec::new();
        for index in 0..2 {
            let path = format!("src/file-{index}.rs");
            let target = root.join(&path);
            fs::write(&target, if undo { "after" } else { "before" }).unwrap();
            let (temporary, backup) = sidecars(&target, &"a".repeat(64), index);
            files.push(PreparedFile {
                path,
                location: paths::Target::bind(&checkout, &target).unwrap(),
                target,
                preimage: b"before".to_vec(),
                postimage: b"after".to_vec(),
                postimage_hash: hash(b"after"),
                temporary,
                backup,
            });
        }
        if undo {
            // Exercise the production undo preparation, including its retained
            // hashes. The transaction then uses the same engine as apply.
            let history = EditHistory {
                schema_version: PLAN_SCHEMA_VERSION,
                plan_id: "a".repeat(64),
                operation: "rename_symbol".into(),
                codebase_id: "cb".into(),
                source_identity: "source".into(),
                files: files
                    .iter()
                    .map(|file| HistoryFile {
                        path: file.path.clone(),
                        preimage_hash: hash(b"before"),
                        preimage_base64: BASE64.encode(b"before"),
                        postimage_hash: hash(b"after"),
                    })
                    .collect(),
                undone: false,
            };
            files = prepare_undo(&checkout, &history).unwrap();
        }
        Self { files, directory }
    }

    fn target(&self, index: usize) -> &Path {
        &self.files[index].target
    }

    fn retained(&self, index: usize, name: &str) -> PathBuf {
        self.files[index].recovery_path().join(name)
    }

    fn assert_originals(&self) {
        for file in &self.files {
            assert_eq!(fs::read(&file.target).unwrap(), file.preimage);
        }
    }

    fn assert_clean(&self) {
        assert!(self.directory.path().is_dir());
        for file in &self.files {
            assert!(!file.recovery_path().exists());
        }
    }
}

fn save(target: &Path, bytes: &[u8]) {
    let temporary = target.with_extension("editor-save");
    fs::write(&temporary, bytes).unwrap();
    fs::rename(temporary, target).unwrap();
}

fn fail_second(boundary: Boundary, index: usize) -> Result<()> {
    if boundary == Boundary::BeforeDisplace && index == 1 {
        Err(anyhow!("injected second-file failure"))
    } else {
        Ok(())
    }
}

#[test]
fn successful_apply_and_undo_remove_verified_recovery_files() {
    for undo in [false, true] {
        let fixture = Fixture::new(undo);
        Transaction::commit(&fixture.files)
            .unwrap()
            .finish()
            .unwrap();
        for file in &fixture.files {
            assert_eq!(fs::read(&file.target).unwrap(), file.postimage);
        }
        fixture.assert_clean();
    }
}

#[test]
fn partial_apply_and_undo_failures_restore_every_installed_file() {
    for undo in [false, true] {
        let fixture = Fixture::new(undo);
        let error = Transaction::commit_with(&fixture.files, &mut fail_second)
            .err()
            .unwrap();
        assert!(!recovery_required(&error));
        fixture.assert_originals();
        fixture.assert_clean();
    }
}

#[test]
fn failed_history_publication_rolls_back_a_committed_undo() {
    let fixture = Fixture::new(true);
    let transaction = Transaction::commit(&fixture.files).unwrap();
    let error = transaction.rollback_error(anyhow!("injected history publication failure"));
    assert!(!recovery_required(&error));
    fixture.assert_originals();
    fixture.assert_clean();
}

#[test]
fn saves_after_staging_are_restored_and_retained_for_apply_and_undo() {
    for undo in [false, true] {
        for replace_inode in [false, true] {
            let fixture = Fixture::new(undo);
            let error = Transaction::commit_with(&fixture.files, &mut |boundary, _| {
                if boundary == Boundary::Staged {
                    if replace_inode {
                        save(fixture.target(0), b"editor save");
                    } else {
                        fs::write(fixture.target(0), b"editor save")?;
                    }
                }
                Ok(())
            })
            .err()
            .unwrap();
            assert!(recovery_required(&error));
            assert_eq!(fs::read(fixture.target(0)).unwrap(), b"editor save");
            assert_eq!(
                fs::read(fixture.retained(0, PREIMAGE)).unwrap(),
                b"editor save"
            );
            assert_eq!(
                fs::read(fixture.target(1)).unwrap(),
                fixture.files[1].preimage
            );
            assert!(
                format!("{error:#}")
                    .contains(&fixture.files[0].recovery_path().display().to_string())
            );
        }
    }
}

#[test]
fn saves_created_after_displacement_are_never_replaced() {
    for undo in [false, true] {
        let fixture = Fixture::new(undo);
        let error = Transaction::commit_with(&fixture.files, &mut |boundary, index| {
            if boundary == Boundary::AfterDisplace && index == 0 {
                fs::write(fixture.target(0), b"new editor target")?;
            }
            Ok(())
        })
        .err()
        .unwrap();
        assert!(recovery_required(&error));
        assert_eq!(fs::read(fixture.target(0)).unwrap(), b"new editor target");
        assert_eq!(
            fs::read(fixture.retained(0, PREIMAGE)).unwrap(),
            fixture.files[0].preimage
        );
        assert_eq!(
            fs::read(fixture.retained(0, POSTIMAGE)).unwrap(),
            fixture.files[0].postimage
        );
    }
}

#[test]
fn writes_through_displaced_descriptors_are_restored_and_retained() {
    for undo in [false, true] {
        for boundary_to_write in [
            Boundary::AfterDisplace,
            Boundary::AfterInstall,
            Boundary::BeforeVerify,
        ] {
            let fixture = Fixture::new(undo);
            let mut editor = fs::OpenOptions::new()
                .write(true)
                .open(fixture.target(0))
                .unwrap();
            let error = Transaction::commit_with(&fixture.files, &mut |boundary, index| {
                if boundary == boundary_to_write && index == 0 {
                    editor.set_len(0)?;
                    editor.rewind()?;
                    editor.write_all(b"late descriptor write")?;
                    editor.sync_all()?;
                }
                Ok(())
            })
            .err()
            .unwrap();
            assert!(recovery_required(&error));
            assert_eq!(
                fs::read(fixture.target(0)).unwrap(),
                b"late descriptor write"
            );
            assert_eq!(
                fs::read(fixture.retained(0, PREIMAGE)).unwrap(),
                b"late descriptor write"
            );
        }
    }
}

#[test]
fn saves_after_installation_survive_failed_commit_and_rollback() {
    for undo in [false, true] {
        for replace_inode in [false, true] {
            let fixture = Fixture::new(undo);
            let error = Transaction::commit_with(&fixture.files, &mut |boundary, index| {
                if boundary == Boundary::AfterInstall && index == 0 {
                    if replace_inode {
                        save(fixture.target(0), b"post-install save");
                    } else {
                        fs::write(fixture.target(0), b"post-install save")?;
                    }
                }
                Ok(())
            })
            .err()
            .unwrap();
            assert!(recovery_required(&error));
            assert_eq!(fs::read(fixture.target(0)).unwrap(), b"post-install save");
            assert_eq!(
                fs::read(fixture.retained(0, PREIMAGE)).unwrap(),
                fixture.files[0].preimage
            );
        }
    }
}

#[test]
fn rollback_verifies_the_version_it_actually_displaced() {
    for undo in [false, true] {
        let fixture = Fixture::new(undo);
        let error = Transaction::commit_with(&fixture.files, &mut |boundary, index| {
            fail_second(boundary, index)?;
            if boundary == Boundary::BeforeRollbackDisplace && index == 0 {
                save(fixture.target(0), b"save during rollback");
            }
            Ok(())
        })
        .err()
        .unwrap();
        assert!(recovery_required(&error));
        assert_eq!(
            fs::read(fixture.target(0)).unwrap(),
            b"save during rollback"
        );
        assert_eq!(
            fs::read(fixture.retained(0, ROLLBACK)).unwrap(),
            b"save during rollback"
        );
        assert_eq!(
            fs::read(fixture.retained(0, PREIMAGE)).unwrap(),
            fixture.files[0].preimage
        );
    }
}

#[test]
fn rollback_never_replaces_a_target_created_after_its_displacement() {
    for undo in [false, true] {
        let fixture = Fixture::new(undo);
        let error = Transaction::commit_with(&fixture.files, &mut |boundary, index| {
            fail_second(boundary, index)?;
            if boundary == Boundary::AfterRollbackDisplace && index == 0 {
                fs::write(fixture.target(0), b"newer rollback save")?;
            }
            Ok(())
        })
        .err()
        .unwrap();
        assert!(recovery_required(&error));
        assert_eq!(fs::read(fixture.target(0)).unwrap(), b"newer rollback save");
        assert_eq!(
            fs::read(fixture.retained(0, PREIMAGE)).unwrap(),
            fixture.files[0].preimage
        );
        assert_eq!(
            fs::read(fixture.retained(0, ROLLBACK)).unwrap(),
            fixture.files[0].postimage
        );
    }
}

#[test]
fn failed_history_publication_preserves_a_new_editor_version() {
    let fixture = Fixture::new(true);
    let transaction = Transaction::commit(&fixture.files).unwrap();
    save(fixture.target(0), b"save before history rollback");
    let error = transaction.rollback_error(anyhow!("injected history publication failure"));
    assert!(recovery_required(&error));
    assert_eq!(
        fs::read(fixture.target(0)).unwrap(),
        b"save before history rollback"
    );
    assert_eq!(
        fs::read(fixture.retained(0, PREIMAGE)).unwrap(),
        fixture.files[0].preimage
    );
    assert_eq!(
        fs::read(fixture.target(1)).unwrap(),
        fixture.files[1].preimage
    );
}

#[test]
fn final_cleanup_retains_preimages_modified_after_commit_verification() {
    for undo in [false, true] {
        let fixture = Fixture::new(undo);
        let transaction = Transaction::commit(&fixture.files).unwrap();
        let error = transaction
            .finish_with(&mut |boundary, _| {
                assert_eq!(boundary, Boundary::BeforeCleanup);
                fs::write(fixture.retained(0, PREIMAGE), b"unexpected retained bytes")?;
                Ok(())
            })
            .unwrap_err();
        assert!(recovery_required(&error));
        assert_eq!(
            fs::read(fixture.retained(0, PREIMAGE)).unwrap(),
            b"unexpected retained bytes"
        );
        assert_eq!(
            fs::read(fixture.retained(1, PREIMAGE)).unwrap(),
            fixture.files[1].preimage
        );
    }
}

#[test]
fn staging_preserves_legacy_recovery_files() {
    let fixture = Fixture::new(false);
    for legacy in [&fixture.files[1].temporary, &fixture.files[1].backup] {
        fs::write(legacy, b"retained legacy bytes").unwrap();
        assert!(Transaction::commit(&fixture.files).is_err());
        assert_eq!(fs::read(legacy).unwrap(), b"retained legacy bytes");
        fixture.assert_originals();
        fixture.assert_clean();
        fs::remove_file(legacy).unwrap();
    }
}

#[test]
fn staging_preserves_existing_recovery_directories() {
    let fixture = Fixture::new(false);
    fs::create_dir(fixture.files[1].recovery_path()).unwrap();
    fs::write(fixture.retained(1, PREIMAGE), b"old recovery bytes").unwrap();
    assert!(Transaction::commit(&fixture.files).is_err());
    assert_eq!(
        fs::read(fixture.retained(1, PREIMAGE)).unwrap(),
        b"old recovery bytes"
    );
    fixture.assert_originals();
    assert!(!fixture.files[0].recovery_path().exists());
}

#[cfg(unix)]
#[test]
fn recovery_directories_are_private_at_creation() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new(false);
    let transaction = Transaction::commit(&fixture.files).unwrap();
    assert_eq!(
        fs::metadata(fixture.files[0].recovery_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    transaction.finish().unwrap();
}

#[cfg(unix)]
fn replace_parent_with_outside_symlink(fixture: &Fixture, outside: &Path) -> PathBuf {
    let parent = fixture.target(0).parent().unwrap();
    let moved = fixture.directory.path().join("moved-source-directory");
    fs::rename(parent, &moved).unwrap();
    std::os::unix::fs::symlink(outside, parent).unwrap();
    moved
}

#[cfg(unix)]
#[test]
fn ancestor_replacements_cannot_redirect_staging_or_commit() {
    for replace_after_staging in [false, true] {
        let fixture = Fixture::new(false);
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("file-0.rs"), b"before").unwrap();
        fs::write(outside.path().join("file-1.rs"), b"before").unwrap();
        if !replace_after_staging {
            replace_parent_with_outside_symlink(&fixture, outside.path());
        }
        assert!(
            Transaction::commit_with(&fixture.files, &mut |boundary, _| {
                if replace_after_staging && boundary == Boundary::Staged {
                    replace_parent_with_outside_symlink(&fixture, outside.path());
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(
            fs::read(outside.path().join("file-0.rs")).unwrap(),
            b"before"
        );
        assert_eq!(
            fs::read(outside.path().join("file-1.rs")).unwrap(),
            b"before"
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 2);
        assert_eq!(
            fs::read(
                fixture
                    .directory
                    .path()
                    .join("moved-source-directory/file-0.rs")
            )
            .unwrap(),
            b"before"
        );
    }
}

#[cfg(unix)]
#[test]
fn ancestor_replacements_after_displacement_preserve_the_original_directory_backup() {
    let fixture = Fixture::new(false);
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("file-0.rs"), b"before").unwrap();
    let mut moved = PathBuf::new();
    let error = Transaction::commit_with(&fixture.files, &mut |boundary, index| {
        if boundary == Boundary::AfterDisplace && index == 0 {
            moved = replace_parent_with_outside_symlink(&fixture, outside.path());
        }
        Ok(())
    })
    .err()
    .unwrap();
    assert!(recovery_required(&error));
    assert_eq!(
        fs::read(outside.path().join("file-0.rs")).unwrap(),
        b"before"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
    let retained = moved
        .join(fixture.files[0].recovery_path().file_name().unwrap())
        .join(PREIMAGE);
    assert_eq!(fs::read(retained).unwrap(), b"before");
}

#[cfg(unix)]
#[test]
fn ancestor_replacements_during_rollback_do_not_modify_outside_files() {
    let fixture = Fixture::new(true);
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("file-0.rs"), b"outside bytes").unwrap();
    let mut moved = PathBuf::new();
    let error = Transaction::commit_with(&fixture.files, &mut |boundary, index| {
        fail_second(boundary, index)?;
        if boundary == Boundary::BeforeRollbackDisplace && index == 0 {
            moved = replace_parent_with_outside_symlink(&fixture, outside.path());
        }
        Ok(())
    })
    .err()
    .unwrap();
    assert!(recovery_required(&error));
    assert_eq!(
        fs::read(outside.path().join("file-0.rs")).unwrap(),
        b"outside bytes"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
    let retained = moved
        .join(fixture.files[0].recovery_path().file_name().unwrap())
        .join(PREIMAGE);
    assert_eq!(fs::read(retained).unwrap(), b"after");
}

#[cfg(unix)]
#[test]
fn an_absolute_symlink_within_the_checkout_remains_supported() {
    let mut fixture = Fixture::new(false);
    let link = fixture.directory.path().join("source-alias.rs");
    std::os::unix::fs::symlink(fixture.target(0), &link).unwrap();
    fixture.files[0].path = "source-alias.rs".into();
    Transaction::commit(&fixture.files)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(fs::read(link).unwrap(), b"after");
    fixture.assert_clean();
}

#[cfg(unix)]
#[test]
fn an_alias_retargeted_after_staging_cannot_edit_its_original_file() {
    let mut fixture = Fixture::new(false);
    let link = fixture.directory.path().join("source-alias.rs");
    std::os::unix::fs::symlink(fixture.target(0), &link).unwrap();
    fixture.files[0].path = "source-alias.rs".into();
    assert!(
        Transaction::commit_with(&fixture.files, &mut |boundary, _| {
            if boundary == Boundary::Staged {
                fs::remove_file(&link)?;
                std::os::unix::fs::symlink(fixture.target(1), &link)?;
            }
            Ok(())
        })
        .is_err()
    );
    fixture.assert_originals();
    fixture.assert_clean();
}

#[cfg(unix)]
#[test]
fn replacing_the_checkout_root_cannot_redirect_a_prepared_transaction() {
    let fixture = Fixture::new(false);
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(outside.path().join("src")).unwrap();
    fs::write(outside.path().join("src/file-0.rs"), b"before").unwrap();
    let moved = fixture.directory.path().with_extension("moved-checkout");
    fs::rename(fixture.directory.path(), &moved).unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.directory.path()).unwrap();
    assert!(Transaction::commit(&fixture.files).is_err());
    assert_eq!(
        fs::read(outside.path().join("src/file-0.rs")).unwrap(),
        b"before"
    );
    assert_eq!(fs::read_dir(outside.path().join("src")).unwrap().count(), 1);
    assert_eq!(fs::read(moved.join("src/file-0.rs")).unwrap(), b"before");
    fs::remove_file(fixture.directory.path()).unwrap();
    fs::rename(moved, fixture.directory.path()).unwrap();
}

#[cfg(unix)]
#[test]
fn apply_and_undo_preserve_permissions_tightened_after_staging() {
    use std::os::unix::fs::PermissionsExt as _;
    for undo in [false, true] {
        let fixture = Fixture::new(undo);
        fs::set_permissions(fixture.target(0), fs::Permissions::from_mode(0o644)).unwrap();
        let transaction = Transaction::commit_with(&fixture.files, &mut |boundary, index| {
            if boundary == Boundary::BeforeDisplace && index == 0 {
                fs::set_permissions(fixture.target(0), fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        })
        .unwrap();
        transaction.finish().unwrap();
        assert_eq!(
            fs::metadata(fixture.target(0))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn permission_changes_on_displaced_files_abort_and_restore_the_tighter_version() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new(false);
    fs::set_permissions(fixture.target(0), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        Transaction::commit_with(&fixture.files, &mut |boundary, index| {
            if boundary == Boundary::AfterInstall && index == 0 {
                fs::set_permissions(
                    fixture.retained(0, PREIMAGE),
                    fs::Permissions::from_mode(0o600),
                )?;
            }
            Ok(())
        })
        .is_err()
    );
    fixture.assert_originals();
    assert_eq!(
        fs::metadata(fixture.target(0))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
