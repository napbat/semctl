//! The one failure surface of every MCP tool and CLI query.
//!
//! A failed call returns a [`ToolError`]. The MCP layer turns it into a result
//! with `isError: true`. The CLI reports the same cause and exits with a
//! non-zero status. Both read the typed cause, never the message text.

use std::{fmt, time::Duration};

use rmcp::model::{Content, IntoContents};

use crate::client::{ApiFailure, LoadingKind};

/// What the caller can do about a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The server is still loading the code graph.
    GraphLoading { after: Duration },
    /// The server is still loading the requested file.
    FileLoading { after: Duration },
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

    /// The leading line of the error, without HTTP details for loading failures.
    pub fn summary(&self) -> String {
        match self.kind {
            FailureKind::GraphLoading { .. } => format!(
                "{}: the server is still loading the code graph for this codebase (GraphLoading).",
                self.op
            ),
            FailureKind::FileLoading { .. } => format!(
                "{}: the server is still loading the requested file (FileLoading).",
                self.op
            ),
            _ => format!("{} failed: {}", self.op, self.detail),
        }
    }

    /// Loading failures give CLI users a next step and retain the HTTP cause.
    pub fn into_cli_error(self) -> anyhow::Error {
        let summary = self.summary();
        let next = match self.kind {
            FailureKind::GraphLoading { after } => format!(
                "run `semctl search` or search the local checkout now. \
                 Try graph commands again in about {} s.",
                seconds(after)
            ),
            FailureKind::FileLoading { after } => format!(
                "read the local file if the requested revision is available. \
                 Try the file request again in about {} s.",
                seconds(after)
            ),
            _ => return anyhow::Error::msg(summary),
        };
        anyhow::Error::msg(self.detail).context(format!("{summary}\nnext: {next}"))
    }
}

fn classify(failure: &ApiFailure) -> FailureKind {
    match failure.loading_kind() {
        Some(LoadingKind::Graph) => FailureKind::GraphLoading {
            after: failure.loading_window(),
        },
        Some(LoadingKind::File) => FailureKind::FileLoading {
            after: failure.loading_window(),
        },
        None => match failure.status().as_u16() {
            400 | 404 | 422 if failure.is_api_response() => FailureKind::InvalidArgument,
            _ => FailureKind::Unavailable,
        },
    }
}

fn seconds(delay: Duration) -> u128 {
    delay.as_millis().div_ceil(1000)
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.summary())?;
        if matches!(
            self.kind,
            FailureKind::GraphLoading { .. } | FailureKind::FileLoading { .. }
        ) {
            write!(formatter, "\ndetail: {}", self.detail)?;
        }
        formatter.write_str("\nnext: ")?;
        match self.kind {
            FailureKind::GraphLoading { after } => write!(
                formatter,
                "use search_codebase or grep now; they do not need the graph. \
                 Try graph tools again in about {} s. sync_status reports index state.",
                seconds(after)
            ),
            FailureKind::FileLoading { after } => write!(
                formatter,
                "use local Read if the requested revision is available. \
                 Try the file request again in about {} s.",
                seconds(after)
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
