//! Tests of the retry and deadline rules against a loopback stub server.
//!
//! The stub answers over real sockets, so these tests use the real clock and
//! small times. A paused clock would jump forward while a socket is idle.

use std::time::Duration;

use serde::Deserialize;
use tokio::time::Instant;

use super::{DEADLINE_MARGIN, GATEWAY_RETRY_DELAY, allows_wait, is_gateway_status, usable_until};
use crate::client::stub::{Reply, Stub};
use crate::client::{ApiFailure, Client};
use crate::query::ToolError;

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Probe {
    ok: bool,
}

const PROBE: &str = r#"{"ok":true}"#;

fn client(stub: &Stub) -> Client {
    Client::for_test_server(&stub.url)
}

/// A client whose requests must all end within `seconds` from now.
fn within(client: Client, seconds: f64) -> Client {
    client.with_deadline(Some(Instant::now() + Duration::from_secs_f64(seconds)))
}

fn failure(error: &anyhow::Error) -> &ApiFailure {
    error
        .downcast_ref::<ApiFailure>()
        .expect("the error must carry the typed failure")
}

#[test]
fn a_call_without_a_deadline_has_no_usable_time_limit() {
    let now = Instant::now();

    assert_eq!(usable_until(None, now), None);
    assert!(allows_wait(None, Duration::from_secs(3600)));
}

#[test]
fn the_usable_time_keeps_the_margin_in_reserve() {
    let now = Instant::now();

    assert_eq!(
        usable_until(Some(now + Duration::from_secs(10)), now),
        Some(Duration::from_millis(9500))
    );
    assert_eq!(
        usable_until(Some(now + DEADLINE_MARGIN), now),
        Some(Duration::ZERO)
    );
    assert_eq!(
        usable_until(Some(now), now + Duration::from_secs(1)),
        Some(Duration::ZERO),
        "a deadline in the past leaves nothing"
    );
}

#[test]
fn a_wait_must_leave_time_for_the_attempt_after_it() {
    let usable = Some(Duration::from_secs(2));

    assert!(allows_wait(usable, Duration::from_secs(1)));
    assert!(!allows_wait(usable, Duration::from_secs(2)));
    assert!(!allows_wait(Some(Duration::ZERO), Duration::ZERO));
}

#[test]
fn only_gateway_statuses_are_retried() {
    for code in [502_u16, 503, 504, 530] {
        let status = reqwest::StatusCode::from_u16(code).expect("a valid status");
        assert!(is_gateway_status(status), "{code}");
    }
    for code in [200_u16, 400, 404, 409, 500, 501] {
        let status = reqwest::StatusCode::from_u16(code).expect("a valid status");
        assert!(!is_gateway_status(status), "{code}");
    }
}

#[tokio::test]
async fn a_loading_answer_that_outlasts_the_deadline_returns_retryable_before_it() {
    let stub = Stub::serve(vec![Reply::loading("1")]).await;
    let client = within(client(&stub), 3.0);
    let started = Instant::now();

    let error = client
        .get::<Probe>("/v1/probe")
        .await
        .expect_err("the server never finishes loading");

    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the failure must arrive before the deadline: {:?}",
        started.elapsed()
    );
    assert!(failure(&error).is_loading());
    assert_eq!(failure(&error).retry_after(), Some(Duration::from_secs(1)));
    assert!(
        stub.requests() >= 2,
        "the call must retry while time remains: {} requests",
        stub.requests()
    );
    let text = ToolError::from_client("trace", &error).to_string();
    assert!(
        text.contains("retry this call once after 1 s"),
        "the typed failure must give the retry instruction: {text}"
    );
}

#[tokio::test]
async fn a_loading_answer_is_retried_until_the_server_is_ready_without_a_deadline() {
    let stub = Stub::serve(vec![Reply::loading("1"), Reply::ok(PROBE)]).await;

    let probe = client(&stub)
        .get::<Probe>("/v1/probe")
        .await
        .expect("the second answer succeeds");

    assert_eq!(probe, Probe { ok: true });
    assert_eq!(stub.requests(), 2);
}

#[tokio::test]
async fn a_gateway_failure_on_a_get_is_retried_once() {
    let stub = Stub::serve(vec![Reply::gateway("502 Bad Gateway"), Reply::ok(PROBE)]).await;

    let probe = client(&stub)
        .get::<Probe>("/v1/probe")
        .await
        .expect("the retry succeeds");

    assert_eq!(probe, Probe { ok: true });
    assert_eq!(stub.requests(), 2, "exactly one retry");
}

#[tokio::test]
async fn a_second_gateway_failure_is_reported_and_not_retried_again() {
    let stub = Stub::serve(vec![
        Reply::gateway("502 Bad Gateway"),
        Reply::gateway("504 Gateway Timeout"),
        Reply::ok(PROBE),
    ])
    .await;

    let error = client(&stub)
        .get::<Probe>("/v1/probe")
        .await
        .expect_err("two gateway failures end the call");

    assert_eq!(failure(&error).status().as_u16(), 504);
    assert_eq!(stub.requests(), 2);
}

#[tokio::test]
async fn every_gateway_status_gets_its_retry() {
    for status in [
        "503 Service Unavailable",
        "504 Gateway Timeout",
        "530 Origin Unreachable",
    ] {
        let stub = Stub::serve(vec![Reply::gateway(status), Reply::ok(PROBE)]).await;

        client(&stub)
            .get::<Probe>("/v1/probe")
            .await
            .unwrap_or_else(|error| panic!("{status} must be retried: {error:#}"));

        assert_eq!(stub.requests(), 2, "{status}");
    }
}

#[tokio::test]
async fn a_server_error_that_is_not_a_gateway_status_is_not_retried() {
    let stub = Stub::serve(vec![Reply::gateway("500 Internal Server Error")]).await;

    let error = client(&stub)
        .get::<Probe>("/v1/probe")
        .await
        .expect_err("a 500 is the server's own answer");

    assert_eq!(failure(&error).status().as_u16(), 500);
    assert_eq!(stub.requests(), 1);
}

#[tokio::test]
async fn a_gateway_failure_on_a_write_is_not_retried() {
    let stub = Stub::serve(vec![Reply::gateway("502 Bad Gateway"), Reply::ok(PROBE)]).await;
    let client = client(&stub);

    let post = client
        .post::<_, Probe>("/v1/write", &serde_json::json!({}))
        .await
        .expect_err("a write is not repeated");
    let put = client
        .put::<_, Probe>("/v1/write", &serde_json::json!({}))
        .await
        .expect("the second reply answers the second request");

    assert_eq!(failure(&post).status().as_u16(), 502);
    assert_eq!(put, Probe { ok: true });
    assert_eq!(
        stub.requests(),
        2,
        "one request for the failed POST and one for the PUT, with no retry between"
    );
}

#[tokio::test]
async fn a_failed_write_is_sent_exactly_once() {
    let stub = Stub::serve(vec![Reply::gateway("503 Service Unavailable")]).await;

    client(&stub)
        .put::<_, Probe>("/v1/upload", &serde_json::json!({}))
        .await
        .expect_err("a write is not repeated");

    assert_eq!(stub.request_lines(), ["PUT /v1/upload HTTP/1.1"]);
}

#[tokio::test]
async fn a_read_only_post_is_retried_like_a_get() {
    let stub = Stub::serve(vec![
        Reply::gateway("503 Service Unavailable"),
        Reply::ok(PROBE),
    ])
    .await;

    let probe = client(&stub)
        .post_read::<_, Probe>("/v1/search", &serde_json::json!({}))
        .await
        .expect("the retry succeeds");

    assert_eq!(probe, Probe { ok: true });
    assert_eq!(
        stub.request_lines(),
        ["POST /v1/search HTTP/1.1", "POST /v1/search HTTP/1.1"]
    );
}

#[tokio::test]
async fn a_gateway_retry_that_does_not_fit_the_deadline_is_not_taken() {
    let stub = Stub::serve(vec![Reply::gateway("502 Bad Gateway"), Reply::ok(PROBE)]).await;
    // After the margin, 0.7 s remain. The retry waits 1 s first.
    let deadline = DEADLINE_MARGIN + GATEWAY_RETRY_DELAY.mul_f64(0.7);
    let client = client(&stub).with_deadline(Some(Instant::now() + deadline));

    let error = client
        .get::<Probe>("/v1/probe")
        .await
        .expect_err("the retry would end after the deadline");

    assert_eq!(failure(&error).status().as_u16(), 502);
    assert_eq!(stub.requests(), 1);
}

/// A port that refuses connections now and can listen later. A socket that is
/// bound and does not listen refuses every connection, and the test keeps the
/// port, so no other test can take it between the two states.
fn refusing_port() -> (tokio::net::TcpSocket, std::net::SocketAddr) {
    let socket = tokio::net::TcpSocket::new_v4().expect("create a socket");
    socket
        .bind("127.0.0.1:0".parse().expect("a loopback address"))
        .expect("bind a loopback port");
    let address = socket.local_addr().expect("local address");
    (socket, address)
}

#[tokio::test]
async fn a_refused_connection_on_a_get_is_retried_once_after_a_pause() {
    let (socket, address) = refusing_port();
    // The server starts listening while the client waits to retry.
    let late = tokio::spawn(async move {
        tokio::time::sleep(GATEWAY_RETRY_DELAY / 2).await;
        let listener = socket.listen(16).expect("start listening");
        Stub::serve_on(listener, vec![Reply::ok(PROBE)])
    });
    let client = Client::for_test_server(&format!("http://{address}"));

    let probe = client
        .get::<Probe>("/v1/probe")
        .await
        .unwrap_or_else(|error| panic!("the retry must reach the server: {error:#}"));

    assert_eq!(probe, Probe { ok: true });
    assert_eq!(late.await.expect("the stub task").requests(), 1);
}

#[tokio::test]
async fn a_refused_connection_is_retried_only_for_a_request_that_reads() {
    let (_socket, address) = refusing_port();
    let client = Client::for_test_server(&format!("http://{address}"));

    let started_write = Instant::now();
    let write_error = client
        .put::<_, Probe>("/v1/upload", &serde_json::json!({}))
        .await
        .expect_err("nothing listens");
    let write = started_write.elapsed();
    let started_read = Instant::now();
    let read_error = client
        .get::<Probe>("/v1/probe")
        .await
        .expect_err("nothing listens");
    let read = started_read.elapsed();

    assert!(
        write < GATEWAY_RETRY_DELAY,
        "a write fails at once: {write:?}: {write_error:#}"
    );
    assert!(
        read >= GATEWAY_RETRY_DELAY,
        "a read pauses once and tries again: {read:?}: {read_error:#}"
    );
}

#[tokio::test]
async fn an_expired_deadline_fails_without_sending() {
    let stub = Stub::serve(vec![Reply::ok(PROBE)]).await;
    let client = client(&stub).with_deadline(Some(Instant::now() + DEADLINE_MARGIN));

    let error = client
        .get::<Probe>("/v1/probe")
        .await
        .expect_err("no time is left");

    assert!(
        format!("{error:#}").contains("the call deadline passed before the server answered"),
        "{error:#}"
    );
    assert_eq!(stub.requests(), 0, "nothing may be sent");
}

#[tokio::test]
async fn a_server_that_never_answers_fails_before_the_deadline_and_names_it() {
    let stub = Stub::serve(vec![Reply::Silent]).await;
    let client = within(client(&stub), 1.5);
    let started = Instant::now();

    let error = client
        .get::<Probe>("/v1/probe")
        .await
        .expect_err("the server is silent");

    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "the failure must arrive before the deadline: {:?}",
        started.elapsed()
    );
    let text = format!("{error:#}");
    assert!(
        text.contains("the call deadline passed before the server answered"),
        "{text}"
    );
    assert!(
        text.contains(&format!("GET {}/v1/probe", stub.url)),
        "{text}"
    );
    assert_eq!(stub.requests(), 1, "a timed-out read is not retried");
}
