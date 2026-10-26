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
//! server has now. The client tells the user on stderr that the answer is
//! partial, why, and whether a later call can return more.

use reqwest::header::HeaderMap;
use tracing::warn;

const STATE_HEADER: &str = "X-Semctx-Graph-State";
const INVALIDATION_HEADER: &str = "X-Semctx-Graph-Invalidation";
const REPAIR_HEADER: &str = "X-Semctx-Graph-Repair";
const PARTIAL: &str = "partial";

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

/// A partial graph answer, as its headers describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Partial {
    /// The invalidation reason as the server sent it, for example
    /// `providers-unavailable`. `None` when the header is absent.
    reason: Option<String>,
    repair: Repair,
}

impl Partial {
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

    /// The warning text for the user.
    fn warning(&self) -> String {
        let reason = self.reason.as_deref().unwrap_or("unknown reason");
        let mut text = format!("the graph answer is partial ({reason})");
        match self.repair {
            Repair::Pending => text.push_str(
                "; the server still builds the missing part, so a later call can return more",
            ),
            Repair::None => text.push_str("; a retry returns the same result"),
            Repair::Unknown => {}
        }
        text
    }
}

/// Write a warning to stderr when a success answer is a partial graph answer.
/// The answer itself stays as the server sent it.
pub(super) fn warn_if_partial(method: &str, url: &str, headers: &HeaderMap) {
    if let Some(partial) = Partial::from_headers(headers) {
        warn!("{method} {url}: {}", partial.warning());
    }
}

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderMap, HeaderValue};

    use super::{Partial, Repair};

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    fn partial(reason: &'static str, repair: &'static str) -> Partial {
        Partial::from_headers(&headers(&[
            ("X-Semctx-Graph-State", "partial"),
            ("X-Semctx-Graph-Invalidation", reason),
            ("X-Semctx-Graph-Repair", repair),
        ]))
        .expect("the answer is partial")
    }

    #[test]
    fn an_answer_without_graph_headers_is_complete() {
        assert_eq!(Partial::from_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn an_answer_with_another_state_is_complete() {
        let map = headers(&[
            ("X-Semctx-Graph-State", "complete"),
            ("X-Semctx-Graph-Invalidation", "flow-truncated"),
            ("X-Semctx-Graph-Repair", "none"),
        ]);
        assert_eq!(Partial::from_headers(&map), None);
    }

    #[test]
    fn unavailable_providers_warn_that_a_retry_returns_the_same_result() {
        let partial = partial("providers-unavailable", "none");

        assert_eq!(partial.repair, Repair::None);
        assert_eq!(
            partial.warning(),
            "the graph answer is partial (providers-unavailable); a retry returns the same result"
        );
    }

    #[test]
    fn a_truncated_flow_warns_that_a_retry_returns_the_same_result() {
        let partial = partial("flow-truncated", "none");

        assert_eq!(partial.repair, Repair::None);
        assert_eq!(
            partial.warning(),
            "the graph answer is partial (flow-truncated); a retry returns the same result"
        );
    }

    #[test]
    fn loading_providers_warn_that_a_later_call_can_return_more() {
        let partial = partial("providers-loading", "pending");

        assert_eq!(partial.repair, Repair::Pending);
        assert_eq!(
            partial.warning(),
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
                partial.warning(),
                format!(
                    "the graph answer is partial ({reason}); the server still builds the \
                     missing part, so a later call can return more"
                )
            );
        }
    }

    #[test]
    fn header_values_ignore_case_and_surrounding_space() {
        let partial = Partial::from_headers(&headers(&[
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
            partial.warning(),
            "the graph answer is partial (providers-unavailable)"
        );
    }

    #[test]
    fn a_partial_answer_without_reason_or_repair_still_warns() {
        let partial = Partial::from_headers(&headers(&[("X-Semctx-Graph-State", "partial")]))
            .expect("the answer is partial");

        assert_eq!(partial.repair, Repair::Unknown);
        assert_eq!(
            partial.warning(),
            "the graph answer is partial (unknown reason)"
        );
    }
}
