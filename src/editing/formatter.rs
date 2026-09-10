//! Format planned postimages through standard input and captured output.
//!
//! The formatter receives no input file argument. Only the edit transaction
//! writes repository files, so module traversal cannot expand its write set.

use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tokio::io::AsyncWriteExt;

use super::{MAX_FILES, PreparedFile, api, hash, resolve_target};

const FORMATTER_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) async fn format(
    root: &Path,
    step: &api::FormatterStep,
    planned: &mut [PreparedFile],
) -> Result<()> {
    let name = formatter_name(&step.program)?;
    validate_arguments(name, &step.arguments)?;
    ensure!(
        !step.paths.is_empty() && step.paths.len() <= MAX_FILES,
        "formatter must name between 1 and {MAX_FILES} planned paths"
    );
    let mut seen = HashSet::new();
    let mut indices = Vec::with_capacity(step.paths.len());
    for path in &step.paths {
        let (normalized, target) = resolve_target(root, path)?;
        let index = planned
            .iter()
            .position(|file| file.target == target)
            .with_context(|| format!("formatter path {normalized} is not part of the edit plan"))?;
        ensure!(seen.insert(index), "duplicate formatter path {normalized}");
        indices.push(index);
    }

    tokio::time::timeout(FORMATTER_TIMEOUT, async {
        for index in indices {
            let file = &mut planned[index];
            let output = format_source(root, step, name, file).await?;
            file.postimage_hash = hash(&output);
            file.postimage = output;
        }
        Ok(())
    })
    .await
    .context("formatter timed out after 30 seconds")?
}

fn formatter_name(program: &str) -> Result<&'static str> {
    match program.to_ascii_lowercase().as_str() {
        "rustfmt" | "rustfmt.exe" => Ok("rustfmt"),
        "gofmt" | "gofmt.exe" => Ok("gofmt"),
        "prettier" | "prettier.exe" => Ok("prettier"),
        // npm installs this launcher on Windows. Rust's process implementation
        // handles the command script and escapes its arguments.
        #[cfg(windows)]
        "prettier.cmd" => Ok("prettier"),
        _ => anyhow::bail!(
            "formatter program must be a bare executable name in the bounded allowlist"
        ),
    }
}

async fn format_source(
    root: &Path,
    step: &api::FormatterStep,
    name: &str,
    file: &PreparedFile,
) -> Result<Vec<u8>> {
    let mut command = tokio::process::Command::new(&step.program);
    command
        .current_dir(root)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // A formatter diagnostic can contain private source. Report its status
        // and the affected path without forwarding source to logs or MCP stdout.
        .stderr(Stdio::null());
    match name {
        "rustfmt" => {
            command.args(&step.arguments).args(["--emit", "stdout"]);
        }
        "prettier" => {
            command.args(
                step.arguments
                    .iter()
                    .filter(|argument| argument.as_str() != "--write"),
            );
            // Configuration can load executable plugins. The approved plan does
            // not authorize those plugins or their filesystem writes.
            command
                .args(["--no-config", "--no-editorconfig", "--stdin-filepath"])
                .arg(root.join(&file.path));
        }
        _ => {} // gofmt formats standard input when its write flag is removed.
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("start formatter for {}", file.path))?;
    let mut stdin = child
        .stdin
        .take()
        .context("formatter standard input is unavailable")?;
    let write = async {
        stdin.write_all(&file.postimage).await?;
        drop(stdin);
        Ok::<_, std::io::Error>(())
    };
    let ((), output) = tokio::try_join!(write, child.wait_with_output())
        .with_context(|| format!("format {}", file.path))?;
    ensure!(
        output.status.success(),
        "formatter for {} exited with {}",
        file.path,
        output.status
    );
    Ok(formatted_postimage(
        name,
        &step.arguments,
        &file.postimage,
        output.stdout,
    ))
}

fn formatted_postimage(
    program: &str,
    arguments: &[String],
    original: &[u8],
    output: Vec<u8>,
) -> Vec<u8> {
    // Prettier reports an ignored, unknown stdin format with success and empty
    // stdout. That skip signal must preserve the planned bytes.
    if program == "prettier"
        && arguments
            .iter()
            .any(|argument| argument == "--ignore-unknown")
        && output.is_empty()
    {
        original.to_vec()
    } else {
        output
    }
}

fn validate_arguments(program: &str, arguments: &[String]) -> Result<()> {
    let valid = match program {
        "rustfmt" => {
            matches!(arguments, [])
                || matches!(arguments, [flag, edition]
            if flag == "--edition" && matches!(edition.as_str(), "2015" | "2018" | "2021" | "2024"))
        }
        "gofmt" => matches!(arguments, [write] if write == "-w"),
        "prettier" => {
            matches!(arguments, [write] if write == "--write")
                || matches!(arguments, [write, unknown] if write == "--write" && unknown == "--ignore-unknown")
        }
        _ => false,
    };
    ensure!(
        valid,
        "formatter arguments are outside semctl's bounded allowlist"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatter_names_allow_only_supported_executables() {
        for (program, expected) in [
            ("rustfmt", "rustfmt"),
            ("rustfmt.exe", "rustfmt"),
            ("RUSTFMT.EXE", "rustfmt"),
            ("gofmt", "gofmt"),
            ("gofmt.exe", "gofmt"),
            ("prettier", "prettier"),
            ("prettier.exe", "prettier"),
        ] {
            assert_eq!(formatter_name(program).unwrap(), expected);
        }
        for program in [
            "rustfmt.sh",
            "rustfmt.exe.sh",
            "rustfmt.cmd",
            "gofmt.bat",
            "prettier.js",
            "prettier.ps1",
            "./rustfmt",
            "/usr/bin/rustfmt",
            r".\rustfmt.exe",
            r"C:\tools\prettier.cmd",
        ] {
            assert!(formatter_name(program).is_err(), "accepted {program}");
        }
        assert_eq!(formatter_name("prettier.cmd").is_ok(), cfg!(windows));
    }

    #[test]
    fn formatter_arguments_cannot_select_a_subcommand_or_plugin() {
        assert!(validate_arguments("rustfmt", &[]).is_ok());
        assert!(validate_arguments("gofmt", &["-w".into()]).is_ok());
        assert!(validate_arguments("prettier", &["--write".into()]).is_ok());
        assert!(validate_arguments("cargo", &["run".into()]).is_err());
        assert!(
            validate_arguments("prettier", &["--plugin".into(), "untrusted.js".into()]).is_err()
        );
    }

    #[test]
    fn prettier_skip_signal_preserves_unknown_input() {
        let original = b"retained content in an unsupported format\n";
        let result = formatted_postimage(
            "prettier",
            &["--write".into(), "--ignore-unknown".into()],
            original,
            Vec::new(),
        );
        assert_eq!(result, original);
    }

    #[test]
    fn prettier_nonempty_output_replaces_input_even_with_ignore_unknown() {
        let result = formatted_postimage(
            "prettier",
            &["--write".into(), "--ignore-unknown".into()],
            b"const value=1;\n",
            b"const value = 1;\n".to_vec(),
        );
        assert_eq!(result, b"const value = 1;\n");
    }

    #[tokio::test]
    async fn rustfmt_formats_only_the_planned_postimage() {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let target = root.join("lib.rs");
        let child = root.join("child.rs");
        let preimage = b"mod child;\npub fn original() {}\n";
        let child_source = b"pub fn child( ){let x=1;}\n";
        std::fs::write(&target, preimage).unwrap();
        std::fs::write(&child, child_source).unwrap();
        let (temporary, backup) = super::super::sidecars(&target, &"a".repeat(64), 0);
        let checkout = super::super::paths::Checkout::open(&root).unwrap();
        let location = super::super::paths::Target::bind(&checkout, &target).unwrap();
        let mut files = vec![PreparedFile {
            path: "lib.rs".into(),
            target,
            location,
            preimage: preimage.to_vec(),
            postimage: b"mod child;\npub fn renamed( ){ }\n".to_vec(),
            postimage_hash: String::new(),
            temporary,
            backup,
        }];
        let step = api::FormatterStep {
            program: "rustfmt".into(),
            arguments: vec![],
            paths: vec!["lib.rs".into()],
        };
        format(&root, &step, &mut files).await.unwrap();
        assert_eq!(files[0].postimage, b"mod child;\npub fn renamed() {}\n");
        assert_eq!(files[0].postimage_hash, hash(&files[0].postimage));
        assert_eq!(std::fs::read(&files[0].target).unwrap(), preimage);
        assert_eq!(std::fs::read(&child).unwrap(), child_source);
    }
}
