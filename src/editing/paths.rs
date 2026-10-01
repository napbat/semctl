//! Directory capabilities for edit mutations.
//!
//! Absolute paths are diagnostic labels. Every source mutation uses the held
//! parent directory, so an ancestor rename cannot redirect an operation through
//! a replacement symlink. Location checks also reject a detached checkout.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use cap_std::fs::{Dir, DirBuilder, OpenOptions};
use same_file::Handle;

pub(super) struct Checkout {
    pub(super) path: PathBuf,
    directory: Dir,
    identity: Handle,
}

impl Checkout {
    pub(super) fn open(path: &Path) -> Result<Arc<Self>> {
        let directory = Dir::open_ambient_dir(path, cap_std::ambient_authority())
            .with_context(|| format!("open checkout directory {}", path.display()))?;
        let identity = identity(&directory)?;
        let checkout = Arc::new(Self {
            path: path.to_path_buf(),
            directory,
            identity,
        });
        checkout.verify()?;
        Ok(checkout)
    }

    fn verify(&self) -> Result<()> {
        let current = Dir::open_ambient_dir(&self.path, cap_std::ambient_authority())
            .context("reopen checkout directory")?;
        ensure!(
            identity(&current)? == self.identity,
            "checkout directory changed during the edit"
        );
        Ok(())
    }
}

pub(super) struct Target {
    checkout: Arc<Checkout>,
    pub(super) directory: Dir,
    pub(super) name: OsString,
    parent: PathBuf,
    identity: Handle,
}

impl Target {
    pub(super) fn bind(checkout: &Arc<Checkout>, target: &Path) -> Result<Self> {
        let relative = target
            .strip_prefix(&checkout.path)
            .context("edit target escapes the checkout")?;
        let parent = relative.parent().context("edit target has no parent")?;
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let directory = checkout
            .directory
            .open_dir(parent)
            .context("open edit target directory within checkout")?;
        let identity = identity(&directory)?;
        let bound = Self {
            checkout: Arc::clone(checkout),
            directory,
            name: relative
                .file_name()
                .context("edit target has no name")?
                .into(),
            parent: parent.into(),
            identity,
        };
        bound.verify()?;
        Ok(bound)
    }

    pub(super) fn verify(&self) -> Result<()> {
        self.checkout.verify()?;
        let current = self
            .checkout
            .directory
            .open_dir(&self.parent)
            .context("edit target directory no longer belongs to the checkout")?;
        ensure!(
            identity(&current)? == self.identity,
            "edit target directory changed during the edit"
        );
        Ok(())
    }

    pub(super) fn verify_requested_path(&self, requested: &str, target: &Path) -> Result<()> {
        self.verify()?;
        // Preserve support for absolute symlink destinations inside this
        // checkout. This lookup only compares names; mutations use held dirs.
        let current = std::fs::canonicalize(self.checkout.path.join(requested))
            .context("resolve the planned edit path within its original checkout")?;
        ensure!(
            current == target,
            "planned edit path changed its target during the edit"
        );
        Ok(())
    }

    pub(super) fn read(&self) -> Result<Vec<u8>> {
        read_regular(&self.directory, &self.name)
    }

    pub(super) fn exists(&self) -> Result<bool> {
        exists(&self.directory, &self.name)
    }
}

pub(super) struct Recovery {
    pub(super) directory: Dir,
    pub(super) path: PathBuf,
    name: OsString,
}

impl Recovery {
    pub(super) fn create(target: &Target, display: &Path, name: &OsStr) -> Result<Self> {
        #[cfg(unix)]
        let options = {
            use cap_std::fs::DirBuilderExt as _;
            let mut options = DirBuilder::new();
            options.mode(0o700);
            options
        };
        #[cfg(not(unix))]
        let options = DirBuilder::new();
        target
            .directory
            .create_dir_with(name, &options)
            .with_context(|| {
                format!(
                    "create private edit recovery directory {}",
                    display.display()
                )
            })?;
        let directory = target.directory.open_dir(name)?;
        Ok(Self {
            directory,
            path: display.to_path_buf(),
            name: name.into(),
        })
    }

    pub(super) fn create_file(&self, name: &str) -> Result<cap_std::fs::File> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        self.directory.open_with(name, &options).with_context(|| {
            format!(
                "create edit recovery file {}",
                self.path.join(name).display()
            )
        })
    }

    pub(super) fn read(&self, name: &str) -> Result<Vec<u8>> {
        read_regular(&self.directory, OsStr::new(name))
    }

    pub(super) fn exists(&self, name: &str) -> Result<bool> {
        exists(&self.directory, OsStr::new(name))
    }

    /// A hard link publishes a complete file only when the target is absent.
    /// It cannot overwrite a replacement that an editor created during a swap.
    pub(super) fn publish(&self, name: &str, target: &Target) -> Result<()> {
        self.directory
            .hard_link(name, &target.directory, &target.name)
            .context("install edit without replacing a concurrently created target")
    }

    pub(super) fn remove_empty(self, target: &Target) -> Result<()> {
        // Close the directory handle before removal on Windows. Remove only an
        // empty directory; unexpected entries must remain available for recovery.
        drop(self.directory);
        target
            .directory
            .remove_dir(&self.name)
            .context("remove empty edit recovery directory")
    }
}

fn identity(directory: &Dir) -> Result<Handle> {
    Handle::from_file(directory.try_clone()?.into_std_file()).context("read directory identity")
}

fn read_regular(directory: &Dir, name: &OsStr) -> Result<Vec<u8>> {
    ensure!(
        directory.symlink_metadata(name)?.is_file(),
        "edit target or recovery entry is no longer a regular file"
    );
    directory.read(name).context("read verified edit file")
}

pub(super) fn exists(directory: &Dir, name: &OsStr) -> Result<bool> {
    match directory.symlink_metadata(name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("check edit recovery entry"),
    }
}
