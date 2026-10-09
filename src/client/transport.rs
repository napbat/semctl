//! The shared HTTP transport.
//!
//! One `reqwest::Client` serves every [`super::Client`] built from it. The
//! `reqwest` client owns the connection pool, so sharing it is what keeps a
//! process that serves many sessions from opening one pool per session.

use std::time::Duration;

use anyhow::{Context, Result};

/// The longest wait to open one connection. The transport sets no limit on a
/// whole request: a large sync upload over a slow link must not be cut off.
/// A caller that needs one bounds its calls through
/// [`super::Client::with_deadline`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A `reqwest::Client` with this program's user agent, shared by every
/// [`super::Client`] built against it.
pub(crate) struct HttpTransport {
    http: reqwest::Client,
}

impl HttpTransport {
    /// Build the shared client. The builder reads proxy settings from the
    /// environment and loads the TLS roots, and either can fail.
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("semctx-cli/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .context("build HTTP client")?,
        })
    }

    /// The shared HTTP client. Clone it into a [`super::Client`], or borrow it
    /// for a request that carries no codebase or tenant selection.
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }
}
