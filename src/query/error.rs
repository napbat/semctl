//! The one failure surface of every MCP tool and CLI query.
//!
//! A failed call returns a [`ToolError`]. The MCP layer turns it into a result
//! with `isError: true`. The CLI prints its first line and exits with a
//! non-zero status. Both read the same typed cause, never the message text.

use std::{fmt, time::Duration};

use rmcp::model::{Content, IntoContents};

use crate::client::ApiFailure;

/// What the caller can do about a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The server is loading the graph or a file. A repeat call succeeds
    /// after the delay.
    Retryable { after: Duration },
    /// The server, a gateway, the network, or the credentials did not give an
    /// answer. The caller uses its local tools.
    Unavailable,
    /// The call named something that does not exist or is not valid.
    InvalidArgument,
    /// The first index of the codebase is still running.
    IndexPending,
    /// The first index of the codebase failed.
    IndexFailed,
    /// An edit precondition failed. The edit changed no file.
    Refused,
}

/// A failed tool call: the operation, the kind of failure, and the reason.
#[derive(Debug)]
pub struct ToolError {
    op: &'static str,
    kind: FailureKind,
    detail: String,
}

impl ToolError {
    pub fn new(op: &'static str, kind: FailureKind, detail: impl Into<String>) -> Self {
        Self {
            op,
            kind,
            detail: detail.into(),
        }
    }

    /// Classify an error from the HTTP client by its typed cause. The detail is
    /// the full error chain, as a user reads it.
    pub fn from_client(op: &'static str, error: &anyhow::Error) -> Self {
        let kind = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ApiFailure>())
            .map_or(FailureKind::Unavailable, classify);
        Self::new(op, kind, format!("{error:#}"))
    }

    /// The first line of the rendering: `<op> failed: <detail>`. The CLI prints
    /// only this line, because the next step is guidance for a model.
    pub fn summary(&self) -> String {
        format!("{} failed: {}", self.op, self.detail)
    }

    /// The error for a CLI command: the summary line, with no next step.
    pub fn into_cli_error(self) -> anyhow::Error {
        anyhow::Error::msg(self.summary())
    }
}

fn classify(failure: &ApiFailure) -> FailureKind {
    if failure.is_loading() {
        return FailureKind::Retryable {
            after: failure.loading_delay(),
        };
    }
    match failure.status().as_u16() {
        400 | 404 | 422 => FailureKind::InvalidArgument,
        _ => FailureKind::Unavailable,
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}\nnext: ", self.summary())?;
        match self.kind {
            FailureKind::Retryable { after } => write!(
                formatter,
                "retry this call once after {} s. If it fails again, use local Read/Grep for \
                 this request.",
                after.as_millis().div_ceil(1000)
            ),
            FailureKind::Unavailable => {
                formatter.write_str("use local Read/Grep for this request.")
            }
            FailureKind::InvalidArgument => {
                formatter.write_str("correct the argument and call again.")
            }
            FailureKind::IndexPending => formatter.write_str(
                "call sync_status to follow the first index, and use local Read/Grep until it \
                 completes.",
            ),
            FailureKind::IndexFailed => {
                formatter.write_str("call index_codebase for this path to retry the first index.")
            }
            FailureKind::Refused => formatter
                .write_str("read the reason above, and do not repeat the same call unchanged."),
        }
    }
}

impl std::error::Error for ToolError {}

/// With this conversion a tool that returns `Result<String, ToolError>` makes
/// rmcp mark the failure result with `isError: true`.
impl IntoContents for ToolError {
    fn into_contents(self) -> Vec<Content> {
        vec![Content::text(self.to_string())]
    }
}

#[cfg(test)]
mod tests;
