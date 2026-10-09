//! A failed response, with the facts a caller acts on kept typed.

use std::fmt;
use std::time::Duration;

/// The server, or a gateway in front of it, answered with a failure.
///
/// The message is the complete text a person reads. The status and the
/// advertised retry delay stay typed, so a caller can tell a rejected request
/// from a request that can succeed later without parsing that text.
#[derive(Debug)]
pub(crate) struct ResponseError {
    status: reqwest::StatusCode,
    retry_after: Option<Duration>,
    message: String,
}

impl ResponseError {
    pub(crate) fn new(
        status: reqwest::StatusCode,
        retry_after: Option<Duration>,
        message: String,
    ) -> Self {
        Self {
            status,
            retry_after,
            message,
        }
    }

    /// The status of the failed response.
    pub(crate) fn status(&self) -> reqwest::StatusCode {
        self.status
    }

    /// The delay the response asked for before another attempt, when it gave
    /// one in seconds.
    pub(crate) fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

impl fmt::Display for ResponseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ResponseError {}

/// Read a `Retry-After` header in its delta-seconds form. The HTTP-date form
/// is ignored: no server or gateway in front of semctx sends it.
pub(super) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds))
}
