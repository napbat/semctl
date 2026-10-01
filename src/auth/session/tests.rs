use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

use super::*;
use crate::config;

fn store() -> (tempfile::TempDir, CredentialStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = CredentialStore {
        state: config::StateStore {
            current: directory.path().join("semctl"),
            legacy: directory.path().join("semctx"),
        },
    };
    (directory, store)
}

fn tokens(expired: bool) -> TokenSet {
    TokenSet {
        access_token: "fake-access".into(),
        refresh_token: Some("fake-refresh".into()),
        expires_at_unix: if expired { 0 } else { u64::MAX },
    }
}

fn login(store: &CredentialStore, server: &str, expired: bool) -> AuthenticatedSession {
    let _lock = store.state.lock_blocking().unwrap();
    publish_login(
        store,
        LoginAttempt {
            server_url: server.into(),
            generation: store.state.generation().unwrap(),
        },
        server.into(),
        tokens(expired),
    )
    .unwrap()
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
}

/// Serve one refresh request. The channels hold its response while another
/// operation contends for the same state lock.
async fn refresh_server() -> (
    String,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (seen, received) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert!(request.starts_with("POST /connect/token "));
        assert!(request.contains("refresh_token=fake-refresh"));
        seen.send(()).unwrap();
        released.await.unwrap();
        respond(&mut stream, serde_json::json!({"access_token":"new-access", "refresh_token":"new-refresh", "expires_in":3600})).await;
    });
    (url, received, release, task)
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut request = Vec::new();
    loop {
        let mut buffer = [0_u8; 1024];
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0);
        request.extend_from_slice(&buffer[..read]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                return String::from_utf8(request).unwrap();
            }
        }
    }
}

async fn respond(stream: &mut tokio::net::TcpStream, value: serde_json::Value) {
    let body = value.to_string();
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

#[tokio::test]
async fn concurrent_requests_share_refresh_and_keep_the_login_generation() {
    let (_directory, store) = store();
    let (url, received, release, server) = refresh_server().await;
    let session = login(&store, &url, true);
    let store = Arc::new(store);
    let task_store = store.clone();
    let task_url = url.clone();
    let first =
        tokio::spawn(async move { valid_stored_session(&http(), &task_store, &task_url).await });
    received.await.unwrap();
    let task_store = store.clone();
    let second =
        tokio::spawn(async move { valid_stored_session(&http(), &task_store, &url).await });
    release.send(()).unwrap();
    assert_eq!(first.await.unwrap().unwrap().access_token, "new-access");
    assert_eq!(second.await.unwrap().unwrap().access_token, "new-access");
    let stored = store.load().unwrap().unwrap();
    assert_eq!(stored.tokens.refresh_token.as_deref(), Some("new-refresh"));
    assert_eq!(
        stored.session.unwrap().stamp.generation,
        session.stamp.generation
    );
    server.await.unwrap();
}

#[tokio::test]
async fn waiting_client_rejects_a_replacement_login_for_another_server() {
    let (_directory, store) = store();
    login(&store, "http://127.0.0.1:1", true);
    let lock = store.state.lock().await.unwrap();
    let store = Arc::new(store);
    let waiting_store = store.clone();
    let mut waiting = tokio::spawn(async move {
        valid_stored_session(&http(), &waiting_store, "http://127.0.0.1:1").await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut waiting)
            .await
            .is_err()
    );
    publish_login(
        &store,
        LoginAttempt {
            server_url: "http://127.0.0.1:2".into(),
            generation: store.state.generation().unwrap(),
        },
        "http://127.0.0.1:2".into(),
        tokens(true),
    )
    .unwrap();
    drop(lock);
    let error = waiting.await.unwrap().err().unwrap().to_string();
    assert!(error.contains("another server"), "{error}");
    assert_eq!(
        store
            .load()
            .unwrap()
            .unwrap()
            .tokens
            .refresh_token
            .as_deref(),
        Some("fake-refresh")
    );
}

#[test]
fn delayed_tenant_selection_and_clear_cannot_change_a_newer_login() {
    let (_directory, store) = store();
    let first = login(&store, "http://127.0.0.1:1", false);
    let second = login(&store, "http://127.0.0.1:2", false);
    let _lock = store.state.lock_blocking().unwrap();
    assert!(set_tenant(&store, &second.stamp, None, Some("second-tenant".into())).unwrap());
    assert!(!set_tenant(&store, &first.stamp, None, Some("first-tenant".into())).unwrap());
    assert!(!set_tenant(&store, &first.stamp, None, None).unwrap());
    assert_eq!(
        store.state.load_config().unwrap().active_tenant.as_deref(),
        Some("second-tenant")
    );
}

#[tokio::test]
async fn canceled_refresh_releases_the_state_lock() {
    let (_directory, store) = store();
    let (url, received, _release, server) = refresh_server().await;
    login(&store, &url, true);
    let store = Arc::new(store);
    let reader = store.clone();
    let refreshing =
        tokio::spawn(async move { valid_stored_session(&http(), &reader, &url).await });
    received.await.unwrap();
    refreshing.abort();
    assert!(refreshing.await.err().unwrap().is_cancelled());
    let _lock = tokio::time::timeout(Duration::from_secs(1), store.state.lock())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .load()
            .unwrap()
            .unwrap()
            .tokens
            .refresh_token
            .as_deref(),
        Some("fake-refresh")
    );
    server.abort();
}

#[tokio::test]
async fn refresh_body_timeout_releases_the_state_lock() {
    let (_directory, store) = store();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    login(&store, &url, true);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{").await.unwrap();
        std::future::pending::<()>().await;
    });
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(20))
        .build()
        .unwrap();
    let result = valid_stored_session(&http, &store, &url).await;
    assert!(format!("{:#}", result.err().unwrap()).contains("timed out"));
    let _lock = tokio::time::timeout(Duration::from_secs(1), store.state.lock())
        .await
        .unwrap()
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn purge_waits_for_refresh_and_credentials_do_not_reappear() {
    let (_directory, store) = store();
    let (url, received, release, server) = refresh_server().await;
    login(&store, &url, true);
    let store = Arc::new(store);
    let reader = store.clone();
    let refresh = tokio::spawn(async move { valid_stored_session(&http(), &reader, &url).await });
    received.await.unwrap();
    let purging = store.clone();
    let mut purge = tokio::task::spawn_blocking(move || purging.state.purge());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut purge)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    refresh.await.unwrap().unwrap();
    assert!(purge.await.unwrap().unwrap());
    server.await.unwrap();
    assert!(store.load().unwrap().is_none());
    assert!(!store.state.current.exists());
    assert_eq!(store.state.generation().unwrap(), 2);
}

#[test]
fn purge_invalidates_an_unfinished_device_login_and_config_update() {
    let (_directory, store) = store();
    let generation = store.state.generation().unwrap();
    store.state.purge().unwrap();
    let lock = store.state.lock_blocking().unwrap();
    assert!(
        publish_login(
            &store,
            LoginAttempt {
                server_url: "http://127.0.0.1:1".into(),
                generation
            },
            "http://127.0.0.1:1".into(),
            tokens(false)
        )
        .is_err()
    );
    drop(lock);
    assert!(
        store
            .state
            .update_config(generation, |cfg| cfg.active_tenant = Some("old".into()))
            .is_err()
    );
    assert!(!store.state.current.exists());
}

#[tokio::test]
async fn interrupted_login_publication_rejects_old_credentials() {
    let (_directory, store) = store();
    login(&store, "http://127.0.0.1:1", false);
    store.state.advance().unwrap();
    let error = valid_stored_session(&http(), &store, "http://127.0.0.1:1")
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("incomplete or was cleared"));
}

#[tokio::test]
async fn legacy_credentials_migrate_only_when_the_issuer_matches() {
    for matches in [true, false] {
        let (_directory, store) = store();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let authority = url.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(
                read_request(&mut stream)
                    .await
                    .starts_with("GET /.well-known/oauth-protected-resource ")
            );
            respond(
                &mut stream,
                serde_json::json!({"authorization_servers":[authority]}),
            )
            .await;
        });
        store
            .state
            .save_config(&config::Config {
                server_url: Some(url.clone()),
                ..config::Config::default()
            })
            .unwrap();
        let mut token = tokens(false);
        let issuer = if matches {
            url.as_str()
        } else {
            "http://127.0.0.1:1"
        };
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::json!({"iss":issuer}).to_string());
        token.access_token = format!("header.{payload}.signature");
        config::atomic_write_private(
            &store.state.legacy.join("credentials.json"),
            &serde_json::to_vec(&token).unwrap(),
        )
        .unwrap();
        let result = valid_stored_session(&http(), &store, &url).await;
        assert_eq!(result.is_ok(), matches);
        assert_eq!(
            store.state.current.join("credentials.json").exists(),
            matches
        );
        server.await.unwrap();
    }
}

#[test]
fn logout_removes_legacy_credentials_and_invalidates_retained_state() {
    let (_directory, store) = store();
    login(&store, "http://127.0.0.1:1", false);
    config::atomic_write_private(
        &store.state.legacy.join("credentials.json"),
        &serde_json::to_vec(&tokens(false)).unwrap(),
    )
    .unwrap();
    let _lock = store.state.lock_blocking().unwrap();
    store.state.advance().unwrap();
    store.clear().unwrap();
    assert!(store.load().unwrap().is_none());
    assert_eq!(store.state.generation().unwrap(), 2);
}
