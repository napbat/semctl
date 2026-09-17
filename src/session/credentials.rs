//! Where one session's credentials come from.
//!
//! A session either carries its own access token or uses the stored login. The
//! choice is made once, at the invocation boundary, and then travels with the
//! session. No code below this module reads the process environment to decide
//! how a request is authorized, so one process can serve sessions that
//! authenticate differently.

use std::fmt;

/// The environment variable that carries a headless access token.
const TOKEN_VAR: &str = "SEMCTX_TOKEN";

/// A credential value that must not reach a log, an error message, or any other
/// output.
///
/// The type deliberately implements neither `Display` nor a revealing `Debug`.
/// [`Secret::expose`] is the only way to read the value, so every use of the
/// plain text is visible at the call site.
#[derive(Clone)]
pub(crate) struct Secret(String);

impl Secret {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    /// Read the plain credential. Call this at the point where the value is
    /// put on the wire, never to build a message.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(redacted)")
    }
}

/// How one session authorizes its requests.
#[derive(Clone, Debug)]
pub(crate) enum CredentialSource {
    /// An access token supplied with the invocation. It belongs to this session
    /// only. It never borrows a persisted refresh token and never changes
    /// stored login state.
    Invocation(Secret),
    /// The login held in the credential store. The store is read through its
    /// lock on every token fetch, so a login or logout performed while the
    /// process runs is honored by the next request.
    Stored,
}

impl CredentialSource {
    /// Read `SEMCTX_TOKEN` for one session.
    ///
    /// Only [`super::SessionContext::from_process`] calls this. An unset,
    /// empty, or whitespace-only value means the stored login.
    pub(crate) fn from_environment() -> Self {
        Self::from_token(std::env::var(TOKEN_VAR).ok().as_deref())
    }

    /// The pure mapping behind [`Self::from_environment`].
    fn from_token(value: Option<&str>) -> Self {
        match value.filter(|token| !token.trim().is_empty()) {
            Some(token) => Self::Invocation(Secret::new(token.to_string())),
            None => Self::Stored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CredentialSource, Secret};

    #[test]
    fn an_invocation_token_replaces_the_stored_login() {
        let source = CredentialSource::from_token(Some("header-token"));

        match source {
            CredentialSource::Invocation(secret) => assert_eq!(secret.expose(), "header-token"),
            CredentialSource::Stored => panic!("an explicit token must win over the stored login"),
        }
    }

    #[test]
    fn a_blank_or_absent_token_falls_back_to_the_stored_login() {
        for value in [None, Some(""), Some("   "), Some("\t\n")] {
            assert!(
                matches!(
                    CredentialSource::from_token(value),
                    CredentialSource::Stored
                ),
                "{value:?} must not be treated as a credential"
            );
        }
    }

    /// A token must not be recoverable from a `{:?}` in a log line or an error.
    #[test]
    fn a_secret_is_redacted_in_debug_output() {
        let secret = Secret::new("super-secret-token".to_string());

        assert_eq!(format!("{secret:?}"), "Secret(redacted)");
        assert!(!format!("{:?}", CredentialSource::Invocation(secret)).contains("super-secret"));
    }
}
