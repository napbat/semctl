//! The shared HTTP transport.
//!
//! One `reqwest::Client` serves every [`super::Client`] built from it. The
//! `reqwest` client owns the connection pool, so sharing it is what keeps a
//! process that serves many sessions from opening one pool per session.

/// A `reqwest::Client` with this program's user agent, shared by every
/// [`super::Client`] built against it.
pub(crate) struct HttpTransport {
    http: reqwest::Client,
}

impl HttpTransport {
    pub(crate) fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("semctx-cli/", env!("CARGO_PKG_VERSION")))
                .build()
                // The builder only fails on TLS or proxy configuration this
                // call never sets.
                .expect("reqwest client build is infallible with default config"),
        }
    }

    /// The shared HTTP client. Clone it into a [`super::Client`], or borrow it
    /// for a request that carries no codebase or tenant selection.
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new()
    }
}
