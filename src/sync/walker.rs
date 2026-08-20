//! Filesystem walker for `semctl index` and the `semctl mcp` auto-index.
//!
//! Produces the candidate file list both sync paths share — a **stat-only**
//! traversal (no content read) so the caller can decide, via its hash cache,
//! which files actually need reading. Conventions:
//!   - `.gitignore` honored even outside a git checkout (`require_git(false)`),
//!     including nested ignore files in non-repository workspaces;
//!   - a project-local `.semctlignore`;
//!   - a built-in file-glob backstop ([`DEFAULT_EXCLUDE_FILE_GLOBS`]) for junk
//!     that often isn't gitignored — lockfiles, minified/map assets, and
//!     generated protobuf code;
//!   - a byte-size cap.
//!
//! Content-level hygiene (empty / generated / minified) is [`is_indexable`],
//! which the caller applies once it has actually read a new or changed file.

use std::path::{Path, PathBuf};

use ignore::{WalkBuilder, overrides::OverrideBuilder};

/// A file the walker accepted, with the stamp needed to tell whether it changed
/// since the last sync. No content is read here — that's the caller's job, only
/// on a cache miss.
pub struct Candidate {
    /// Forward-slashed path relative to the walk root (the server's key).
    pub rel: String,
    /// Absolute path, for the caller to read on a cache miss.
    pub path: PathBuf,
    pub mtime_ns: u128,
    pub size: u64,
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
            max_file_bytes: 16 * 1024 * 1024,
            excludes: DEFAULT_EXCLUDE_FILE_GLOBS,
        }
    }
}

/// Walk `root`, returning the candidate files sorted by path for a stable
/// manifest. Directories, oversized files, gitignored paths, and the built-in
/// exclude globs are filtered out. No file contents are read.
pub fn walk(root: &Path, opts: &WalkOptions) -> Vec<Candidate> {
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(true)
        .hidden(true)
        .require_git(false)
        .add_custom_ignore_filename(".semctlignore")
        .overrides(exclude_overrides(root, opts.excludes));

    let mut out = Vec::new();
    for entry in builder.build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() > opts.max_file_bytes {
            continue;
        }
        let path = entry.into_path();
        let Some(rel) = rel_path(root, &path) else {
            continue;
        };
        out.push(Candidate {
            rel,
            path,
            mtime_ns: mtime_ns(&meta),
            size: meta.len(),
        });
    }
    out.sort_unstable_by(|a, b| a.rel.cmp(&b.rel));
    out
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

fn mtime_ns(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

/// Forward-slashed path of `file` relative to `root` — the form the server keys
/// on. `None` if `file` isn't under `root`.
fn rel_path(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let mut normalized = String::new();
    for component in rel.components() {
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(&component.as_os_str().to_string_lossy());
    }
    Some(normalized)
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

        let files = walk(root, &WalkOptions::default());
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

        let files = walk(root, &WalkOptions::default());
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
        let files = walk(root, &opts);
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
}
