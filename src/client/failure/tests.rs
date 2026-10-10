use std::fmt::Write as _;
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{ApiFailure, first_error_code, gateway_error, response_body_error, retry_after};
use crate::client::{unwrap_envelope, unwrap_page};

/// Answer one local request with a fixed HTTP response, and return what a
/// client reads from it. The test needs no network beyond the loopback.
async fn answered(
    status: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (reqwest::Response, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/probe", listener.local_addr().unwrap());
    let mut raw = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        write!(raw, "{name}: {value}\r\n").unwrap();
    }
    raw.push_str("\r\n");
    raw.push_str(body);
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 512];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0, "request ended before its head was complete");
            request.extend_from_slice(&buffer[..count]);
        }
        stream.write_all(raw.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    });
    let response = reqwest::Client::new().get(&url).send().await.unwrap();
    (response, url)
}

fn typed(error: &anyhow::Error) -> &ApiFailure {
    error
        .downcast_ref::<ApiFailure>()
        .expect("the error must carry the typed failure")
}

fn failure(status: StatusCode, code: Option<&str>) -> ApiFailure {
    ApiFailure::new(status, code.map(str::to_owned), None, String::new())
}

#[tokio::test]
async fn a_loading_conflict_carries_its_code_and_delay() {
    let body = r#"{"success":false,"errors":[{"code":"GraphLoading","message":"loading"}]}"#;
    let (response, url) = answered("409 Conflict", &[("Retry-After", "2")], body).await;

    let error = unwrap_envelope::<serde_json::Value>(response, "GET", &url)
        .await
        .unwrap_err();

    let failure = typed(&error);
    assert_eq!(failure.status, StatusCode::CONFLICT);
    assert_eq!(failure.code.as_deref(), Some("GraphLoading"));
    assert_eq!(failure.retry_after, Some(Duration::from_secs(2)));
    assert!(failure.is_loading());
    assert_eq!(
        error.to_string(),
        format!("GET {url} -> 409 Conflict: {{\"code\":\"GraphLoading\",\"message\":\"loading\"}}"),
        "the human text keeps its wording"
    );
}

#[tokio::test]
async fn a_conflict_without_a_typed_code_is_not_a_loading_state() {
    let (response, url) = answered(
        "409 Conflict",
        &[],
        r#"{"success":false,"errors":[{"message":"stale"}]}"#,
    )
    .await;

    let error = unwrap_envelope::<serde_json::Value>(response, "GET", &url)
        .await
        .unwrap_err();

    let failure = typed(&error);
    assert_eq!(failure.status, StatusCode::CONFLICT);
    assert_eq!(failure.code, None);
    assert_eq!(failure.retry_after, None);
    assert!(!failure.is_loading());
}

#[tokio::test]
async fn a_paged_failure_is_typed_like_an_enveloped_failure() {
    let body = r#"{"success":false,"errors":[{"code":"FileLoading"}],"data":[],"page":0,"pageSize":25,"total":0}"#;
    let (response, url) = answered("409 Conflict", &[("Retry-After", "3600")], body).await;

    let error = unwrap_page::<serde_json::Value>(response, "GET", &url)
        .await
        .unwrap_err();

    let failure = typed(&error);
    assert_eq!(failure.code.as_deref(), Some("FileLoading"));
    assert_eq!(
        failure.retry_after,
        Some(Duration::from_secs(3600)),
        "the failure keeps the delay as the response sent it"
    );
    assert!(failure.is_loading());
}

#[tokio::test]
async fn a_failure_of_any_status_keeps_its_retry_after_delay() {
    let body = r#"{"success":false,"errors":[{"message":"busy"}]}"#;
    let (response, url) = answered("503 Service Unavailable", &[("Retry-After", "30")], body).await;

    let error = unwrap_envelope::<serde_json::Value>(response, "GET", &url)
        .await
        .unwrap_err();

    let failure = typed(&error);
    assert_eq!(failure.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(failure.retry_after(), Some(Duration::from_secs(30)));
    assert!(!failure.is_loading());
}

#[tokio::test]
async fn a_gateway_page_keeps_its_retry_after_delay() {
    let (response, url) = answered(
        "503 Service Unavailable",
        &[("Retry-After", "7")],
        "<html><body>503</body></html>",
    )
    .await;

    let error = unwrap_envelope::<serde_json::Value>(response, "GET", &url)
        .await
        .unwrap_err();

    let failure = typed(&error);
    assert_eq!(failure.code, None);
    assert_eq!(failure.retry_after(), Some(Duration::from_secs(7)));
}

#[test]
fn a_loading_delay_stays_within_the_client_retry_bounds() {
    let delayed = |seconds: Option<u64>| {
        ApiFailure::new(
            StatusCode::CONFLICT,
            Some("GraphLoading".to_owned()),
            seconds.map(Duration::from_secs),
            String::new(),
        )
        .loading_delay()
    };

    assert_eq!(delayed(None), Duration::from_secs(1));
    assert_eq!(delayed(Some(0)), Duration::from_secs(1));
    assert_eq!(delayed(Some(3)), Duration::from_secs(3));
    assert_eq!(delayed(Some(3600)), Duration::from_secs(5));
}

#[test]
fn a_retry_after_header_is_read_as_delta_seconds_only() {
    let read = |value: &str| {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, value.parse().unwrap());
        retry_after(&headers)
    };

    assert_eq!(read("12"), Some(Duration::from_secs(12)));
    assert_eq!(read(" 12 "), Some(Duration::from_secs(12)));
    assert_eq!(read("Wed, 21 Oct 2026 07:28:00 GMT"), None);
    assert_eq!(read("invalid"), None);
    assert_eq!(retry_after(&reqwest::header::HeaderMap::new()), None);
}

#[tokio::test]
async fn a_gateway_page_is_a_typed_failure_without_a_code() {
    let (response, url) = answered(
        "502 Bad Gateway",
        &[],
        "<html><body>502 Bad Gateway</body></html>",
    )
    .await;

    let error = unwrap_envelope::<serde_json::Value>(response, "GET", &url)
        .await
        .unwrap_err();

    let failure = typed(&error);
    assert_eq!(failure.status, StatusCode::BAD_GATEWAY);
    assert_eq!(failure.code, None);
    assert!(
        failure
            .message
            .contains("the gateway could not reach the server")
    );
}

#[test]
fn a_structured_denial_keeps_its_code() {
    let url = "https://example/v1/x";
    let enveloped = response_body_error(
        "GET",
        url,
        StatusCode::FORBIDDEN,
        r#"{"success":false,"errors":[{"code":"TenantBindingDenied"}]}"#,
    );
    let bare = response_body_error(
        "GET",
        url,
        StatusCode::FORBIDDEN,
        r#"{"code":"TenantBindingDenied","message":"denied"}"#,
    );
    let page = response_body_error("GET", url, StatusCode::FORBIDDEN, "<html>forbidden</html>");

    assert_eq!(
        typed(&enveloped).code.as_deref(),
        Some("TenantBindingDenied")
    );
    assert_eq!(typed(&bare).code.as_deref(), Some("TenantBindingDenied"));
    assert_eq!(typed(&page).code, None);
    assert_eq!(typed(&page).status, StatusCode::FORBIDDEN);
}

#[test]
fn a_gateway_error_names_its_status_in_the_typed_failure() {
    let error = gateway_error(
        "PUT",
        "https://example/v1/x",
        StatusCode::GATEWAY_TIMEOUT,
        None,
        "",
    );

    assert_eq!(typed(&error).status, StatusCode::GATEWAY_TIMEOUT);
    assert!(typed(&error).message.contains("gateway timed out"));
}

#[test]
fn the_first_error_code_skips_entries_without_a_string_code() {
    let errors = [
        json!("plain text"),
        json!({ "message": "no code" }),
        json!({ "code": 7 }),
        json!({ "Code": "Second" }),
        json!({ "code": "Third" }),
    ];

    assert_eq!(first_error_code(&errors).as_deref(), Some("Second"));
    assert_eq!(first_error_code(&[]), None);
}

#[test]
fn only_a_conflict_with_a_loading_code_is_a_loading_state() {
    assert!(failure(StatusCode::CONFLICT, Some("GraphLoading")).is_loading());
    assert!(failure(StatusCode::CONFLICT, Some("fileloading")).is_loading());
    assert!(!failure(StatusCode::CONFLICT, Some("Other")).is_loading());
    assert!(!failure(StatusCode::CONFLICT, None).is_loading());
    assert!(!failure(StatusCode::BAD_GATEWAY, Some("GraphLoading")).is_loading());
}
