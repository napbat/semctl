//! Filesystem walker for `semctl index` and the `semctl mcp` auto-index.
//!
//! Produces the candidate file list both sync paths share. The policy engine
//! reads and validates all effective rules. The scanner then reads and hashes
//! every accepted source file. Conventions:
//!   - `.gitignore` honored even outside a git checkout (`require_git(false)`),
//!     including nested ignore files in non-repository workspaces;
//!   - project-local `.semctxignore` and `.semctlignore` files;
//!   - a built-in file-glob backstop ([`DEFAULT_EXCLUDE_FILE_GLOBS`]) for junk
//!     that often isn't gitignored — lockfiles, minified/map assets, and
//!     generated protobuf code;
//!   - a byte-size cap.
//!
//! Content-level hygiene (empty / generated / minified) is [`is_indexable`],
//! which the caller applies once it has actually read a new or changed file.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use ignore::overrides::OverrideBuilder;

use super::blocking::Cancellation;
use super::policy::SourcePolicy;

pub(super) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Both names remain supported at the indexing boundary.
pub(super) const IGNORE_FILES: &[&str] = &[".semctxignore", ".semctlignore"];

/// A file accepted by the source policy. The scanner verifies its content.
#[derive(Debug)]
pub struct Candidate {
    /// Forward-slashed path relative to the walk root (the server's key).
    pub rel: String,
}

/// Tunables for [`walk`]. [`Default`] matches what the server can embed.
#[derive(Clone, Copy)]
pub struct WalkOptions {
    /// Skip files larger than this — almost always assets/binaries, not source.
    /// Sized to admit the occasional big source file (generated code, large
    /// fixtures) while staying under the server's ~30 MB request-body limit; a
    /// file this size uploads in its own PUT (see `UPLOAD_BATCH_BYTES`).
    pub max_file_bytes: u64,
    /// Gitignore-style file globs to exclude on top of project ignore files.
    pub excludes: &'static [&'static str],
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            max_file_bytes: MAX_FILE_BYTES,
            excludes: DEFAULT_EXCLUDE_FILE_GLOBS,
        }
    }
}

/// Walk `root`, returning the candidate files sorted by path for a stable
/// manifest. Directories, oversized files, gitignored paths, and the built-in
/// exclude globs are filtered out. No file contents are read.
pub(super) struct WalkResult {
    pub(super) candidates: Vec<Candidate>,
    pub(super) policy: SourcePolicy,
}

pub(super) fn walk(
    root: &Path,
    opts: &WalkOptions,
    cancellation: &Cancellation,
) -> Result<WalkResult> {
    cancellation.check()?;
    let metadata = std::fs::metadata(root)
        .with_context(|| format!("read checkout root {}", root.display()))?;
    ensure!(
        metadata.is_dir(),
        "checkout root is not a directory: {}",
        root.display()
    );
    let mut policy = SourcePolicy::load(root, cancellation)?;
    let excludes = exclude_overrides(root, opts.excludes);
    let mut pending = vec![root.to_path_buf()];
    let mut out = Vec::new();
    while let Some(directory) = pending.pop() {
        cancellation.check()?;
        policy.load_directory(&directory, cancellation)?;
        for entry in std::fs::read_dir(&directory)
            .with_context(|| format!("walk {}", directory.display()))?
        {
            cancellation.check()?;
            let entry = entry.with_context(|| format!("walk {}", directory.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("read file type for {}", path.display()))?;
            if !file_type.is_dir() && !file_type.is_file() {
                continue;
            }
            if excludes.matched(&path, file_type.is_dir()).is_ignore()
                || !policy.includes(&path, file_type.is_dir())
            {
                continue;
            }
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            let metadata = entry
                .metadata()
                .with_context(|| format!("read metadata for {}", path.display()))?;
            if metadata.len() <= opts.max_file_bytes {
                out.push(Candidate {
                    rel: rel_path(root, &path)?,
                });
            }
        }
    }
    policy.verify(cancellation)?;
    out.sort_unstable_by(|a, b| a.rel.cmp(&b.rel));
    Ok(WalkResult {
        candidates: out,
        policy,
    })
}

/// Whether a file's *content* is worth indexing: non-blank, not machine-
/// generated, and not minified.
pub fn is_indexable(content: &str) -> bool {
    has_uploadable_content(content) && !looks_generated(content) && !looks_minified(content)
}

/// The server validates uploaded `Content` with a required-string rule, which
/// rejects whitespace-only strings as well as `""`.
pub(super) fn has_uploadable_content(content: &str) -> bool {
    !content.trim().is_empty()
}

/// File globs excluded on top of project ignore files. Directories are never
/// excluded by default: users opt into that policy with `.gitignore`, `.ignore`,
/// or `.semctlignore` so potentially useful vendored/test source stays visible.
pub const DEFAULT_EXCLUDE_FILE_GLOBS: &[&str] = &[
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "*.svg",
    "*.min.js",
    "*.min.css",
    "*.map",
    "*.pb.go",
    "*.pb.py",
    "*_pb2.py",
    "*.pb.cc",
    "*_generated.go",
    "*.gen.go",
];

/// Compile the built-in backstop into the walker's native matcher. Overrides
/// run before entries are yielded, so matching files are never statted by our
/// scan loop. The default list intentionally contains no directory patterns.
fn exclude_overrides(root: &Path, patterns: &[&str]) -> ignore::overrides::Override {
    let mut builder = OverrideBuilder::new(root);
    for pattern in patterns {
        // Override syntax inverts gitignore's `!`: a leading `!` means ignore.
        builder
            .add(&format!("!{pattern}"))
            .expect("built-in exclude glob must be valid");
    }
    builder
        .build()
        .expect("built-in exclude matcher must compile")
}

/// Forward-slashed path of `file` relative to `root` — the form the server keys
/// on. `None` if `file` isn't under `root`.
fn rel_path(root: &Path, file: &Path) -> Result<String> {
    let rel = file
        .strip_prefix(root)
        .context("file is outside the checkout root")?;
    let mut normalized = String::new();
    for component in rel.components() {
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(
            component
                .as_os_str()
                .to_str()
                .context("file path is not UTF-8")?,
        );
    }
    Ok(normalized)
}

/// Conservative generated-file detection: scan the first few lines for the
/// conventional banners tools emit. Only the head is inspected so a stray match
/// deep in a hand-written file doesn't disqualify it.
fn looks_generated(content: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "@generated",
        "DO NOT EDIT",
        "Code generated by",
        "autogenerated",
        "auto-generated",
    ];
    content
        .lines()
        .take(8)
        .any(|line| MARKERS.iter().any(|m| line.contains(m)))
}

/// Heuristic minified-file detection: a single line longer than this almost
/// never occurs in hand-written source but is the norm for bundled/minified
/// assets. Cheap and good enough — anything genuinely huge also trips the size
/// cap first.
fn looks_minified(content: &str) -> bool {
    const MAX_LINE_BYTES: usize = 2_000;
    content.lines().any(|line| line.len() > MAX_LINE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn walk(
        root: &Path,
        opts: &WalkOptions,
        cancellation: &Cancellation,
    ) -> Result<Vec<Candidate>> {
        super::walk(root, opts, cancellation).map(|walk| walk.candidates)
    }

    fn rels(files: &[Candidate]) -> Vec<&str> {
        files.iter().map(|c| c.rel.as_str()).collect()
    }

    #[test]
    fn honors_nested_gitignore_without_a_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // No `.git` is ever created: this only passes with require_git(false).
        fs::create_dir_all(root.join("svc/bin")).unwrap();
        fs::write(root.join("svc/.gitignore"), "bin/\n").unwrap();
        fs::write(root.join("svc/bin/artifact.json"), "{\"built\": true}\n").unwrap();
        fs::write(root.join("svc/main.rs"), "fn main() {}\n").unwrap();

        let files = walk(root, &WalkOptions::default(), &Cancellation::default()).unwrap();
        let names = rels(&files);
        assert!(names.contains(&"svc/main.rs"), "got {names:?}");
        assert!(
            !names.iter().any(|n| n.contains("bin/")),
            "nested .gitignore not applied: {names:?}"
        );
    }

    #[test]
    fn excludes_default_file_globs_without_hiding_source_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("real.rs"), "fn a() {}\n").unwrap();
        fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        fs::write(root.join("node_modules/dep/index.js"), "x\n").unwrap();
        fs::create_dir_all(root.join("vendor/lib")).unwrap();
        fs::write(root.join("vendor/lib/source.go"), "package lib\n").unwrap();
        fs::create_dir_all(root.join("testdata/case")).unwrap();
        fs::write(root.join("testdata/case/input.rs"), "fn input() {}\n").unwrap();
        fs::write(root.join("Cargo.lock"), "[[package]]\n").unwrap();
        fs::write(root.join("app.min.js"), "a\n").unwrap();

        let files = walk(root, &WalkOptions::default(), &Cancellation::default()).unwrap();
        assert_eq!(
            rels(&files),
            vec![
                "node_modules/dep/index.js",
                "real.rs",
                "testdata/case/input.rs",
                "vendor/lib/source.go"
            ]
        );
    }

    #[test]
    fn size_cap_excludes_large_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("small.txt"), "hi\n").unwrap();
        fs::write(root.join("big.txt"), "x\n".repeat(100)).unwrap();

        let opts = WalkOptions {
            max_file_bytes: 10,
            ..WalkOptions::default()
        };
        let files = walk(root, &opts, &Cancellation::default()).unwrap();
        assert_eq!(rels(&files), vec!["small.txt"]);
    }

    #[test]
    fn is_indexable_rejects_empty_generated_minified() {
        assert!(is_indexable("fn main() {}\n"));
        assert!(!is_indexable(""), "empty");
        assert!(!is_indexable(" \n\t\r\n"), "whitespace-only");
        assert!(
            !is_indexable("// @generated by prost\nstruct X;\n"),
            "generated"
        );
        assert!(
            !is_indexable(&format!("var x={};", "1".repeat(3000))),
            "minified"
        );
    }

    #[test]
    fn a_missing_or_non_directory_root_cannot_be_an_empty_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let cancellation = Cancellation::default();
        assert!(
            walk(
                &temp.path().join("missing"),
                &WalkOptions::default(),
                &cancellation
            )
            .is_err()
        );
        let file = temp.path().join("file.txt");
        fs::write(&file, "text").unwrap();
        assert!(walk(&file, &WalkOptions::default(), &cancellation).is_err());
    }

    #[test]
    fn malformed_ignore_rules_abort_the_walk() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(".semctxignore"), "{unterminated\n").unwrap();
        fs::write(temp.path().join("main.rs"), "fn main() {}\n").unwrap();
        assert!(
            walk(
                temp.path(),
                &WalkOptions::default(),
                &Cancellation::default()
            )
            .is_err()
        );
    }

    #[test]
    fn an_unreadable_ignore_path_aborts_the_walk() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".semctxignore")).unwrap();
        fs::write(temp.path().join("main.rs"), "fn main() {}\n").unwrap();
        let error = walk(
            temp.path(),
            &WalkOptions::default(),
            &Cancellation::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("policy path is a directory"));
    }

    #[test]
    fn both_project_ignore_names_apply_in_nested_directories() {
        let temp = tempfile::tempdir().unwrap();
        for (index, name) in IGNORE_FILES.iter().enumerate() {
            let directory = temp.path().join(format!("part{index}"));
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join(name), "private.txt\n").unwrap();
            fs::write(directory.join("private.txt"), "TEST DATA\n").unwrap();
            fs::write(directory.join("main.rs"), "fn main() {}\n").unwrap();
        }
        let files = walk(
            temp.path(),
            &WalkOptions::default(),
            &Cancellation::default(),
        )
        .unwrap();
        assert_eq!(rels(&files), vec!["part0/main.rs", "part1/main.rs"]);
    }
}
