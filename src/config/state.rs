//! One persistent lock and generation for configuration and login state.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};

use super::{Config, atomic_write_private, load_from, lock_file, open_lock};

#[derive(Clone)]
pub(crate) struct StateStore {
    pub(crate) current: PathBuf,
    pub(crate) legacy: PathBuf,
}

pub(crate) struct StateGuard {
    _file: File,
}

impl StateStore {
    pub(crate) fn configured() -> Result<Self> {
        Ok(Self {
            current: super::config_dir()?,
            legacy: super::legacy_config_dir()?,
        })
    }

    fn control_path(&self, extension: &str) -> Result<PathBuf> {
        let parent = self
            .current
            .parent()
            .context("state directory has no parent")?;
        let name = self
            .current
            .file_name()
            .context("state directory has no name")?;
        let mut control = std::ffi::OsString::from(".");
        control.push(name);
        control.push(format!(".state.{extension}"));
        Ok(parent.join(control))
    }

    /// The lock and generation remain outside the directory removed by purge.
    /// Every config and credential writer acquires this lock before publication.
    pub(crate) fn lock_blocking(&self) -> Result<StateGuard> {
        Ok(StateGuard {
            _file: lock_file(&self.control_path("lock")?)?,
        })
    }

    pub(crate) async fn lock(&self) -> Result<StateGuard> {
        let path = self.control_path("lock")?;
        let file = tokio::task::spawn_blocking(move || open_lock(&path))
            .await
            .context("wait for state lock file")??;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(StateGuard { _file: file }),
                Err(fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(fs::TryLockError::Error(error)) => {
                    return Err(error).context("lock login state");
                }
            }
        }
    }

    pub(crate) fn generation(&self) -> Result<u64> {
        let path = self.control_path("generation")?;
        match fs::read_to_string(&path) {
            Ok(value) => value.trim().parse().context("parse login state generation"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
        }
    }

    pub(crate) fn check_generation(&self, expected: u64) -> Result<()> {
        ensure!(
            self.generation()? == expected,
            "login state changed while this operation was pending; retry the command"
        );
        Ok(())
    }

    /// The caller holds the state lock. Advance before publishing or deleting
    /// state so interrupted transitions leave old credentials unusable.
    pub(crate) fn advance(&self) -> Result<u64> {
        let generation = self
            .generation()?
            .checked_add(1)
            .context("login generation overflow")?;
        atomic_write_private(
            &self.control_path("generation")?,
            generation.to_string().as_bytes(),
        )?;
        Ok(generation)
    }

    pub(crate) fn load_config(&self) -> Result<Config> {
        load_from(
            &self.current.join("config.toml"),
            &self.legacy.join("config.toml"),
        )
    }

    /// The caller holds the state lock.
    pub(crate) fn save_config(&self, cfg: &Config) -> Result<()> {
        let text = toml::to_string_pretty(cfg).context("serialize config")?;
        atomic_write_private(&self.current.join("config.toml"), text.as_bytes())
    }

    pub(crate) fn update_config(
        &self,
        generation: u64,
        change: impl FnOnce(&mut Config),
    ) -> Result<Config> {
        let _lock = self.lock_blocking()?;
        self.check_generation(generation)?;
        let mut cfg = self.load_config()?;
        change(&mut cfg);
        self.save_config(&cfg)?;
        Ok(cfg)
    }

    pub(crate) fn purge(&self) -> Result<bool> {
        let _lock = self.lock_blocking()?;
        self.advance()?;
        remove_directory(&self.current)
    }
}

fn remove_directory(path: &Path) -> Result<bool> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}
