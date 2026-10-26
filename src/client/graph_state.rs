//! The completeness headers of a graph answer.
//!
//! semctx answers a degraded graph read with a success status and the usual
//! body. It reports the degradation only in three headers (see
//! `GraphReadHeaders` in the semctx server):
//!
//! - `X-Semctx-Graph-State`: `partial` on a degraded answer, absent otherwise.
//! - `X-Semctx-Graph-Invalidation`: the reason: `providers-unavailable`,
//!   `flow-truncated`, `providers-loading`, `source-incomplete`, or
//!   `graph-loading`.
//! - `X-Semctx-Graph-Repair`: `pending` when the server still builds the
//!   missing part, `none` when a retry gets the same result.
//!
//! The client keeps a partial answer and does not retry it. A `none` answer
//! does not change on a retry, and a `pending` answer is the best result the
//! server has now. The client puts each partial answer in the
//! [`PartialAnswers`] record of the call, and the caller shows it to the user:
//! the CLI on stderr, the MCP server in the tool result. A client without a
//! record writes the partial answer to the log.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use reqwest::header::HeaderMap;
use tracing::warn;

const STATE_HEADER: &str = "X-Semctx-Graph-State";
const INVALIDATION_HEADER: &str = "X-Semctx-Graph-Invalidation";
const REPAIR_HEADER: &str = "X-Semctx-Graph-Repair";
const PARTIAL: &str = "partial";

/// The most partial answers that one record keeps. The record keeps each
/// distinct answer once, and the server sends only a small set of values, so
/// a normal call stays far below this bound. The bound keeps the memory of a
/// record small when a server sends unexpected values.
const MAX_RECORDED: usize = 8;

/// Where the recovery of a partial answer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repair {
    /// The server still builds the missing part. A later call can return more.
    Pending,
    /// No run restores the missing part. A retry gets the same result.
    None,
    /// The server sent no repair value, or a value this client does not know.
    Unknown,
}

impl Repair {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some(value) if value.eq_ignore_ascii_case("pending") => Self::Pending,
            Some(value) if value.eq_ignore_ascii_case("none") => Self::None,
            _ => Self::Unknown,
        }
    }
}

/// A partial graph answer, as its headers describe it. Its display text is
/// the warning for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PartialAnswer {
    /// The invalidation reason as the server sent it, for example
    /// `providers-unavailable`. `None` when the header is absent.
    reason: Option<String>,
    repair: Repair,
}

impl PartialAnswer {
    /// Read the headers of a success answer. Returns `None` for a complete
    /// answer, that is, an answer without `X-Semctx-Graph-State: partial`.
    fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        if !text(STATE_HEADER).is_some_and(|state| state.eq_ignore_ascii_case(PARTIAL)) {
            return None;
        }
        Some(Self {
            reason: text(INVALIDATION_HEADER).map(str::to_owned),
            repair: Repair::parse(text(REPAIR_HEADER)),
        })
    }
}

impl fmt::Display for PartialAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = self.reason.as_deref().unwrap_or("unknown reason");
        write!(f, "the graph answer is partial ({reason})")?;
        match self.repair {
            Repair::Pending => f.write_str(
                "; the server still builds the missing part, so a later call can return more",
            ),
            Repair::None => f.write_str("; a retry returns the same result"),
            Repair::Unknown => Ok(()),
        }
    }
}

/// The partial graph answers of one call.
///
/// The caller makes one record for one call and gives it to the client with
/// [`super::Client::with_partial_answers`]. Every clone of that client adds to
/// the same record. After the call, the caller reads the record and shows each
/// answer to the user. A client that a long-lived owner keeps never has a
/// record, so a later call does not show the answers of an earlier one.
#[derive(Clone, Debug, Default)]
pub(crate) struct PartialAnswers(Arc<Mutex<Vec<PartialAnswer>>>);

impl PartialAnswers {
    /// Add `answer`, unless the record has it already or is full.
    fn record(&self, answer: PartialAnswer) {
        let mut answers = self.lock();
        if answers.len() < MAX_RECORDED && !answers.contains(&answer) {
            answers.push(answer);
        }
    }

    /// The recorded answers, each distinct answer once, in the order in which
    /// they arrived.
    pub(crate) fn answers(&self) -> Vec<PartialAnswer> {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<PartialAnswer>> {
        // The lock only guards a push or a clone. A panic cannot leave the
        // list half changed, so a poisoned lock still holds valid data.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Look at the headers of a success answer. Put a partial answer in `record`,
/// or write it to the log when the caller keeps no record. The answer itself
/// stays as the server sent it.
pub(super) fn observe(
    method: &str,
    url: &str,
    headers: &HeaderMap,
    record: Option<&PartialAnswers>,
) {
    let Some(answer) = PartialAnswer::from_headers(headers) else {
        return;
    };
    if let Some(record) = record {
        record.record(answer);
    } else {
        // No caller shows this answer to a user, so the log is its only place.
        warn!("{method} {url}: {answer}");
    }
}

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderMap, HeaderValue};

    use super::{MAX_RECORDED, PartialAnswer, PartialAnswers, Repair, observe};

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    fn partial_headers(reason: &'static str, repair: &'static str) -> HeaderMap {
        headers(&[
            ("X-Semctx-Graph-State", "partial"),
            ("X-Semctx-Graph-Invalidation", reason),
            ("X-Semctx-Graph-Repair", repair),
        ])
    }

    fn partial(reason: &'static str, repair: &'static str) -> PartialAnswer {
        PartialAnswer::from_headers(&partial_headers(reason, repair))
            .expect("the answer is partial")
    }

    #[test]
    fn an_answer_without_graph_headers_is_complete() {
        assert_eq!(PartialAnswer::from_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn an_answer_with_another_state_is_complete() {
        let map = headers(&[
            ("X-Semctx-Graph-State", "complete"),
            ("X-Semctx-Graph-Invalidation", "flow-truncated"),
            ("X-Semctx-Graph-Repair", "none"),
        ]);
        assert_eq!(PartialAnswer::from_headers(&map), None);
    }

    #[test]
    fn unavailable_providers_warn_that_a_retry_returns_the_same_result() {
        let partial = partial("providers-unavailable", "none");

        assert_eq!(partial.repair, Repair::None);
        assert_eq!(
            partial.to_string(),
            "the graph answer is partial (providers-unavailable); a retry returns the same result"
        );
    }

    #[test]
    fn a_truncated_flow_warns_that_a_retry_returns_the_same_result() {
        let partial = partial("flow-truncated", "none");

        assert_eq!(partial.repair, Repair::None);
        assert_eq!(
            partial.to_string(),
            "the graph answer is partial (flow-truncated); a retry returns the same result"
        );
    }

    #[test]
    fn loading_providers_warn_that_a_later_call_can_return_more() {
        let partial = partial("providers-loading", "pending");

        assert_eq!(partial.repair, Repair::Pending);
        assert_eq!(
            partial.to_string(),
            "the graph answer is partial (providers-loading); the server still builds the \
             missing part, so a later call can return more"
        );
    }

    #[test]
    fn every_pending_reason_warns_that_a_later_call_can_return_more() {
        for reason in ["source-incomplete", "graph-loading"] {
            let partial = partial(reason, "pending");

            assert_eq!(partial.repair, Repair::Pending, "{reason}");
            assert_eq!(
                partial.to_string(),
                format!(
                    "the graph answer is partial ({reason}); the server still builds the \
                     missing part, so a later call can return more"
                )
            );
        }
    }

    #[test]
    fn header_values_ignore_case_and_surrounding_space() {
        let partial = PartialAnswer::from_headers(&headers(&[
            ("X-Semctx-Graph-State", " Partial "),
            ("X-Semctx-Graph-Invalidation", "flow-truncated"),
            ("X-Semctx-Graph-Repair", "NONE"),
        ]))
        .expect("the answer is partial");

        assert_eq!(partial.reason.as_deref(), Some("flow-truncated"));
        assert_eq!(partial.repair, Repair::None);
    }

    #[test]
    fn an_unknown_repair_value_gives_no_retry_advice() {
        let partial = partial("providers-unavailable", "later");

        assert_eq!(partial.repair, Repair::Unknown);
        assert_eq!(
            partial.to_string(),
            "the graph answer is partial (providers-unavailable)"
        );
    }

    #[test]
    fn a_partial_answer_without_reason_or_repair_still_warns() {
        let partial = PartialAnswer::from_headers(&headers(&[("X-Semctx-Graph-State", "partial")]))
            .expect("the answer is partial");

        assert_eq!(partial.repair, Repair::Unknown);
        assert_eq!(
            partial.to_string(),
            "the graph answer is partial (unknown reason)"
        );
    }

    #[test]
    fn a_record_keeps_each_distinct_answer_once_in_arrival_order() {
        let record = PartialAnswers::default();
        let clone = record.clone();

        for (reason, repair) in [
            ("flow-truncated", "none"),
            ("providers-loading", "pending"),
            ("flow-truncated", "none"),
        ] {
            observe("GET", "/g", &partial_headers(reason, repair), Some(&clone));
        }
        observe("GET", "/g", &HeaderMap::new(), Some(&clone));

        assert_eq!(
            record.answers(),
            vec![
                partial("flow-truncated", "none"),
                partial("providers-loading", "pending")
            ],
            "a clone adds to the same record, and a complete answer adds nothing"
        );
    }

    #[test]
    fn a_record_keeps_a_bounded_number_of_answers() {
        let record = PartialAnswers::default();
        let reasons: Vec<String> = (0..=MAX_RECORDED).map(|n| format!("reason-{n}")).collect();

        for reason in &reasons {
            let mut map = partial_headers("unused", "none");
            map.insert(
                "X-Semctx-Graph-Invalidation",
                HeaderValue::from_str(reason).expect("a valid header value"),
            );
            observe("GET", "/g", &map, Some(&record));
        }

        let answers = record.answers();
        assert_eq!(answers.len(), MAX_RECORDED);
        assert_eq!(answers[0].reason.as_deref(), Some("reason-0"));
    }
}
