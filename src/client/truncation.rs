//! The header of a search or grep result that a response budget of the server
//! shortened.
//!
//! semctx sends `X-Semctx-Result-Truncated: content-budget` on
//! `POST /v1/search` and `GET /v1/codebases/{id}/grep` when its content budget
//! cut a snippet or line, left a Symbol hit unexpanded, or dropped later
//! results. The status stays 200 and the body keeps its shape, so only the
//! header tells the client that the result is not complete.

use reqwest::header::HeaderMap;

const TRUNCATED_HEADER: &str = "X-Semctx-Result-Truncated";
const CONTENT_BUDGET: &str = "content-budget";

/// Why the server shortened a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResultTruncation {
    /// The content budget of the server (`content-budget`).
    ContentBudget,
    /// A value that this client does not know. The header still says that the
    /// result is shortened, so the client reports the value as it is.
    Other(String),
}

impl ResultTruncation {
    /// Read the headers of a success answer. `None` when the result is
    /// complete, that is, when the header is absent.
    pub(crate) fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let value = headers.get(TRUNCATED_HEADER)?;
        let value = String::from_utf8_lossy(value.as_bytes()).trim().to_owned();
        Some(if value.eq_ignore_ascii_case(CONTENT_BUDGET) {
            Self::ContentBudget
        } else {
            Self::Other(value)
        })
    }

    /// The header and its value, as the server sent them.
    pub(crate) fn header(&self) -> String {
        let value = match self {
            Self::ContentBudget => CONTENT_BUDGET,
            Self::Other(value) => value,
        };
        format!("{TRUNCATED_HEADER}: {value}")
    }
}

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderMap, HeaderValue};

    use super::ResultTruncation;

    fn headers(value: Option<&'static str>) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Some(value) = value {
            map.insert("X-Semctx-Result-Truncated", HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn only_a_present_header_marks_a_result_as_shortened() {
        assert_eq!(ResultTruncation::from_headers(&headers(None)), None);
        assert_eq!(
            ResultTruncation::from_headers(&headers(Some(" Content-Budget "))),
            Some(ResultTruncation::ContentBudget)
        );
        let other = ResultTruncation::from_headers(&headers(Some("line-budget")))
            .expect("an unknown value still marks the result as shortened");
        assert_eq!(other, ResultTruncation::Other("line-budget".into()));
        assert_eq!(other.header(), "X-Semctx-Result-Truncated: line-budget");
    }
}
