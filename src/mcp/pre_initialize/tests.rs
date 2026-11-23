use std::path::PathBuf;
use std::sync::Arc;

use rmcp::service::ServerInitializeError;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};

use crate::client::Client;
use crate::engine::Engine;
use crate::engine::coordinator::IdleReconciler;
use crate::mcp::McpServer;
use crate::session::SessionContext;

fn server() -> McpServer {
    McpServer::with_parts(
        SessionContext::for_test(),
        Client::for_test_server("http://127.0.0.1:1"),
        PathBuf::from("launch"),
        true,
        Engine::for_test(Arc::new(IdleReconciler)),
    )
}

/// An MCP host over an in-memory pipe, connected to a live session.
struct Host {
    write: WriteHalf<DuplexStream>,
    read: Lines<BufReader<ReadHalf<DuplexStream>>>,
}

impl Host {
    fn connect() -> Self {
        let server = server();
        let (host_end, server_end) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let running = server
                .serve_session(server_end)
                .await
                .expect("serve the session");
            let _ = running.waiting().await;
        });
        let (read, write) = tokio::io::split(host_end);
        Self {
            write,
            read: BufReader::new(read).lines(),
        }
    }

    async fn send_line(&mut self, line: &str) {
        self.write
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write to the server");
    }

    async fn send(&mut self, message: &Value) {
        self.send_line(&message.to_string()).await;
    }

    async fn next(&mut self) -> Value {
        let line = self
            .read
            .next_line()
            .await
            .expect("read from the server")
            .expect("the server closed the session");
        serde_json::from_str(&line).expect("a JSON-RPC line")
    }

    /// Send a request and return its answer. The server can send notifications
    /// first once the session is initialized; they are skipped.
    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let answer = self.next().await;
            if answer["id"] == id {
                return answer;
            }
            assert!(
                answer.get("id").is_none() && answer.get("method").is_some(),
                "only a notification may come before the answer: {answer}"
            );
        }
    }

    async fn initialize(&mut self, id: u64) {
        let answer = self
            .request(
                id,
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "test-host", "version": "0"},
                }),
            )
            .await;
        assert!(answer["result"]["serverInfo"].is_object(), "{answer}");
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
    }

    async fn assert_tools_listed(&mut self, id: u64) {
        let answer = self.request(id, "tools/list", json!({})).await;
        let tools = answer["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools/list returns tools: {answer}"));
        assert!(
            tools.iter().any(|tool| tool["name"] == "search_codebase"),
            "{answer}"
        );
    }
}

/// oh-my-pi 19.1 sends `server/discover` before `initialize`. Without the
/// pre-initialize transport, `rmcp` ends the session at that request.
#[tokio::test]
async fn a_request_before_initialize_gets_method_not_found_and_the_session_continues() {
    let mut host = Host::connect();

    let answer = host.request(1, "server/discover", json!({})).await;
    assert_eq!(answer["error"]["code"], -32601, "{answer}");
    assert!(answer.get("result").is_none(), "{answer}");

    host.initialize(2).await;
    host.assert_tools_listed(3).await;
}

#[tokio::test]
async fn a_session_that_starts_with_initialize_is_unchanged() {
    let mut host = Host::connect();

    host.initialize(1).await;
    host.assert_tools_listed(2).await;
}

#[tokio::test]
async fn a_notification_before_initialize_is_dropped_without_an_answer() {
    let mut host = Host::connect();

    host.send(&json!({"jsonrpc": "2.0", "method": "notifications/probe"}))
        .await;
    // An answer to the notification would carry an `id` and fail `request`.
    host.initialize(1).await;
    host.assert_tools_listed(2).await;
}

#[tokio::test]
async fn a_malformed_line_before_initialize_gets_a_parse_error_and_the_session_continues() {
    let mut host = Host::connect();

    host.send_line("{not json").await;
    let answer = host.next().await;
    assert_eq!(answer["error"]["code"], -32700, "{answer}");

    host.initialize(1).await;
    host.assert_tools_listed(2).await;
}

#[tokio::test]
async fn the_end_of_the_stream_before_initialize_ends_the_session() {
    let (host_end, server_end) = tokio::io::duplex(1 << 16);
    drop(host_end);

    let error = server()
        .serve_session(server_end)
        .await
        .err()
        .expect("the session cannot start without initialize");

    assert!(
        matches!(*error, ServerInitializeError::ConnectionClosed(_)),
        "{error:?}"
    );
}
