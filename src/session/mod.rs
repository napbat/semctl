//! The invocation context of one session.
//!
//! [`SessionContext`] is the only way per-session invocation state enters the
//! rest of the program. Everything that used to be read from the process — the
//! working directory, `SEMCTX_TOKEN`, `SEMCTX_MCP_RESYNC_SECS`, and
//! `SEMCTX_MCP_UPDATE_CHECK` — is read once, here, and then passed explicitly.
//!
//! That separation is what lets one process serve several sessions. A process
//! has one environment and one working directory; a session must not be
//! described by them.

pub(crate) mod credentials;

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};

use crate::cli::Cli;
use crate::ipc::handshake::{SessionRequest, Token};

pub(crate) use credentials::{CredentialScope, CredentialSource};

/// Seconds between periodic re-syncs. Unset means the built-in default.
const RESYNC_SECS_VAR: &str = "SEMCTX_MCP_RESYNC_SECS";
/// `0` turns the startup update check off. Any other value leaves it on.
const UPDATE_CHECK_VAR: &str = "SEMCTX_MCP_UPDATE_CHECK";

/// Every environment variable that describes one session.
///
/// A daemon serves several sessions, so none of these may describe the daemon
/// process itself. The client removes them from the environment of a daemon it
/// spawns, and a daemon that still finds one warns that it is ignored.
///
/// `SEMCTX_SERVER`, `SEMCTX_TENANT`, and `SEMCTX_CODEBASE` are declared as
/// clap fallbacks on the global flags in [`crate::cli`]; the other two are read
/// in this module.
pub(crate) const PER_SESSION_VARS: [&str; 6] = [
    credentials::TOKEN_VAR,
    "SEMCTX_SERVER",
    "SEMCTX_TENANT",
    "SEMCTX_CODEBASE",
    RESYNC_SECS_VAR,
    UPDATE_CHECK_VAR,
];

/// Validated invocation context for one session.
///
/// A standalone process builds it with [`SessionContext::from_process`]. A
/// daemon builds one per connection with [`SessionContext::from_handshake`].
#[derive(Debug)]
pub(crate) struct SessionContext {
    /// The directory the caller invoked from. Absolute. Relative selectors
    /// resolve against this field, never against the process working
    /// directory.
    pub(crate) cwd: PathBuf,
    /// Server base URL override (`--server` / `SEMCTX_SERVER`). The config file
    /// default applies when this is `None`.
    pub(crate) server: Option<String>,
    /// Active tenant override (`--tenant` / `SEMCTX_TENANT`).
    pub(crate) tenant: Option<String>,
    /// Codebase pinned up front (`--codebase` / `SEMCTX_CODEBASE`).
    pub(crate) codebase: Option<String>,
    /// How this session authorizes its requests.
    pub(crate) credentials: CredentialSource,
    /// Periodic re-sync interval in seconds. `None` means the indexing default.
    pub(crate) resync_secs: Option<u64>,
    /// Whether this session wants the startup update check.
    pub(crate) update_check: bool,
}

/// One reading of the per-session environment variables.
///
/// Keeping the raw strings here lets [`SessionContext::build`] stay pure, so
/// the mapping and the parsing are testable without touching the real process.
struct Environment {
    credentials: CredentialSource,
    resync_secs: Option<String>,
    update_check: Option<String>,
}

impl SessionContext {
    /// Build the context of a session that runs in this process.
    ///
    /// This is the single place that reads the working directory and the
    /// per-session environment variables. It fails when the working directory
    /// cannot be read, because a session with no directory cannot resolve a
    /// relative selector or a launch codebase.
    pub(crate) fn from_process(cli: &Cli) -> Result<Self> {
        // `current_dir` returns an absolute path, which is the `cwd` invariant.
        let cwd = std::env::current_dir().context("read working directory")?;
        let environment = Environment {
            credentials: CredentialSource::from_environment(),
            resync_secs: std::env::var(RESYNC_SECS_VAR).ok(),
            update_check: std::env::var(UPDATE_CHECK_VAR).ok(),
        };
        Ok(Self::build(cwd, cli, environment))
    }

    /// Build the context of a session that runs in another process.
    ///
    /// This is the daemon side of the attach handshake, and the only other way
    /// a context exists. The body is external data, so it is validated here:
    /// `cwd` must be absolute, because every relative selector resolves
    /// against it and the daemon never falls back to its own working
    /// directory.
    pub(crate) fn from_handshake(request: SessionRequest) -> Result<Self> {
        ensure!(
            request.cwd.is_absolute(),
            "session working directory {} is not absolute",
            request.cwd.display()
        );
        Ok(Self {
            cwd: request.cwd,
            server: request.server,
            tenant: request.tenant,
            codebase: request.codebase,
            // One rule decides what counts as a credential, whether the token
            // came from this process or from an attach body.
            credentials: CredentialSource::from_token(request.token.as_ref().map(Token::expose)),
            resync_secs: request.resync_secs,
            update_check: request.update_check,
        })
    }

    /// A context for tests that never act on its fields. The working directory
    /// is a path no test creates, so a test that starts reading the filesystem
    /// through it fails instead of finding the process working directory.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            cwd: PathBuf::from("/semctl-test-session"),
            server: None,
            tenant: None,
            codebase: None,
            credentials: CredentialSource::Stored,
            resync_secs: None,
            update_check: false,
        }
    }

    /// The pure mapping from one working directory, the command-line
    /// overrides, and one environment snapshot to a context.
    ///
    /// `cwd` must be absolute. The callers that obtain it — the process reader
    /// above, and later the attach handshake — own that check.
    fn build(cwd: PathBuf, cli: &Cli, environment: Environment) -> Self {
        Self {
            cwd,
            // clap already folded `SEMCTX_SERVER`, `SEMCTX_TENANT`, and
            // `SEMCTX_CODEBASE` into these fields.
            server: cli.server.clone(),
            tenant: cli.tenant.clone(),
            codebase: cli.codebase.clone(),
            credentials: environment.credentials,
            resync_secs: parse_resync_secs(environment.resync_secs.as_deref()),
            update_check: update_check_enabled(environment.update_check.as_deref()),
        }
    }
}

/// Parse the re-sync interval. An absent or unreadable value means "use the
/// indexing default", which keeps a typo from disabling the drift backstop.
fn parse_resync_secs(raw: Option<&str>) -> Option<u64> {
    raw.and_then(|value| value.trim().parse().ok())
}

/// Only the exact value `0` turns the update check off.
fn update_check_enabled(raw: Option<&str>) -> bool {
    raw != Some("0")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        CredentialSource, Environment, PER_SESSION_VARS, SessionContext, SessionRequest, Token,
        parse_resync_secs, update_check_enabled,
    };
    use crate::cli::{Cli, Command};

    fn cli(server: Option<&str>, tenant: Option<&str>, codebase: Option<&str>) -> Cli {
        Cli {
            server: server.map(str::to_string),
            tenant: tenant.map(str::to_string),
            codebase: codebase.map(str::to_string),
            command: Command::Mcp,
        }
    }

    fn environment(resync_secs: Option<&str>, update_check: Option<&str>) -> Environment {
        Environment {
            credentials: CredentialSource::Stored,
            resync_secs: resync_secs.map(str::to_string),
            update_check: update_check.map(str::to_string),
        }
    }

    #[test]
    fn command_line_overrides_become_session_fields() {
        let context = SessionContext::build(
            PathBuf::from("/work/checkout"),
            &cli(Some("https://example.invalid"), Some("acme"), Some("id")),
            environment(Some("15"), Some("1")),
        );

        assert_eq!(context.cwd, PathBuf::from("/work/checkout"));
        assert_eq!(context.server.as_deref(), Some("https://example.invalid"));
        assert_eq!(context.tenant.as_deref(), Some("acme"));
        assert_eq!(context.codebase.as_deref(), Some("id"));
        assert_eq!(context.resync_secs, Some(15));
        assert!(context.update_check);
        assert!(matches!(context.credentials, CredentialSource::Stored));
    }

    /// Nothing set anywhere must leave every optional field unset and keep the
    /// update check on.
    #[test]
    fn an_empty_invocation_leaves_every_override_unset() {
        let context = SessionContext::build(
            PathBuf::from("/work"),
            &cli(None, None, None),
            environment(None, None),
        );

        assert_eq!(context.server, None);
        assert_eq!(context.tenant, None);
        assert_eq!(context.codebase, None);
        assert_eq!(context.resync_secs, None);
        assert!(context.update_check);
    }

    #[test]
    fn resync_seconds_are_parsed_and_unreadable_values_keep_the_default() {
        assert_eq!(parse_resync_secs(Some("30")), Some(30));
        assert_eq!(parse_resync_secs(Some("  30\n")), Some(30));
        assert_eq!(
            parse_resync_secs(Some("0")),
            Some(0),
            "zero is a real setting: it disables the periodic re-sync"
        );
        assert_eq!(parse_resync_secs(None), None);
        assert_eq!(parse_resync_secs(Some("")), None);
        assert_eq!(parse_resync_secs(Some("soon")), None);
        assert_eq!(parse_resync_secs(Some("-5")), None);
    }

    fn request(cwd: &str, token: Option<&str>) -> SessionRequest {
        SessionRequest {
            cwd: PathBuf::from(cwd),
            server: Some("https://example.invalid".to_string()),
            tenant: Some("acme".to_string()),
            codebase: Some("id".to_string()),
            token: token.map(Token::new),
            resync_secs: Some(15),
            update_check: false,
        }
    }

    #[test]
    fn an_attach_body_becomes_the_session_context() {
        let context = SessionContext::from_handshake(request("/work/checkout", Some("wire-token")))
            .expect("an absolute working directory is accepted");

        assert_eq!(context.cwd, PathBuf::from("/work/checkout"));
        assert_eq!(context.server.as_deref(), Some("https://example.invalid"));
        assert_eq!(context.tenant.as_deref(), Some("acme"));
        assert_eq!(context.codebase.as_deref(), Some("id"));
        assert_eq!(context.resync_secs, Some(15));
        assert!(!context.update_check);
        match context.credentials {
            CredentialSource::Invocation(secret) => assert_eq!(secret.expose(), "wire-token"),
            CredentialSource::Stored => panic!("the attach token must authorize this session"),
        }
    }

    /// The daemon resolves every relative selector against this field and must
    /// never fall back to its own working directory.
    #[test]
    fn a_relative_working_directory_is_refused() {
        let error = SessionContext::from_handshake(request("relative/path", None))
            .expect_err("a relative working directory cannot resolve a selector");

        assert!(error.to_string().contains("is not absolute"), "{error:#}");
    }

    #[test]
    fn an_absent_or_blank_attach_token_means_the_stored_login() {
        for token in [None, Some(""), Some("   ")] {
            let context = SessionContext::from_handshake(request("/work", token))
                .expect("an absolute working directory is accepted");
            assert!(
                matches!(context.credentials, CredentialSource::Stored),
                "{token:?} must not be treated as a credential"
            );
        }
    }

    /// The list is what a client strips from a daemon's environment. A missing
    /// entry would let the daemon describe every session with its own value.
    #[test]
    fn the_per_session_variables_are_the_documented_six() {
        assert_eq!(
            PER_SESSION_VARS,
            [
                "SEMCTX_TOKEN",
                "SEMCTX_SERVER",
                "SEMCTX_TENANT",
                "SEMCTX_CODEBASE",
                "SEMCTX_MCP_RESYNC_SECS",
                "SEMCTX_MCP_UPDATE_CHECK",
            ]
        );
    }

    #[test]
    fn only_zero_disables_the_update_check() {
        assert!(!update_check_enabled(Some("0")));
        assert!(update_check_enabled(None));
        assert!(update_check_enabled(Some("1")));
        assert!(update_check_enabled(Some("")));
        assert!(update_check_enabled(Some(" 0 ")));
    }
}
