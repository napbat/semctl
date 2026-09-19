//! Where one session's credentials come from.
//!
//! A session either carries its own access token or uses the stored login. The
//! choice is made once, at the invocation boundary, and then travels with the
//! session. No code below this module reads the process environment to decide
//! how a request is authorized, so one process can serve sessions that
//! authenticate differently.

use std::fmt;

/// The environment variable that carries a headless access token.
pub(super) const TOKEN_VAR: &str = "SEMCTX_TOKEN";

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

/// Which credentials a piece of shared work is authorized by.
///
/// Two sessions with different scopes must never share a coordinator, because
/// what one is allowed to read and write the other may not be. The scope is
/// comparable but carries no credential: an invocation token appears only as a
/// digest, so the key of a shared map cannot leak it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CredentialScope {
    /// The stored login. Every session that uses it has the same authorization.
    Stored,
    /// A token supplied with one invocation, identified by its blake3 digest.
    Invocation(String),
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

    /// An invocation token for tests that need a second credential scope.
    #[cfg(test)]
    pub(crate) fn from_test_token(value: &str) -> Self {
        Self::Invocation(Secret::new(value.to_string()))
    }

    /// The comparable identity of these credentials.
    ///
    /// The digest is computed here so the plain token never leaves this
    /// module. A caller that holds a scope cannot recover the token from it.
    pub(crate) fn scope(&self) -> CredentialScope {
        match self {
            Self::Invocation(secret) => CredentialScope::Invocation(
                blake3::hash(secret.expose().as_bytes())
                    .to_hex()
                    .to_string(),
            ),
            Self::Stored => CredentialScope::Stored,
        }
    }

    /// The pure mapping behind [`Self::from_environment`].
    ///
    /// [`super::SessionContext::from_handshake`] uses it for the token an
    /// attach body carries, so one rule decides what counts as a credential
    /// however the session was invoked.
    pub(super) fn from_token(value: Option<&str>) -> Self {
        match value.filter(|token| !token.trim().is_empty()) {
            Some(token) => Self::Invocation(Secret::new(token.to_string())),
            None => Self::Stored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CredentialScope, CredentialSource, Secret};

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

    /// The scope tells two authorizations apart without carrying either one.
    #[test]
    fn a_scope_identifies_credentials_without_exposing_them() {
        let first = CredentialSource::from_token(Some("first-token")).scope();
        let second = CredentialSource::from_token(Some("second-token")).scope();

        assert_eq!(
            first,
            CredentialSource::from_token(Some("first-token")).scope(),
            "one token must always map to one scope"
        );
        assert_ne!(first, second);
        assert_ne!(first, CredentialScope::Stored);
        assert!(
            !format!("{first:?}").contains("first-token"),
            "a scope must not carry the token it describes"
        );
    }

    /// A token must not be recoverable from a `{:?}` in a log line or an error.
    #[test]
    fn a_secret_is_redacted_in_debug_output() {
        let secret = Secret::new("super-secret-token".to_string());

        assert_eq!(format!("{secret:?}"), "Secret(redacted)");
        assert!(!format!("{:?}", CredentialSource::Invocation(secret)).contains("super-secret"));
    }
}
