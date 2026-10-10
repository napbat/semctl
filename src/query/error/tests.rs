use std::time::Duration;

use reqwest::StatusCode;
use rmcp::handler::server::tool::IntoCallToolResult;

use super::{FailureKind, ToolError};
use crate::client::ApiFailure;

fn api_failure(
    status: StatusCode,
    code: Option<&str>,
    retry_after: Option<Duration>,
) -> anyhow::Error {
    anyhow::Error::new(ApiFailure::new(
        status,
        code.map(str::to_owned),
        retry_after,
        format!("GET https://example/v1/x -> {status}: failed"),
    ))
}

fn classified(
    status: StatusCode,
    code: Option<&str>,
    retry_after: Option<Duration>,
) -> FailureKind {
    ToolError::from_client("trace", &api_failure(status, code, retry_after)).kind
}

#[test]
fn a_graph_loading_conflict_is_retryable_after_the_server_delay() {
    assert_eq!(
        classified(
            StatusCode::CONFLICT,
            Some("GraphLoading"),
            Some(Duration::from_secs(2))
        ),
        FailureKind::Retryable {
            after: Duration::from_secs(2)
        }
    );
}

#[test]
fn a_loading_conflict_is_retryable_within_the_client_delay_bounds() {
    let after = |seconds| match classified(
        StatusCode::CONFLICT,
        Some("GraphLoading"),
        Some(Duration::from_secs(seconds)),
    ) {
        FailureKind::Retryable { after } => after,
        other => panic!("a loading conflict must be retryable: {other:?}"),
    };

    assert_eq!(after(0), Duration::from_secs(1));
    assert_eq!(after(3600), Duration::from_secs(5));
}

#[test]
fn a_file_loading_conflict_without_a_delay_is_retryable_after_one_second() {
    assert_eq!(
        classified(StatusCode::CONFLICT, Some("FileLoading"), None),
        FailureKind::Retryable {
            after: Duration::from_secs(1)
        }
    );
}

#[test]
fn a_conflict_without_a_loading_code_is_not_retryable() {
    let delay = Some(Duration::from_secs(2));
    assert_eq!(
        classified(StatusCode::CONFLICT, None, delay),
        FailureKind::Unavailable
    );
    assert_eq!(
        classified(StatusCode::CONFLICT, Some("StaleRevision"), delay),
        FailureKind::Unavailable
    );
}

#[test]
fn a_loading_code_on_another_status_is_not_retryable() {
    assert_eq!(
        classified(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("GraphLoading"),
            None
        ),
        FailureKind::Unavailable
    );
}

#[test]
fn a_gateway_failure_is_unavailable() {
    for status in [
        StatusCode::BAD_GATEWAY,
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::GATEWAY_TIMEOUT,
    ] {
        assert_eq!(classified(status, None, None), FailureKind::Unavailable);
    }
}

#[test]
fn a_rejected_credential_is_unavailable() {
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        assert_eq!(classified(status, None, None), FailureKind::Unavailable);
    }
}

#[test]
fn a_missing_or_invalid_request_is_an_invalid_argument() {
    for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::NOT_FOUND,
        StatusCode::UNPROCESSABLE_ENTITY,
    ] {
        assert_eq!(classified(status, None, None), FailureKind::InvalidArgument);
    }
}

#[test]
fn a_failure_keeps_its_class_through_context_layers() {
    let error = api_failure(StatusCode::NOT_FOUND, None, None).context("can't resolve codebase");

    let tool_error = ToolError::from_client("trace", &error);

    assert_eq!(tool_error.kind, FailureKind::InvalidArgument);
    assert!(
        tool_error
            .detail
            .starts_with("can't resolve codebase: GET https://example/v1/x -> 404")
    );
}

#[tokio::test]
async fn a_refused_connection_is_unavailable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    drop(listener);

    let error = reqwest::Client::new().get(&url).send().await.unwrap_err();
    assert!(error.is_connect());
    let error = anyhow::Error::new(error).context(format!("GET {url}"));

    assert_eq!(
        ToolError::from_client("trace", &error).kind,
        FailureKind::Unavailable
    );
}

#[test]
fn an_error_without_a_typed_cause_is_unavailable() {
    let error = anyhow::anyhow!("no codebase for this directory");

    let tool_error = ToolError::from_client("trace", &error);

    assert_eq!(tool_error.kind, FailureKind::Unavailable);
    assert_eq!(tool_error.detail, "no codebase for this directory");
}

#[test]
fn every_kind_renders_a_failure_line_and_one_next_step() {
    let cases = [
        (
            FailureKind::Retryable {
                after: Duration::from_secs(3),
            },
            "retry this call once after 3 s. If it fails again, use local Read/Grep for this \
             request.",
        ),
        (
            FailureKind::Unavailable,
            "use local Read/Grep for this request.",
        ),
        (
            FailureKind::InvalidArgument,
            "correct the argument and call again.",
        ),
        (
            FailureKind::IndexPending,
            "call sync_status to follow the first index, and use local Read/Grep until it \
             completes.",
        ),
        (
            FailureKind::IndexFailed,
            "call index_codebase for this path to retry the first index.",
        ),
        (
            FailureKind::Refused,
            "read the reason above, and do not repeat the same call unchanged.",
        ),
    ];
    for (kind, step) in cases {
        let error = ToolError::new("find_definition", kind, "the reason");
        assert_eq!(
            error.to_string(),
            format!("find_definition failed: the reason\nnext: {step}")
        );
    }
}

#[test]
fn a_retry_delay_below_one_second_rounds_up() {
    let error = ToolError::new(
        "trace",
        FailureKind::Retryable {
            after: Duration::from_millis(1),
        },
        "loading",
    );

    assert!(error.to_string().contains("after 1 s."));
}

#[test]
fn the_cli_error_is_the_summary_line_without_the_next_step() {
    let error = ToolError::new("grep", FailureKind::Unavailable, "the server is down");

    assert_eq!(error.summary(), "grep failed: the server is down");
    let cli_error = error.into_cli_error();
    assert_eq!(cli_error.to_string(), "grep failed: the server is down");
}

#[test]
fn a_tool_that_returns_an_error_gives_an_mcp_error_result() {
    let failed: Result<String, ToolError> = Err(ToolError::new(
        "trace",
        FailureKind::InvalidArgument,
        "bad symbol",
    ));
    let text = "trace failed: bad symbol\nnext: correct the argument and call again.";

    let result = failed.into_call_tool_result().unwrap();

    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.content.len(), 1);
    assert_eq!(result.content[0].as_text().unwrap().text, text);
}

#[test]
fn a_tool_that_returns_text_gives_an_mcp_success_result() {
    let succeeded: Result<String, ToolError> = Ok("no results".into());

    let result = succeeded.into_call_tool_result().unwrap();

    assert_eq!(result.is_error, Some(false));
    assert_eq!(result.content[0].as_text().unwrap().text, "no results");
}
