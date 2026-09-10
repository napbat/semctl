//! One checked source policy for manifest walks and filesystem notifications.
//!
//! Rule bytes are compiled once through `ignore`'s public matcher. Filesystem
//! errors are never converted into empty rules. The snapshot records absent
//! rules too, so creating, replacing, or removing a rule during a scan aborts it.

mod git;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::blocking::Cancellation;
use super::walker::IGNORE_FILES;

#[derive(Clone, PartialEq, Eq)]
enum State {
    Missing,
    Directory(PathBuf),
    File(blake3::Hash),
}

#[derive(Default)]
struct Sources(BTreeMap<PathBuf, State>);

impl Sources {
    fn inspect(path: &Path) -> Result<(State, Option<Vec<u8>>)> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_symlink() => fs::metadata(path)
                .with_context(|| format!("resolve policy file {}", path.display()))?,
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((State::Missing, None));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect policy file {}", path.display()));
            }
        };
        if metadata.is_dir() {
            return Ok((
                State::Directory(
                    fs::canonicalize(path)
                        .with_context(|| format!("resolve policy directory {}", path.display()))?,
                ),
                None,
            ));
        }
        ensure!(
            metadata.is_file() || is_null_device(path, &metadata),
            "policy path is not a regular file: {}",
            path.display()
        );
        let bytes =
            fs::read(path).with_context(|| format!("read policy file {}", path.display()))?;
        Ok((State::File(blake3::hash(&bytes)), Some(bytes)))
    }

    fn observe(&mut self, path: &Path) -> Result<(State, Option<Vec<u8>>)> {
        let (state, bytes) = Self::inspect(path)?;
        if let Some(previous) = self.0.get(path) {
            ensure!(
                previous == &state,
                "source policy changed during sync: {}",
                path.display()
            );
        } else {
            self.0.insert(path.to_path_buf(), state.clone());
        }
        Ok((state, bytes))
    }

    fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
        let (state, bytes) = self.observe(path)?;
        ensure!(
            !matches!(state, State::Directory(_)),
            "policy path is a directory: {}",
            path.display()
        );
        Ok(bytes)
    }

    fn rules(
        &mut self,
        root: &Path,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Gitignore> {
        let mut builder = GitignoreBuilder::new(root);
        for path in paths {
            let Some(bytes) = self.read(&path)? else {
                continue;
            };
            let text = std::str::from_utf8(&bytes)
                .with_context(|| format!("policy file is not UTF-8: {}", path.display()))?;
            for (index, line) in text.lines().enumerate() {
                let line = if index == 0 {
                    line.trim_start_matches('\u{feff}')
                } else {
                    line
                };
                builder
                    .add_line(Some(path.clone()), line)
                    .with_context(|| {
                        format!("parse ignore rule {}:{}", path.display(), index + 1)
                    })?;
            }
        }
        builder.build().context("compile source policy")
    }

    fn verify(&self, cancellation: &Cancellation) -> Result<()> {
        for (path, expected) in &self.0 {
            cancellation.check()?;
            ensure!(
                &Self::inspect(path)?.0 == expected,
                "source policy changed during sync: {}",
                path.display()
            );
        }
        Ok(())
    }
}

fn is_null_device(path: &Path, metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        // Git commonly uses /dev/null to disable a config or ignore file.
        // Other devices and pipes can block, so only this empty input is safe.
        metadata.file_type().is_char_device()
            && fs::canonicalize(path).is_ok_and(|resolved| resolved == Path::new("/dev/null"))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, metadata);
        false
    }
}

struct DirectoryRules {
    custom: Gitignore,
    ignore: Gitignore,
    gitignore: Gitignore,
    exclude: Gitignore,
    global: Option<Gitignore>,
}

pub(super) struct SourcePolicy {
    root: PathBuf,
    sources: Sources,
    directories: BTreeMap<PathBuf, DirectoryRules>,
    configurations: BTreeMap<PathBuf, git::Configuration>,
    global: Gitignore,
}

impl SourcePolicy {
    pub(super) fn load(root: &Path, cancellation: &Cancellation) -> Result<Self> {
        let mut sources = Sources::default();
        let configuration = git::Configuration::load(root, &mut sources, cancellation)?;
        let global = sources.rules(root, configuration.excludes.iter().cloned())?;
        let mut policy = Self {
            root: root.to_path_buf(),
            sources,
            directories: BTreeMap::new(),
            configurations: BTreeMap::from([(root.to_path_buf(), configuration)]),
            global,
        };
        let ancestors: Vec<_> = root.ancestors().map(Path::to_path_buf).collect();
        for directory in ancestors.iter().rev() {
            policy.load_directory(directory, cancellation)?;
        }
        Ok(policy)
    }

    pub(super) fn load_directory(
        &mut self,
        directory: &Path,
        cancellation: &Cancellation,
    ) -> Result<()> {
        cancellation.check()?;
        if self.directories.contains_key(directory) {
            return Ok(());
        }
        let custom = self.sources.rules(
            directory,
            IGNORE_FILES.iter().map(|name| directory.join(name)),
        )?;
        let ignore = self.sources.rules(directory, [directory.join(".ignore")])?;
        let gitignore = self
            .sources
            .rules(directory, [directory.join(".gitignore")])?;
        let git_dir = git::common_directory(directory, &mut self.sources)?;
        let (exclude, global) = if let Some(git_dir) = git_dir {
            let exclude = self
                .sources
                .rules(directory, [git_dir.join("info/exclude")])?;
            if !self.configurations.contains_key(directory) {
                let configuration =
                    git::Configuration::load(directory, &mut self.sources, cancellation)?;
                self.configurations
                    .insert(directory.to_path_buf(), configuration);
            }
            let configuration = &self.configurations[directory];
            let global = self
                .sources
                .rules(directory, configuration.excludes.iter().cloned())?;
            (exclude, Some(global))
        } else {
            (Gitignore::empty(), None)
        };
        self.directories.insert(
            directory.to_path_buf(),
            DirectoryRules {
                custom,
                ignore,
                gitignore,
                exclude,
                global,
            },
        );
        Ok(())
    }

    /// Match precedence follows `ignore`: custom rules, `.ignore`, `.gitignore`,
    /// repository excludes, then global excludes. Within one class, the closest
    /// directory wins. A whitelist overrides the hidden-file default.
    pub(super) fn includes(&self, path: &Path, is_dir: bool) -> bool {
        if is_private_path(path) {
            return false;
        }
        if path
            .strip_prefix(&self.root)
            .is_ok_and(|relative| relative.components().any(|part| part.as_os_str() == ".git"))
        {
            return false;
        }
        let layers: Vec<_> = path
            .ancestors()
            .filter_map(|directory| self.directories.get(directory))
            .collect();
        for select in [
            (|rules: &DirectoryRules| &rules.custom) as fn(&DirectoryRules) -> &Gitignore,
            |rules| &rules.ignore,
            |rules| &rules.gitignore,
            |rules| &rules.exclude,
        ] {
            for rules in &layers {
                let matched = select(rules).matched(path, is_dir);
                if !matched.is_none() {
                    return matched.is_whitelist();
                }
            }
        }
        let global = layers
            .iter()
            .find_map(|rules| rules.global.as_ref())
            .unwrap_or(&self.global);
        let matched = global.matched(path, is_dir);
        if !matched.is_none() {
            return matched.is_whitelist();
        }
        !path
            .file_name()
            .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."))
    }

    /// Events use the same rules as the scan. A changed rule always wakes the
    /// scanner, even if that rule file is itself excluded from the manifest.
    pub(super) fn event_is_relevant(
        &mut self,
        path: &Path,
        is_dir: bool,
        cancellation: &Cancellation,
    ) -> Result<bool> {
        let absolute = self.root.join(path);
        let path = absolute.as_path();
        if is_private_path(path) {
            return Ok(false);
        }
        if self
            .observed_sources()
            .any(|source| source == path || source.starts_with(path))
            || is_rule_path(path)
        {
            return Ok(true);
        }
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return Ok(false);
        };
        let mut directory = self.root.clone();
        if let Some(parent) = relative.parent() {
            for component in parent.components() {
                directory.push(component);
                if !self.includes(&directory, true) {
                    return Ok(false);
                }
                self.load_directory(&directory, cancellation)?;
            }
        }
        Ok(self.includes(path, is_dir))
    }

    pub(super) fn verify(&self, cancellation: &Cancellation) -> Result<()> {
        self.sources.verify(cancellation)?;
        for configuration in self.configurations.values() {
            configuration.verify(cancellation)?;
        }
        // Configuration resolution can take time. Recheck rule bytes after it
        // so a rule changed during a Git probe cannot pass this verification.
        self.sources.verify(cancellation)
    }

    pub(super) fn external_sources(&self) -> impl Iterator<Item = PathBuf> {
        self.observed_sources()
            .filter(|path| !path.starts_with(&self.root))
    }

    pub(super) fn observed_sources(&self) -> impl Iterator<Item = PathBuf> {
        // Watch both the logical path and its target. The former detects a
        // retargeted symlink; the latter detects edits through another alias.
        self.sources
            .0
            .keys()
            .flat_map(|path| [path.clone(), watch_target(path)])
    }
}

fn watch_target(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(canonical) = fs::canonicalize(ancestor) {
            return path
                .strip_prefix(ancestor)
                .map_or(canonical.clone(), |suffix| canonical.join(suffix));
        }
    }
    path.to_path_buf()
}

/// Recovery bytes and private process output are never source, even when a
/// project whitelist includes hidden files. Check ancestors to cover contents.
pub(super) fn is_private_path(path: &Path) -> bool {
    path.components().any(|part| {
        part.as_os_str().to_str().is_some_and(|name| {
            let Some(stem) = name
                .strip_suffix(".edit")
                .or_else(|| name.strip_suffix(".tmp"))
                .or_else(|| name.strip_suffix(".bak"))
                .and_then(|name| name.strip_prefix(".semctl-"))
            else {
                return false;
            };
            let Some((digest, index)) = stem.split_once('-') else {
                return false;
            };
            digest.len() == 12
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && !index.is_empty()
                && index.bytes().all(|byte| byte.is_ascii_digit())
                && (index == "0" || !index.starts_with('0'))
        })
    })
}

/// Filter unrelated events before loading policy. In particular, probing Git
/// creates private scratch files under the temporary directory; those events
/// must not cause another policy probe through an external parent watch.
pub(super) fn event_may_affect_policy(
    root: &Path,
    path: &Path,
    observed: &std::collections::HashSet<PathBuf>,
) -> bool {
    if is_private_path(path) {
        return false;
    }
    if observed.contains(path) || observed.iter().any(|source| source.starts_with(path)) {
        return true;
    }
    path.strip_prefix(root).is_ok_and(|relative| {
        is_rule_path(path) || !relative.components().any(|part| part.as_os_str() == ".git")
    })
}

fn is_rule_path(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name == ".git"
            || name == ".gitignore"
            || name == ".ignore"
            || IGNORE_FILES.iter().any(|value| name == *value)
    }) || path.ends_with(".git/config")
        || path.ends_with(".git/info/exclude")
}

#[cfg(test)]
mod tests;
