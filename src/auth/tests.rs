use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::refresh;

async fn token_server(
    replacement: Option<&str>,
    discover: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut responses = Vec::new();
    if discover {
        responses.push((
            "GET /.well-known/oauth-protected-resource ",
            "200 OK",
            serde_json::json!({ "authorization_servers": [&url] }),
        ));
    }
    let mut token = serde_json::json!({ "access_token": "refreshed-access", "expires_in": 3600 });
    if let Some(replacement) = replacement {
        token["refresh_token"] = replacement.into();
    }
    responses.push(("POST /connect/token ", "200 OK", token));
    (url, serve(listener, responses))
}

/// One token endpoint that refuses the grant with an OAuth error body.
async fn token_error_server(error: &str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let body = serde_json::json!({ "error": error, "error_description": "refused" });
    let responses = vec![("POST /connect/token ", "400 Bad Request", body)];
    (url, serve(listener, responses))
}

fn serve(
    listener: tokio::net::TcpListener,
    responses: Vec<(&'static str, &'static str, serde_json::Value)>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for (method, status, response) in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0, "request ended before the HTTP body");
                request.extend_from_slice(&buffer[..count]);
                if request_complete(&request) {
                    break;
                }
            }
            assert!(String::from_utf8_lossy(&request).starts_with(method));
            let body = response.to_string();
            let message = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(message.as_bytes()).await.unwrap();
        }
    })
}

fn request_complete(request: &[u8]) -> bool {
    let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
        return false;
    };
    let header = String::from_utf8_lossy(&request[..end]);
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    request.len() >= end + 4 + length
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn refresh_preserves_the_previous_refresh_token_when_omitted() {
    let (url, server) = token_server(None, false).await;
    let tokens = refresh(&http_client(), &url, "previous-refresh")
        .await
        .unwrap();
    assert_eq!(tokens.refresh_token.as_deref(), Some("previous-refresh"));
    assert_eq!(tokens.access_token, "refreshed-access");
    server.await.unwrap();
}

#[tokio::test]
async fn refresh_replaces_the_previous_refresh_token_when_rotated() {
    let (url, server) = token_server(Some("rotated-refresh"), false).await;
    let tokens = refresh(&http_client(), &url, "previous-refresh")
        .await
        .unwrap();
    assert_eq!(tokens.refresh_token.as_deref(), Some("rotated-refresh"));
    server.await.unwrap();
}

#[tokio::test]
async fn refresh_asks_for_a_new_login_when_identity_refuses_the_grant() {
    let (url, server) = token_error_server("invalid_grant").await;
    let error = refresh(&http_client(), &url, "revoked-refresh")
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("run `semctl auth login`"), "{error}");
    assert!(!error.contains("revoked-refresh"), "{error}");
    server.await.unwrap();
}

#[tokio::test]
async fn refresh_reports_other_token_errors_verbatim() {
    let (url, server) = token_error_server("invalid_client").await;
    let error = refresh(&http_client(), &url, "previous-refresh")
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.starts_with("refresh 400 Bad Request"), "{error}");
    assert!(error.contains("invalid_client"), "{error}");
    server.await.unwrap();
}
