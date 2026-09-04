//! Scope classification for built-in filesystem and shell searches.

use std::path::PathBuf;

/// Observable scope of an eligible search. The hook cannot know the provider's
/// active context, but it can distinguish a current single-file operation from
/// broad repository discovery without guessing about model state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchScope {
    BroadOrUnknown,
    SingleFile,
    OutsideRepo,
}

/// Resolve tool paths against the repository. Missing/non-existent/ambiguous
/// paths stay broad so a malformed or complex command never gains a false
/// single-file exemption.
pub(super) fn search_scope(
    tool_name: &str,
    tool_input: &serde_json::Value,
    cwd: &str,
) -> SearchScope {
    let Some(root) = repo_root(cwd) else {
        return SearchScope::BroadOrUnknown;
    };
    let cwd = std::path::Path::new(cwd);

    if let Some(path) = tool_input.get("path").and_then(|v| v.as_str()) {
        let scope = path_scope(std::path::Path::new(path), cwd, &root);
        if scope != SearchScope::BroadOrUnknown {
            return scope;
        }
    }

    // Glob's `path` is normally a directory; an exact non-glob pattern can still
    // identify one file beneath it. Wildcards remain repository/directory work.
    if tool_name == "Glob"
        && let Some(pattern) = tool_input.get("pattern").and_then(|v| v.as_str())
        && !has_glob_meta(pattern)
    {
        let base = tool_input
            .get("path")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from)
            .map_or_else(
                || cwd.to_path_buf(),
                |path| {
                    if path.is_absolute() {
                        path
                    } else {
                        cwd.join(path)
                    }
                },
            );
        let pattern = std::path::Path::new(pattern);
        let candidate = if pattern.is_absolute() {
            pattern.to_path_buf()
        } else {
            base.join(pattern)
        };
        let scope = resolved_path_scope(&candidate, &root);
        if scope != SearchScope::BroadOrUnknown {
            return scope;
        }
    }

    tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .map_or(SearchScope::BroadOrUnknown, |command| {
            shell_search_scope(command, cwd, &root)
        })
}

fn has_glob_meta(path: &str) -> bool {
    path.contains(['*', '?', '[', ']', '{', '}'])
}

fn path_scope(
    path: &std::path::Path,
    cwd: &std::path::Path,
    root: &std::path::Path,
) -> SearchScope {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    resolved_path_scope(&candidate, root)
}

fn resolved_path_scope(path: &std::path::Path, root: &std::path::Path) -> SearchScope {
    let (Ok(path), Ok(root)) = (std::fs::canonicalize(path), std::fs::canonicalize(root)) else {
        return SearchScope::BroadOrUnknown;
    };
    if !path.starts_with(root) {
        SearchScope::OutsideRepo
    } else if path.is_file() {
        SearchScope::SingleFile
    } else {
        SearchScope::BroadOrUnknown
    }
}

/// Recognize simple shell forms whose path operand is unambiguous:
/// `rg pattern file`, `grep pattern file`, `find path`, or `fd pattern path`.
/// A small no-argument flag allowlist is safe; unknown options, pipelines,
/// sequences, multiple positional targets, and missing paths remain broad.
fn shell_search_scope(command: &str, cwd: &std::path::Path, root: &std::path::Path) -> SearchScope {
    if command.contains(['|', '&', ';']) {
        return SearchScope::BroadOrUnknown;
    }
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let Some(command_index) = tokens.iter().position(|token| is_shell_search_bin(token)) else {
        return SearchScope::BroadOrUnknown;
    };
    let command_name = shell_basename(tokens[command_index]).to_ascii_lowercase();
    let args = &tokens[command_index + 1..];
    let target = if command_name == "find" {
        args.first().copied().filter(|arg| !arg.starts_with('-'))
    } else {
        if args
            .iter()
            .any(|arg| arg.starts_with('-') && !is_no_arg_search_flag(arg))
        {
            return SearchScope::BroadOrUnknown;
        }
        let mut positional = args.iter().copied().filter(|arg| !arg.starts_with('-'));
        match (positional.next(), positional.next(), positional.next()) {
            (Some(_pattern), Some(target), None) => Some(target),
            _ => None,
        }
    };
    let Some(target) = target else {
        return SearchScope::BroadOrUnknown;
    };
    let target = target.trim_matches(|c| c == '"' || c == '\'');
    if target.is_empty() || has_glob_meta(target) {
        return SearchScope::BroadOrUnknown;
    }
    path_scope(std::path::Path::new(target), cwd, root)
}

fn is_no_arg_search_flag(arg: &str) -> bool {
    matches!(
        arg,
        "-n" | "--line-number"
            | "-i"
            | "--ignore-case"
            | "-F"
            | "--fixed-strings"
            | "-w"
            | "--word-regexp"
    )
}

fn is_shell_search_bin(token: &str) -> bool {
    matches!(
        shell_basename(token).to_ascii_lowercase().as_str(),
        "grep"
            | "egrep"
            | "fgrep"
            | "rg"
            | "ripgrep"
            | "ag"
            | "ack"
            | "find"
            | "fd"
            | "fdfind"
            | "select-string"
            | "sls"
            | "findstr"
    )
}

fn shell_basename(token: &str) -> &str {
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
    match base.rfind('.') {
        Some(dot) if base[dot..].eq_ignore_ascii_case(".exe") => &base[..dot],
        _ => base,
    }
}

/// The repo root for `cwd`: the nearest ancestor containing a `.git` entry (a
/// dir, or a file for worktrees/submodules), else `cwd` itself. `None` for an
/// empty cwd.
fn repo_root(cwd: &str) -> Option<PathBuf> {
    if cwd.is_empty() {
        return None;
    }
    let start = PathBuf::from(cwd);
    let mut cur: &std::path::Path = &start;
    loop {
        if cur.join(".git").exists() {
            return Some(cur.to_path_buf());
        }
        let Some(p) = cur.parent() else { break };
        cur = p;
    }
    Some(start)
}
