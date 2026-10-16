//! The time budget of one MCP tool call.
//!
//! `call_tool` builds one [`CallBudget`] per call and puts it in the request's
//! extensions. Each tool receives it and passes it on, explicitly, to the client
//! it builds and to the first-index waits. Nothing reads a deadline from ambient
//! state, so a client that a long-lived owner keeps never has a deadline from an
//! earlier call.

use std::{borrow::Cow, time::Duration};

use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{CallToolResult, IntoContents},
};
use tokio::time::{Instant, error::Elapsed};

use super::{DIRECT_EDIT_TOOLS, McpServer};
use crate::client::DEADLINE_MARGIN;
use crate::query::{FailureKind, ToolError};

/// How long one call may run, as a moment in time. An unbounded budget has no
/// moment.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CallBudget {
    deadline: Option<Instant>,
}

impl CallBudget {
    /// A call that must end by `deadline`.
    pub(crate) fn until(deadline: Instant) -> Self {
        Self {
            deadline: Some(deadline),
        }
    }

    /// A call that nothing cuts off. The edit tools use it: they change files,
    /// and a call cut off after the change would report a failure for an edit
    /// that happened.
    pub(crate) fn unbounded() -> Self {
        Self { deadline: None }
    }

    /// The moment the call must end by, or `None` when it is unbounded.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// The time left before the deadline, or `None` when the call is
    /// unbounded. A deadline in the past leaves zero.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    /// The moment a wait that starts now must end by: after `limit`, or when
    /// the deadline leaves no more than [`DEADLINE_MARGIN`], whichever comes
    /// first. `None` when the call is unbounded and the wait has no end.
    ///
    /// The margin keeps the wait from ending at the same moment as the
    /// deadline, so the call reports the wait's own error and not the
    /// deadline's.
    pub(crate) fn wait_until(&self, limit: Duration) -> Option<Instant> {
        let usable = self.remaining()?.saturating_sub(DEADLINE_MARGIN);
        Some(Instant::now() + limit.min(usable))
    }

    /// Run `call`, and drop it at the deadline. This backstop ends work that no
    /// client request bounds, such as a wait for a gate.
    pub(crate) async fn run<F: Future>(&self, call: F) -> Result<F::Output, Elapsed> {
        match self.deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, call).await,
            None => Ok(call.await),
        }
    }
}

/// The budget of a call to `tool` that starts at `now`.
///
/// The edit tools are unbounded. Their commit runs in a blocking task that a
/// timeout cannot interrupt, so a deadline could only misreport an edit that
/// was applied and lose its `edit_id`. Every other tool has `deadline`.
pub(crate) fn call_budget(tool: &str, now: Instant, deadline: Duration) -> CallBudget {
    if DIRECT_EDIT_TOOLS.contains(&tool) {
        CallBudget::unbounded()
    } else {
        CallBudget::until(now + deadline)
    }
}

/// The result of a call that had no answer at its deadline.
pub(crate) fn deadline_result(op: &'static str, deadline: Duration) -> CallToolResult {
    let error = ToolError::new(
        op,
        FailureKind::Unavailable,
        format!(
            "no answer within the {} s call deadline",
            deadline.as_secs()
        ),
    );
    CallToolResult::error(error.into_contents())
}

/// The static name of the tool `name`, for an error that must name it.
///
/// The router builds each tool name from a string literal, so it is borrowed
/// for the life of the program. A name that no tool has gets a generic label;
/// the router refuses such a call before any deadline can fire.
pub(super) fn tool_op(router: &ToolRouter<McpServer>, name: &str) -> &'static str {
    match router.get(name).map(|tool| &tool.name) {
        Some(Cow::Borrowed(op)) => op,
        _ => "tool call",
    }
}

#[cfg(test)]
mod tests;
