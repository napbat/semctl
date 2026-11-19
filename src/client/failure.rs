//! The typed error of one failed HTTP exchange with the server.
//!
//! Every non-success answer reaches the caller as an [`ApiFailure`] inside the
//! returned `anyhow::Error`. A caller that must act on the cause reads the typed
//! status, code, and `Retry-After` delay. It never matches on the message text.

use std::{fmt, time::Duration};

use reqwest::StatusCode;

const LOADING_RETRY_MIN_DELAY: Duration = Duration::from_secs(1);
const LOADING_RETRY_MAX_DELAY: Duration = Duration::from_secs(5);
const LOADING_WINDOW_MIN: Duration = Duration::from_secs(15);
const LOADING_WINDOW_MAX: Duration = Duration::from_secs(60);

/// The projection that is not ready for the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadingKind {
    Graph,
    File,
}

/// Whether the response follows the API error contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureResponse {
    Api,
    Unexpected,
}

/// A request that the server, or a gateway in front of it, did not answer with
/// success.
///
/// The fields stay private. A caller reads the typed facts through the
/// accessors and never matches on the message text.
#[derive(Debug)]
pub(crate) struct ApiFailure {
    status: StatusCode,
    response: FailureResponse,
    /// The first typed error code of the response, for example `GraphLoading`.
    code: Option<String>,
    /// The delay in seconds that the response asked for with `Retry-After`, for
    /// any status. The value is as the response sent it. A caller that waits
    /// bounds it first; see [`Self::loading_delay`].
    retry_after: Option<Duration>,
    /// The text that a person reads: the request, the status, and the reason.
    message: String,
}

impl ApiFailure {
    pub(crate) fn new(
        status: StatusCode,
        code: Option<String>,
        retry_after: Option<Duration>,
        message: String,
    ) -> Self {
        Self {
            status,
            response: FailureResponse::Api,
            code,
            retry_after,
            message,
        }
    }

    /// A failure that the server reported in its own JSON envelope.
    pub(super) fn from_errors(
        method: &str,
        url: &str,
        status: StatusCode,
        retry_after: Option<Duration>,
        errors: &[serde_json::Value],
    ) -> Self {
        Self::new(
            status,
            first_error_code(errors),
            retry_after,
            format!("{method} {url} -> {status}: {}", summarize_errors(errors)),
        )
    }

    /// The status of the failed response.
    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    /// Only an API response can attribute a failure to the caller's arguments.
    /// A gateway can return the same status for a missing upstream route.
    pub(crate) fn is_api_response(&self) -> bool {
        self.response == FailureResponse::Api
    }

    /// The delay that the response asked for before another attempt, when it
    /// gave one in seconds. The value is not bounded.
    pub(crate) fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    /// Which projection the server reports as still loading.
    pub(crate) fn loading_kind(&self) -> Option<LoadingKind> {
        if self.status != StatusCode::CONFLICT {
            return None;
        }
        match self.code.as_deref()? {
            code if code.eq_ignore_ascii_case("GraphLoading") => Some(LoadingKind::Graph),
            code if code.eq_ignore_ascii_case("FileLoading") => Some(LoadingKind::File),
            _ => None,
        }
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.loading_kind().is_some()
    }

    /// The delay between attempts of one call. Without a header, do not retry.
    pub(super) fn loading_delay(&self) -> Option<Duration> {
        self.retry_after()
            .map(|delay| delay.clamp(LOADING_RETRY_MIN_DELAY, LOADING_RETRY_MAX_DELAY))
    }

    /// How long later calls should avoid waiting for the same codebase again.
    pub(crate) fn loading_window(&self) -> Duration {
        self.retry_after()
            .unwrap_or_default()
            .clamp(LOADING_WINDOW_MIN, LOADING_WINDOW_MAX)
    }
}

impl fmt::Display for ApiFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApiFailure {}

/// The `code` of the first typed error in an `errors` array. An entry without
/// a string `code` does not count.
fn first_error_code(errors: &[serde_json::Value]) -> Option<String> {
    errors.iter().find_map(|error| {
        error
            .as_object()?
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("code"))?
            .1
            .as_str()
            .map(str::to_owned)
    })
}

pub(super) fn summarize_errors(errors: &[serde_json::Value]) -> String {
    if errors.is_empty() {
        return "request failed".to_string();
    }
    errors
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Preserve a structured JSON denial even when it is not wrapped in the
/// resource server's usual API envelope. Tenant binding failures can be emitted
/// by middleware before controller envelope handling runs.
pub(super) fn response_body_error(
    method: &str,
    url: &str,
    status: StatusCode,
    body: &str,
) -> anyhow::Error {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
        return gateway_error(method, url, status, None, body);
    };
    // The denial is either the envelope or a bare error object.
    let errors = parsed
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .map_or(std::slice::from_ref(&parsed), Vec::as_slice);
    anyhow::Error::new(ApiFailure::new(
        status,
        first_error_code(errors),
        None,
        format!("{method} {url} -> {status}: {body}"),
    ))
}

/// An error for a response that is not the API's JSON envelope at all.
///
/// A gateway between the CLI and the server (ingress, proxy, load balancer)
/// answers failures in ITS format, not the API's — typically an HTML error page.
/// Parsing that as the envelope produces `expected value at line 1 column 1`,
/// which names the CLI's own parser rather than the thing that actually went
/// wrong, and buries the status code that IS the diagnosis.
///
/// Reported by status instead. A response outside the API contract cannot
/// establish that the caller's arguments are invalid.
pub(super) fn gateway_error(
    method: &str,
    url: &str,
    status: StatusCode,
    retry_after: Option<Duration>,
    body: &str,
) -> anyhow::Error {
    let hint = match status.as_u16() {
        504 => "the gateway timed out waiting for the server — the request may still be running",
        502 => "the gateway could not reach the server, or the server closed the connection",
        503 => "the server is unavailable behind the gateway (starting, draining, or overloaded)",
        530 => "the gateway could not route the request to the server",
        _ => "the response was not the API's JSON envelope",
    };
    // A short excerpt only: an HTML error page is pages long and none of it is
    // the diagnosis, but a truncated peek still distinguishes "HTML page" from
    // "empty body" when someone needs it.
    let excerpt: String = body.trim().chars().take(120).collect();
    anyhow::Error::new(ApiFailure {
        status,
        response: FailureResponse::Unexpected,
        code: None,
        retry_after,
        message: format!("{method} {url} -> {status}: {hint} (unexpected response: {excerpt:?})"),
    })
}

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

#[cfg(test)]
mod tests;
