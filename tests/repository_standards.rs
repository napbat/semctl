use std::fs;
use std::path::{Path, PathBuf};

const MAX_RUST_LINES: usize = 1_000;
const SOURCE_ROOTS: &[&str] = &["src", "tests", "examples", "benches"];

fn collect_rust_files(path: &Path, files: &mut Vec<PathBuf>) {
    if !path.exists() {
        return;
    }
    if path.is_file() {
        if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path.to_path_buf());
        }
        return;
    }

    let entries = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    for entry in entries {
        let entry = entry
            .unwrap_or_else(|error| panic!("cannot read an entry in {}: {error}", path.display()));
        collect_rust_files(&entry.path(), files);
    }
}

fn rust_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for source_root in SOURCE_ROOTS {
        collect_rust_files(&root.join(source_root), &mut files);
    }
    let build_script = root.join("build.rs");
    if build_script.exists() {
        files.push(build_script);
    }
    files.sort();
    files
}

fn read_source(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn relative_path(path: &Path) -> String {
    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .display()
        .to_string()
}

fn is_generated(source: &str) -> bool {
    source.lines().take(20).any(|line| {
        line.contains("@generated")
            || line.contains("DO NOT EDIT")
            || line.contains("Code generated")
    })
}

fn module_name(declaration: &str) -> Option<&str> {
    let rest = declaration.trim_start().strip_prefix("mod ")?;
    let length = rest
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(rest.len());
    (length > 0).then(|| &rest[..length])
}

fn test_module_names(source: &str) -> Vec<&str> {
    let lines: Vec<&str> = source.lines().collect();
    let mut names = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let Some(after_attribute) = trimmed.strip_prefix("#[cfg(test)]") else {
            continue;
        };
        let inline = after_attribute.trim();
        let declaration = if inline.is_empty() {
            lines[index + 1..]
                .iter()
                .map(|candidate| candidate.trim())
                .find(|candidate| {
                    !candidate.is_empty()
                        && !candidate.starts_with("//")
                        && !candidate.starts_with("#[")
                })
        } else {
            Some(inline)
        };
        if let Some(name) = declaration.and_then(module_name) {
            names.push(name);
        }
    }
    names
}

#[test]
fn handwritten_rust_files_do_not_exceed_the_size_limit() {
    let mut violations = Vec::new();
    for path in rust_files() {
        let source = read_source(&path);
        if is_generated(&source) {
            continue;
        }
        let lines = source.lines().count();
        if lines > MAX_RUST_LINES {
            violations.push(format!(
                "{} has {lines} lines; split it at a cohesive boundary",
                relative_path(&path)
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "Rust files must stay at or below {MAX_RUST_LINES} lines:\n{}",
        violations.join("\n")
    );
}

#[test]
fn unit_test_modules_use_the_standard_name() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    for path in rust_files() {
        let source = read_source(&path);
        if is_generated(&source) {
            continue;
        }
        let relative = relative_path(&path);
        let modules = test_module_names(&source);
        if modules.len() > 1 {
            violations.push(format!("{relative} declares more than one test module"));
        }
        for module in modules {
            if module != "tests" {
                violations.push(format!(
                    "{relative} declares `mod {module}`; use `mod tests`"
                ));
            }
        }
        if path.starts_with(&source_root)
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_tests.rs"))
        {
            violations.push(format!(
                "{relative} is an external unit-test module; name it `tests.rs`"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "Unit tests must follow the Cargo module convention:\n{}",
        violations.join("\n")
    );
}
