//! Decide whether a `PreToolUse` call is a "built-in search" the nudge should
//! react to, and of what kind: the first-class `Grep` / `Glob` tools, or a
//! `Bash` / `PowerShell` command that shells out to a search binary.
//!
//! The **kind** (content vs filename) drives the message: nudging a filename
//! enumeration (`Glob`, `find`, `fd`, `Get-ChildItem -Recurse`) toward a content
//! search tool like `grep` is actively wrong advice, so those route to the
//! file-listing tools instead.
//!
//! Best-effort, v1. The command sniffers peel the common wrappers (env prefixes,
//! `git grep`, `xargs`, `find -exec`, absolute paths, a case-insensitive `.exe`)
//! but do **not** parse the shell — commands hidden in subshells / command
//! substitution, behind aliases/functions, or a search binary named only inside
//! a quoted string are out of scope (and may false-positive on the last). Those
//! misses are asserted in the tests so the boundary is explicit, not silent.

use serde_json::Value;

use super::message::SearchKind;

/// The tool names `eligible_search` handles. The `hooks.json` `PreToolUse` matcher
/// must list exactly these — a drift guard in `hook.rs` asserts the two stay in
/// lockstep so adding a tool here (or there) can't silently half-wire the nudge.
pub const HANDLED_TOOLS: &[&str] = &["Grep", "Glob", "Bash", "PowerShell"];

/// An eligible built-in search, with its kind and a best-effort pattern for
/// message tailoring (`Grep`/`Glob` expose it directly; shell commands don't).
/// Borrows from the `tool_input` it was read out of — no allocation.
pub struct SearchCall<'a> {
    pub kind: SearchKind,
    pub pattern: Option<&'a str>,
}

/// `Some` when `tool_name` + `tool_input` is a search we should consider
/// nudging on, `None` otherwise. Pure and network-free — safe to run on every
/// `PreToolUse` before any state or availability work.
pub fn eligible_search<'a>(tool_name: &str, tool_input: &'a Value) -> Option<SearchCall<'a>> {
    // Gate on the advertised set first, so HANDLED_TOOLS is the single source of
    // truth the hooks.json matcher is checked against (and can't silently drift
    // from the arms below).
    if !HANDLED_TOOLS.contains(&tool_name) {
        return None;
    }
    match tool_name {
        // Grep/Glob require a non-empty pattern; a malformed call with no pattern
        // is not a real search and must not count toward escalation.
        "Grep" => str_field(tool_input, "pattern")
            .filter(|p| !p.is_empty())
            .map(|p| SearchCall {
                kind: SearchKind::Content,
                pattern: Some(p),
            }),
        "Glob" => str_field(tool_input, "pattern")
            .filter(|p| !p.is_empty())
            .map(|p| SearchCall {
                kind: SearchKind::Filename,
                pattern: Some(p),
            }),
        "Bash" => {
            let cmd = str_field(tool_input, "command")?;
            // Codex names its shell tool `Bash` on every OS, but on Windows runs
            // commands through PowerShell — so a PowerShell-idiom search
            // (`Select-String`, `findstr`, `Get-ChildItem -Recurse`) arrives here,
            // not under `PowerShell`. Try the bash heuristic first, then fall back
            // to the PowerShell one so those don't slip past. Additive: real Unix
            // bash searches match first, and cross-platform names (`grep`/`rg`)
            // are already caught on the bash pass.
            bash_search_kind(cmd)
                .or_else(|| powershell_search_kind(cmd))
                .map(|kind| SearchCall {
                    kind,
                    pattern: None,
                })
        }
        "PowerShell" => {
            let cmd = str_field(tool_input, "command")?;
            powershell_search_kind(cmd).map(|kind| SearchCall {
                kind,
                pattern: None,
            })
        }
        _ => None,
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Content-search binaries an agent shells out to in Bash.
const CONTENT_BINS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ripgrep", "ag", "ack"];
/// Filename-enumeration binaries.
const FILENAME_BINS: &[&str] = &["find", "fd", "fdfind"];

/// The kind of Bash search, if any. Content wins over filename: `find … | grep …`
/// and `find … -exec grep …` are both really content searches.
fn bash_search_kind(command: &str) -> Option<SearchKind> {
    let mut saw_filename = false;
    for seg in split_pipeline(command) {
        if let Some(cmd) = effective_command(seg) {
            if CONTENT_BINS.contains(&cmd) {
                return Some(SearchKind::Content);
            }
            if FILENAME_BINS.contains(&cmd) {
                // `find … -exec grep …` / `-execdir` runs a content search.
                if find_exec_runs_content(seg) {
                    return Some(SearchKind::Content);
                }
                saw_filename = true;
            }
        }
    }
    saw_filename.then_some(SearchKind::Filename)
}

/// Whether a `find`/`fd` segment hands off to a content-search binary via
/// `-exec`/`-execdir` (`find . -name '*.rs' -exec grep foo {} +`).
fn find_exec_runs_content(segment: &str) -> bool {
    let mut tokens = segment.split_whitespace();
    while let Some(tok) = tokens.next() {
        if (tok == "-exec" || tok == "-execdir")
            && tokens
                .next()
                .map(basename)
                .is_some_and(|b| CONTENT_BINS.contains(&b))
        {
            return true;
        }
    }
    false
}

/// The effective command word of one pipeline segment, after peeling leading
/// `VAR=val` / `env`, simple wrappers (`sudo`, `xargs`, …), and `git [opts]`
/// so `git grep` reports `grep`. Borrows from `segment` (no allocation).
fn effective_command(segment: &str) -> Option<&str> {
    let mut tokens = segment.split_whitespace().peekable();

    // Leading environment: `env`, `VAR=value`.
    while let Some(tok) = tokens.peek() {
        if *tok == "env" || is_env_assignment(tok) {
            tokens.next();
        } else {
            break;
        }
    }

    // Simple command wrappers. `xargs` / `time` (and friends) are followed by
    // the real command, possibly after their own flags.
    loop {
        match tokens.peek().copied() {
            Some("sudo" | "command" | "nice" | "nohup" | "time" | "stdbuf" | "setsid") => {
                tokens.next();
            }
            Some("xargs") => {
                tokens.next();
                while tokens.peek().is_some_and(|t| t.starts_with('-')) {
                    // Drop xargs flags; `-I{}` / `-n1` carry their arg inline.
                    tokens.next();
                }
            }
            _ => break,
        }
    }

    // `git [global opts] <subcommand>` — report the subcommand (catches `git grep`).
    if tokens.peek().map(|t| basename(t)) == Some("git") {
        tokens.next();
        while let Some(tok) = tokens.peek().copied() {
            if tok == "-C" || tok == "-c" {
                tokens.next(); // option
                tokens.next(); // its argument
            } else if tok.starts_with('-') {
                tokens.next();
            } else {
                break;
            }
        }
        return tokens.next().map(basename);
    }

    tokens.next().map(basename)
}

fn is_env_assignment(tok: &str) -> bool {
    match tok.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Last path component, cross-platform (`/usr/bin/grep` / `C:\tools\rg.exe`),
/// with a trailing `.exe` stripped case-insensitively (`rg.exe`→`rg`). Casing of
/// the *name* is preserved: the Bash sniffer compares case-sensitively (Unix),
/// while the PowerShell sniffer lowercases the result, so an all-caps `RG.EXE`
/// resolves only on the PowerShell path.
fn basename(tok: &str) -> &str {
    let base = tok.rsplit(['/', '\\']).next().unwrap_or(tok);
    match base.rfind('.') {
        Some(dot) if base[dot..].eq_ignore_ascii_case(".exe") => &base[..dot],
        _ => base,
    }
}

/// Split on the pipeline / sequence operators `| || && ;`. Naive char split —
/// quoting is not honored (an accepted v1 limitation).
fn split_pipeline(command: &str) -> Vec<&str> {
    command
        .split(['|', '&', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// The kind of PowerShell search, if any. `Get-ChildItem` (and its aliases)
/// only count when recursing — a plain listing is not drift. Content wins over
/// filename when both appear.
fn powershell_search_kind(command: &str) -> Option<SearchKind> {
    let mut saw_filename = false;
    for seg in split_pipeline(command) {
        let Some(first) = seg.split_whitespace().next() else {
            continue;
        };
        match basename(first).to_ascii_lowercase().as_str() {
            "select-string" | "sls" | "findstr" | "rg" | "ripgrep" | "grep" | "egrep" | "fgrep" => {
                return Some(SearchKind::Content);
            }
            "get-childitem" | "gci" | "dir" | "ls" if has_recurse_flag(seg) => {
                saw_filename = true;
            }
            _ => {}
        }
    }
    saw_filename.then_some(SearchKind::Filename)
}

fn has_recurse_flag(segment: &str) -> bool {
    segment.split_whitespace().any(|t| {
        let t = t.to_ascii_lowercase();
        t == "-r" || t.starts_with("-rec") // -r, -Recurse (and unambiguous prefixes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kind(tool: &str, input: &serde_json::Value) -> Option<SearchKind> {
        eligible_search(tool, input).map(|c| c.kind)
    }
    fn bash(cmd: &str) -> Option<SearchKind> {
        kind("Bash", &json!({ "command": cmd }))
    }
    fn pwsh(cmd: &str) -> Option<SearchKind> {
        kind("PowerShell", &json!({ "command": cmd }))
    }

    #[test]
    fn first_class_tools_and_their_kinds() {
        assert_eq!(
            kind("Grep", &json!({ "pattern": "foo" })),
            Some(SearchKind::Content)
        );
        assert_eq!(
            kind("Glob", &json!({ "pattern": "**/*.rs" })),
            Some(SearchKind::Filename)
        );
        assert_eq!(kind("Read", &json!({ "file_path": "a" })), None);
        assert_eq!(kind("Edit", &json!({})), None);
    }

    #[test]
    fn grep_glob_without_a_pattern_are_not_eligible() {
        // A malformed first-class search with no/empty pattern must not count.
        assert_eq!(kind("Grep", &json!({})), None);
        assert_eq!(kind("Grep", &json!({ "pattern": "" })), None);
        assert_eq!(kind("Glob", &json!({})), None);
    }

    #[test]
    fn every_handled_tool_is_actually_recognized() {
        // Keeps HANDLED_TOOLS honest against eligible_search's arms (and thus
        // against the hooks.json matcher the wiring guard checks).
        assert!(eligible_search("Grep", &json!({ "pattern": "x" })).is_some());
        assert!(eligible_search("Glob", &json!({ "pattern": "*.rs" })).is_some());
        assert!(eligible_search("Bash", &json!({ "command": "grep x" })).is_some());
        assert!(eligible_search("PowerShell", &json!({ "command": "sls x" })).is_some());
        assert_eq!(HANDLED_TOOLS, &["Grep", "Glob", "Bash", "PowerShell"]);
    }

    #[test]
    fn grep_pattern_is_captured_for_tailoring() {
        let input = json!({ "pattern": "parse_config" });
        let call = eligible_search("Grep", &input).unwrap();
        assert_eq!(call.pattern, Some("parse_config"));
    }

    #[test]
    fn bash_search_commands_and_kinds() {
        assert_eq!(bash("grep -r foo ."), Some(SearchKind::Content));
        assert_eq!(bash("rg foo"), Some(SearchKind::Content));
        assert_eq!(bash("cat a.txt | grep b"), Some(SearchKind::Content)); // mid-pipeline
        assert_eq!(bash("git grep foo"), Some(SearchKind::Content));
        assert_eq!(bash("git -C some/repo grep foo"), Some(SearchKind::Content));
        assert_eq!(bash("FOO=1 BAR=2 rg needle"), Some(SearchKind::Content)); // env prefix
        assert_eq!(bash("env rg needle"), Some(SearchKind::Content));
        assert_eq!(bash("xargs grep foo"), Some(SearchKind::Content)); // xargs wrapper
        assert_eq!(bash("/usr/bin/grep foo"), Some(SearchKind::Content)); // absolute path
        assert_eq!(bash("rg.exe foo"), Some(SearchKind::Content)); // .exe stripped
        // The bash heuristic is case-sensitive (Unix): `RG.EXE`→`RG` ≠ `rg`. But
        // the PowerShell fallback (Codex runs PowerShell under its `Bash` tool on
        // Windows) lowercases, so an all-caps `RG.EXE` is still caught as a search.
        assert_eq!(bash("RG.EXE foo"), Some(SearchKind::Content));
        // filename enumeration
        assert_eq!(bash("find . -name '*.rs'"), Some(SearchKind::Filename));
        assert_eq!(bash("fd -e rs"), Some(SearchKind::Filename));
        // both present → content wins
        assert_eq!(
            bash("find . -name '*.rs' | grep foo"),
            Some(SearchKind::Content)
        );
        // find -exec/-execdir handing off to a content bin → content
        assert_eq!(
            bash("find . -name '*.rs' -exec grep -n foo {} +"),
            Some(SearchKind::Content)
        );
        assert_eq!(
            bash("find . -execdir rg foo {} ;"),
            Some(SearchKind::Content)
        );
        // find -exec running a non-search stays filename enumeration
        assert_eq!(
            bash("find . -name '*.tmp' -exec rm {} +"),
            Some(SearchKind::Filename)
        );
    }

    #[test]
    fn bash_non_search_commands_are_not_eligible() {
        for cmd in [
            "ls -la",
            "cargo build",
            "git status",
            "git commit -m x",
            "npm test",
            "cat file.txt",
            "echo grepping is fun",
        ] {
            assert_eq!(bash(cmd), None, "{cmd}");
        }
    }

    // Codex's shell tool is always `Bash`, but on Windows it runs PowerShell, so
    // PowerShell-idiom searches arrive under the `Bash` tool and must still nudge
    // (the Bash arm falls back to the PowerShell heuristic).
    #[test]
    fn codex_windows_powershell_search_under_bash_tool() {
        assert_eq!(bash("Select-String foo *.rs"), Some(SearchKind::Content));
        assert_eq!(bash("findstr foo file.txt"), Some(SearchKind::Content));
        assert_eq!(bash("sls needle"), Some(SearchKind::Content));
        assert_eq!(
            bash("Get-ChildItem -Recurse -Filter *.rs"),
            Some(SearchKind::Filename)
        );
        // A real Unix bash search still matches via the bash heuristic first.
        assert_eq!(bash("rg foo"), Some(SearchKind::Content));
        // The fallback stays conservative: non-search bash commands don't fire,
        // even when a PowerShell tool name appears only inside a string.
        assert_eq!(bash("cargo build"), None);
        assert_eq!(bash("echo select-string is a tool"), None);
    }

    #[test]
    fn powershell_search_commands_and_kinds() {
        assert_eq!(pwsh("Select-String foo *.rs"), Some(SearchKind::Content));
        assert_eq!(pwsh("sls foo"), Some(SearchKind::Content));
        assert_eq!(pwsh("findstr foo file.txt"), Some(SearchKind::Content));
        assert_eq!(pwsh("rg foo"), Some(SearchKind::Content));
        assert_eq!(
            pwsh("Get-Content x | Select-String foo"),
            Some(SearchKind::Content)
        );
        // PowerShell lowercases, so a Windows `RG.EXE` invocation is recognized.
        assert_eq!(pwsh("RG.EXE foo"), Some(SearchKind::Content));
        assert_eq!(
            pwsh("Get-ChildItem -Recurse -Filter *.rs"),
            Some(SearchKind::Filename)
        );
        assert_eq!(pwsh("gci -r"), Some(SearchKind::Filename));
    }

    #[test]
    fn powershell_non_search_commands_are_not_eligible() {
        for cmd in [
            "Get-ChildItem",
            "gci",
            "Get-Content file.txt",
            "Write-Output hi",
        ] {
            assert_eq!(pwsh(cmd), None, "{cmd}");
        }
    }

    #[test]
    fn documented_v1_misses() {
        // We do not parse the shell, so these known cases are missed. Asserted
        // so the boundary stays explicit rather than becoming an accidental
        // regression if the sniffer is later "improved".
        assert_eq!(bash("echo $(grep foo bar)"), None); // command substitution
        assert_eq!(bash("(grep foo bar)"), None); // subshell — leading token is `(grep`
    }
}
