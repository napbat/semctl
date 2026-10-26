//! The budget of one MCP tool call: its deadline and its record of partial
//! graph answers.
//!
//! `call_tool` builds one [`CallBudget`] per call and puts it in the request's
//! extensions. Each tool receives it and passes it on, explicitly, to the client
//! it builds and to the first-index waits. Nothing reads a deadline or a record
//! from ambient state, so a client that a long-lived owner keeps never has a
//! deadline or a record from an earlier call.

use std::{borrow::Cow, time::Duration};

use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{CallToolResult, Content, IntoContents},
};
use tokio::time::{Instant, error::Elapsed};

use super::{DIRECT_EDIT_TOOLS, McpServer};
use crate::client::{Client, DEADLINE_MARGIN, PartialAnswers};
use crate::query::{FailureKind, ToolError};

/// How long one call may run, as a moment in time, and the partial graph
/// answers that the call got. An unbounded budget has no moment.
///
/// A clone shares the record, so the copy that a tool receives adds to the
/// record that `call_tool` reads.
#[derive(Clone, Debug)]
pub(crate) struct CallBudget {
    deadline: Option<Instant>,
    partial_answers: PartialAnswers,
}

impl CallBudget {
    /// A call that must end by `deadline`.
    pub(crate) fn until(deadline: Instant) -> Self {
        Self {
            deadline: Some(deadline),
            partial_answers: PartialAnswers::default(),
        }
    }

    /// A call that nothing cuts off. The edit tools use it: they change files,
    /// and a call cut off after the change would report a failure for an edit
    /// that happened.
    pub(crate) fn unbounded() -> Self {
        Self {
            deadline: None,
            partial_answers: PartialAnswers::default(),
        }
    }

    /// `client` for this call: bounded by the deadline, and with its partial
    /// graph answers put in the record of this call. Never give the result to
    /// an owner that outlives the call.
    pub(crate) fn for_call(&self, client: Client) -> Client {
        client
            .with_deadline(self.deadline)
            .with_partial_answers(self.partial_answers.clone())
    }

    /// `result` with one note for each partial graph answer of this call, so
    /// the agent knows that the answer is not complete. An error result gets
    /// no note: the agent has no answer to qualify.
    pub(crate) fn with_partial_notes(&self, mut result: CallToolResult) -> CallToolResult {
        if result.is_error != Some(true) {
            result.content.extend(
                self.partial_answers
                    .answers()
                    .iter()
                    .map(|answer| Content::text(format!("note: {answer}"))),
            );
        }
        result
    }

    /// The moment the call must end by, or `None` when it is unbounded, for
    /// tests that check which clients carry it.
    #[cfg(test)]
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
