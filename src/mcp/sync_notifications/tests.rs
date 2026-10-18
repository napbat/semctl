use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ServiceExt;
use serde_json::Value;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::task::{AbortHandle, JoinHandle};

use super::super::{McpServer, readiness::InitialIndexGate};
use super::METHOD;
use crate::client::Client;
use crate::engine::Engine;
use crate::engine::coordinator::IdleReconciler;
use crate::session::SessionContext;

struct Connection {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
    serving: JoinHandle<()>,
}

impl Connection {
    async fn connect(server: McpServer) -> Self {
        let (client, transport) = tokio::io::duplex(64 * 1024);
        let serving = tokio::spawn(async move {
            let _status = server.sync_status_session();
            let service = server
                .serve(transport)
                .await
                .expect("initialize the MCP service");
            service
                .waiting()
                .await
                .expect("serve until the client disconnects");
        });
        let (reader, mut writer) = tokio::io::split(client);
        let mut lines = BufReader::new(reader).lines();
        writer.write_all(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"status-test","version":"0"}}}"#,
            "\n",
        ).as_bytes()).await.expect("send initialize");
        let response = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("initialize must finish")
            .expect("read initialize")
            .expect("initialize response");
        let response: Value = serde_json::from_str(&response).expect("JSON initialize response");
        assert_eq!(response["id"], 1);
        assert!(response.get("result").is_some(), "{response}");
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .expect("send initialized");
        let reset = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("initialized must reset the session status")
            .expect("read reset")
            .expect("reset notification");
        let reset: Value = serde_json::from_str(&reset).expect("JSON reset notification");
        assert_eq!(reset["method"], METHOD);
        assert_eq!(
            reset["params"],
            serde_json::json!({"version": 1, "status": null})
        );
        Self {
            lines,
            writer,
            serving,
        }
    }

    async fn status(&mut self, root: &std::path::Path, phase: Value) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await
                    .expect("read status")
                    .expect("connected status stream");
                let event: Value = serde_json::from_str(&line).expect("JSON status notification");
                assert_eq!(event["method"], METHOD);
                assert_eq!(event["params"]["version"], 1);
                let status = &event["params"]["status"];
                assert_eq!(
                    status["root"],
                    serde_json::to_value(root).unwrap(),
                    "another checkout must not enter this session"
                );
                if status["first_index"] == phase {
                    return status.clone();
                }
            }
        })
        .await
        .expect("the phase transition must notify without a tool call")
    }

    async fn close(self) {
        let Self {
            lines,
            writer,
            serving,
        } = self;
        drop(lines);
        drop(writer);
        tokio::time::timeout(Duration::from_secs(5), serving)
            .await
            .expect("disconnect must end the service")
            .expect("service task");
    }
}

fn session(engine: &Arc<Engine>, root: &str, codebase: &str) -> (McpServer, Client, PathBuf) {
    let root = PathBuf::from(root);
    let client = Client::for_test(codebase, Some(root.clone()));
    let server = McpServer::with_parts(
        SessionContext::for_test(),
        client.clone(),
        root.clone(),
        false,
        engine.clone(),
    );
    (server, client, root)
}

async fn first_index(
    server: &McpServer,
    client: Client,
    root: &std::path::Path,
) -> Arc<InitialIndexGate> {
    server
        .watch_first_once(client, root.to_path_buf())
        .await
        .expect("lease a first index")
}

fn observer(server: &McpServer) -> AbortHandle {
    server
        .shared
        .sync_notifications
        .state
        .lock()
        .unwrap()
        .checkouts
        .values()
        .next()
        .expect("one leased checkout")
        .task
        .as_ref()
        .expect("a connected observer")
        .abort_handle()
}

#[tokio::test]
async fn clients_observe_only_their_leases_and_disconnect_stops_only_their_observer() {
    let engine = Engine::for_test(Arc::new(IdleReconciler));
    let (first, first_client, first_root) = session(&engine, "/status-first-checkout", "first");
    let (second, second_client, second_root) =
        session(&engine, "/status-second-checkout", "second");
    let first_gate = first_index(&first, first_client, &first_root).await;
    let second_gate = first_index(&second, second_client, &second_root).await;
    let mut first_connection = Connection::connect(first.clone()).await;
    let mut second_connection = Connection::connect(second.clone()).await;
    first_connection
        .status(&first_root, Value::from("registering"))
        .await;
    second_connection
        .status(&second_root, Value::from("registering"))
        .await;

    first_gate.register_codebase("first".into()).await;
    first_connection
        .status(&first_root, Value::from("syncing"))
        .await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(80),
            second_connection.lines.next_line()
        )
        .await
        .is_err(),
        "another session's progress must not notify this client"
    );

    let task = observer(&first);
    first_connection.close().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disconnect must stop the observer even with a retained handler clone");
    assert!(
        first
            .shared
            .sync_notifications
            .state
            .lock()
            .unwrap()
            .checkouts
            .is_empty()
    );

    second_gate.finish(Err("registration failed".into())).await;
    second_connection
        .status(
            &second_root,
            serde_json::json!({"failed":{"reason":"registration failed"}}),
        )
        .await;
    assert!(
        !observer(&second).is_finished(),
        "the other session must stay connected"
    );
    second_connection.close().await;
}

#[tokio::test]
async fn late_leases_emit_initial_status_and_gate_transitions_without_polling() {
    let engine = Engine::for_test(Arc::new(IdleReconciler));
    let (server, client, root) = session(&engine, "/status-late-checkout", "late");
    let mut connection = Connection::connect(server.clone()).await;
    let gate = first_index(&server, client, &root).await;
    connection.status(&root, Value::from("registering")).await;
    gate.register_codebase("late".into()).await;
    connection.status(&root, Value::from("syncing")).await;
    assert!(gate.claim_poll().await);
    connection
        .status(&root, serde_json::json!({"embedding":{"job_id":null}}))
        .await;
    gate.finish(Ok(())).await;
    connection.status(&root, Value::from("ready")).await;
    connection.close().await;
}
