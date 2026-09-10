//! Guarded file swaps shared by apply and undo.
//!
//! The checkout lock serializes semctl, but editors do not take that lock. A
//! swap therefore validates the file it actually displaced and publishes only
//! into an absent name. Rollback follows the same rule. Conflicts retain the
//! private recovery directory and report its path.
//!
//! Multiple file replacements are not globally atomic. An uncooperative writer
//! can also retain an open descriptor and write to its displaced inode after our
//! final check. No portable filesystem operation prevents that race. We detect
//! writes observed before final cleanup and retain unexpected bytes. Callers
//! must not promise isolation from arbitrary writes through old descriptors.

use std::ffi::OsStr;
use std::fmt;
use std::io::Write as _;

use anyhow::{Context, Result, anyhow, ensure};

use super::paths::{Recovery, exists};
use super::{PreparedFile, hash};

const POSTIMAGE: &str = "postimage";
const PREIMAGE: &str = "preimage";
const ROLLBACK: &str = "rollback";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Staged,
    Displaced,
    Installed,
    RollbackDisplaced,
    Restored,
}

struct StagedFile {
    recovery: Option<Recovery>,
    state: State,
}

pub(super) struct Transaction<'a> {
    files: &'a [PreparedFile],
    staged: Vec<StagedFile>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Boundary {
    Staged,
    BeforeDisplace,
    AfterDisplace,
    AfterInstall,
    BeforeVerify,
    BeforeRollbackDisplace,
    AfterRollbackDisplace,
    BeforeRestore,
    BeforeCleanup,
}

#[derive(Debug)]
struct RecoveryRequired(String);

impl fmt::Display for RecoveryRequired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "edit recovery files retained at original locations {}; directory renames can change these paths",
            self.0
        )
    }
}

impl std::error::Error for RecoveryRequired {}

pub(super) fn recovery_required(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RecoveryRequired>().is_some()
}

impl<'a> Transaction<'a> {
    pub(super) fn commit(files: &'a [PreparedFile]) -> Result<Self> {
        Self::commit_with(files, &mut |_, _| Ok(()))
    }

    fn commit_with(
        files: &'a [PreparedFile],
        hook: &mut impl FnMut(Boundary, usize) -> Result<()>,
    ) -> Result<Self> {
        let mut transaction = Self {
            files,
            staged: Vec::with_capacity(files.len()),
        };
        if let Err(error) = transaction.stage() {
            return Err(transaction.recover(error, hook));
        }
        let result = (|| {
            hook(Boundary::Staged, 0)?;
            for index in 0..files.len() {
                transaction.install(index, hook)?;
            }
            hook(Boundary::BeforeVerify, 0)?;
            transaction.verify_committed()
        })();
        if let Err(error) = result {
            return Err(transaction.recover(error, hook));
        }
        Ok(transaction)
    }

    fn stage(&mut self) -> Result<()> {
        for file in self.files {
            file.verify_requested_path()?;
            ensure!(
                hash(&file.location.read()?) == hash(&file.preimage),
                "{} changed while the edit was being prepared",
                file.path
            );
            // Older versions used these names. Never erase their recovery data.
            for legacy in [&file.temporary, &file.backup] {
                ensure!(
                    !exists(
                        &file.location.directory,
                        legacy.file_name().context("invalid edit sidecar name")?
                    )?,
                    "retained edit sidecar already exists at {}",
                    legacy.display()
                );
            }
            let path = file.recovery_path();
            let name = path
                .file_name()
                .context("invalid edit recovery directory")?;
            let recovery = Recovery::create(&file.location, &path, name)?;
            self.staged.push(StagedFile {
                recovery: Some(recovery),
                state: State::Staged,
            });
            let recovery = self
                .staged
                .last()
                .context("staged file is missing")?
                .recovery()?;
            let mut output = recovery.create_file(POSTIMAGE)?;
            output.write_all(&file.postimage)?;
            let permissions = file
                .location
                .directory
                .metadata(&file.location.name)?
                .permissions();
            output.set_permissions(permissions)?;
            output
                .sync_all()
                .context("synchronize staged edit postimage")?;
            drop(output);
            // Test no-replace publication support before displacing any source.
            recovery
                .directory
                .hard_link(POSTIMAGE, &recovery.directory, "link-probe")
                .context("edits require hard-link support on the checkout filesystem")?;
            recovery.directory.remove_file("link-probe")?;
        }
        Ok(())
    }

    fn install(
        &mut self,
        index: usize,
        hook: &mut impl FnMut(Boundary, usize) -> Result<()>,
    ) -> Result<()> {
        let file = &self.files[index];
        let staged = &mut self.staged[index];
        hook(Boundary::BeforeDisplace, index)?;
        file.verify_requested_path()?;
        let recovery = staged.recovery()?;
        ensure!(
            !recovery.exists(PREIMAGE)?,
            "preimage recovery slot already exists"
        );
        // This slot is in a new private directory owned by this transaction.
        // Source renames never replace another live source or recovery version.
        file.location
            .directory
            .rename(&file.location.name, &recovery.directory, PREIMAGE)
            .with_context(|| format!("retain displaced preimage for {}", file.path))?;
        staged.state = State::Displaced;
        hook(Boundary::AfterDisplace, index)?;
        file.location.verify()?;
        let recovery = staged.recovery()?;
        ensure!(
            hash(&recovery.read(PREIMAGE)?) == hash(&file.preimage),
            "{} changed after staging; the displaced bytes differ from the approved preimage",
            file.path
        );
        // Permission changes made after staging belong to the displaced
        // version too. Preserve them instead of restoring stale permissions.
        let permissions = recovery.directory.metadata(PREIMAGE)?.permissions();
        recovery.directory.set_permissions(POSTIMAGE, permissions)?;
        recovery.publish(POSTIMAGE, &file.location)?;
        staged.state = State::Installed;
        hook(Boundary::AfterInstall, index)?;
        Ok(())
    }

    fn verify_committed(&self) -> Result<()> {
        for (file, staged) in self.files.iter().zip(&self.staged) {
            file.verify_requested_path()?;
            ensure!(
                hash(&staged.recovery()?.read(PREIMAGE)?) == hash(&file.preimage),
                "displaced preimage for {} changed during commit",
                file.path
            );
            let recovery = staged.recovery()?;
            ensure!(
                recovery.directory.metadata(PREIMAGE)?.permissions()
                    == recovery.directory.metadata(POSTIMAGE)?.permissions(),
                "displaced file permissions for {} changed during commit",
                file.path
            );
            ensure!(
                hash(&file.location.read()?) == file.postimage_hash,
                "{} changed during commit",
                file.path
            );
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<()> {
        self.finish_with(&mut |_, _| Ok(()))
    }

    fn finish_with(mut self, hook: &mut impl FnMut(Boundary, usize) -> Result<()>) -> Result<()> {
        let result = hook(Boundary::BeforeCleanup, 0)
            .and_then(|()| self.verify_committed())
            .and_then(|()| self.cleanup());
        result.map_err(|error| self.recovery_context(error))
    }

    pub(super) fn rollback_error(mut self, error: anyhow::Error) -> anyhow::Error {
        self.recover(error, &mut |_, _| Ok(()))
    }

    fn recover(
        &mut self,
        error: anyhow::Error,
        hook: &mut impl FnMut(Boundary, usize) -> Result<()>,
    ) -> anyhow::Error {
        let mut failures = Vec::new();
        for index in (0..self.staged.len()).rev() {
            if let Err(error) = self.restore(index, hook) {
                failures.push(format!("{}: {error:#}", self.files[index].path));
            }
        }
        if !failures.is_empty() {
            return self.recovery_context(
                error.context(format!("rollback is incomplete: {}", failures.join("; "))),
            );
        }
        let cleanup = hook(Boundary::BeforeCleanup, 0).and_then(|()| self.cleanup());
        match cleanup {
            Ok(()) => error,
            Err(cleanup) => self
                .recovery_context(error.context(format!("recovery cleanup stopped: {cleanup:#}"))),
        }
    }

    fn restore(
        &mut self,
        index: usize,
        hook: &mut impl FnMut(Boundary, usize) -> Result<()>,
    ) -> Result<()> {
        let file = &self.files[index];
        let staged = &mut self.staged[index];
        if matches!(staged.state, State::Staged | State::Restored) {
            return Ok(());
        }
        file.location.verify()?;
        if staged.state == State::Installed && file.location.exists()? {
            ensure!(
                hash(&file.location.read()?) == file.postimage_hash,
                "current file changed; rollback will not replace it"
            );
            hook(Boundary::BeforeRollbackDisplace, index)?;
            file.verify_requested_path()?;
            let recovery = staged.recovery()?;
            ensure!(
                !recovery.exists(ROLLBACK)?,
                "rollback recovery slot already exists"
            );
            file.location
                .directory
                .rename(&file.location.name, &recovery.directory, ROLLBACK)?;
            staged.state = State::RollbackDisplaced;
            hook(Boundary::AfterRollbackDisplace, index)?;
            file.location.verify()?;
            let recovery = staged.recovery()?;
            if hash(&recovery.read(ROLLBACK)?) != file.postimage_hash {
                // The editor saved between the check and rename. Put those
                // bytes back only if no still-newer replacement occupies it.
                recovery.publish(ROLLBACK, &file.location)?;
                return Err(anyhow!(
                    "file changed during rollback; concurrent bytes were restored"
                ));
            }
        }
        hook(Boundary::BeforeRestore, index)?;
        file.location.verify()?;
        staged.recovery()?.publish(PREIMAGE, &file.location)?;
        staged.state = State::Restored;
        Ok(())
    }

    fn cleanup(&mut self) -> Result<()> {
        // Validate all retained contents before removing any recovery file.
        // A backup modified through an old descriptor is unique data, even
        // when rollback already linked those bytes back to the source name.
        for (file, staged) in self.files.iter().zip(&self.staged) {
            let recovery = staged.recovery()?;
            for (name, expected) in [
                (PREIMAGE, hash(&file.preimage)),
                (POSTIMAGE, file.postimage_hash.clone()),
                (ROLLBACK, file.postimage_hash.clone()),
            ] {
                if recovery.exists(name)? {
                    ensure!(
                        hash(&recovery.read(name)?) == expected,
                        "unexpected bytes remain in {}",
                        recovery.path.join(name).display()
                    );
                }
            }
        }
        for (file, staged) in self.files.iter().zip(&mut self.staged) {
            let recovery = staged.recovery()?;
            for (name, expected) in [
                (PREIMAGE, hash(&file.preimage)),
                (POSTIMAGE, file.postimage_hash.clone()),
                (ROLLBACK, file.postimage_hash.clone()),
            ] {
                if recovery.exists(name)? {
                    // Recheck immediately before unlinking. Earlier files can
                    // take time to clean up in a large transaction.
                    ensure!(
                        hash(&recovery.read(name)?) == expected,
                        "unexpected bytes remain in {}",
                        recovery.path.join(name).display()
                    );
                    recovery.directory.remove_file(OsStr::new(name))?;
                }
            }
            let recovery = staged
                .recovery
                .take()
                .context("recovery directory is missing")?;
            recovery.remove_empty(&file.location)?;
        }
        Ok(())
    }

    fn recovery_context(&self, error: anyhow::Error) -> anyhow::Error {
        let paths = self
            .files
            .iter()
            .take(self.staged.len())
            .map(|file| file.recovery_path().display().to_string())
            .collect::<Vec<_>>();
        error.context(RecoveryRequired(paths.join(", ")))
    }
}

impl StagedFile {
    fn recovery(&self) -> Result<&Recovery> {
        self.recovery
            .as_ref()
            .context("edit recovery directory is missing")
    }
}

#[cfg(test)]
mod tests;
