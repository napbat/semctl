//! Tests of the call budget: its choice per tool, its waits, and the way
//! `call_tool` applies it to a call that reaches the server through MCP.
//!
//! The end-to-end tests drive the real handler through an in-memory pipe, so
//! they cover the `Extension<CallBudget>` extractor and the timeout in
//! `call_tool` together. A test that needs a socket uses the real clock. The
//! others run on a paused clock and take no real time.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ServiceExt;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::time::Instant;

use super::{CallBudget, call_budget, deadline_result, tool_op};
use crate::client::Client;
use crate::client::stub::{Reply, Stub};
use crate::engine::Engine;
use crate::engine::coordinator::IdleReconciler;
use crate::mcp::{DIRECT_EDIT_TOOLS, McpServer, tools};
use crate::session::SessionContext;

/// The clock rounds a timer up to the next millisecond.
const TIMER_SLACK: Duration = Duration::from_millis(50);

const DEADLINE: Duration = Duration::from_secs(25);

#[test]
fn every_edit_tool_gets_an_unbounded_budget() {
    let now = Instant::now();

    for tool in DIRECT_EDIT_TOOLS {
        assert_eq!(call_budget(tool, now, DEADLINE).deadline(), None, "{tool}");
    }
}

#[test]
fn every_other_registered_tool_gets_the_call_deadline() {
    let router = tools::router();
    let now = Instant::now();

    let mut bounded = 0;
    for tool in router.list_all() {
        if DIRECT_EDIT_TOOLS.contains(&tool.name.as_ref()) {
            continue;
        }
        let budget = call_budget(&tool.name, now, DEADLINE);
        assert_eq!(budget.deadline(), Some(now + DEADLINE), "{}", tool.name);
        bounded += 1;
    }
    assert!(bounded > 0, "the router must register read tools");
}

#[test]
fn the_indexing_tool_is_bounded_like_a_read_tool() {
    let now = Instant::now();

    assert_eq!(
        call_budget("index_codebase", now, DEADLINE).deadline(),
        Some(now + DEADLINE)
    );
}

/// A misspelled name in the edit list would give an edit tool a deadline.
#[test]
fn the_edit_list_names_only_registered_tools() {
    let router = tools::router();

    for tool in DIRECT_EDIT_TOOLS {
        assert!(
            router.get(tool).is_some(),
            "{tool} is not a registered tool"
        );
    }
}

#[test]
fn every_registered_tool_resolves_to_its_own_static_name() {
    let router = tools::router();

    for tool in router.list_all() {
        assert_eq!(tool_op(&router, &tool.name), tool.name.as_ref());
    }
    assert_eq!(tool_op(&router, "no_such_tool"), "tool call");
}

#[tokio::test(start_paused = true)]
async fn a_bounded_wait_ends_at_its_limit_or_before_the_deadline() {
    let budget = CallBudget::until(Instant::now() + DEADLINE);
    let start = Instant::now();
    assert_eq!(
        budget.wait_until(Duration::from_secs(5)),
        Some(start + Duration::from_secs(5))
    );
    assert_eq!(budget.remaining(), Some(DEADLINE));

    // Three seconds remain. The wait stops half a second before the deadline.
    tokio::time::advance(Duration::from_secs(22)).await;
    let now = Instant::now();
    assert_eq!(budget.remaining(), Some(Duration::from_secs(3)));
    assert_eq!(
        budget.wait_until(Duration::from_secs(5)),
        Some(now + Duration::from_millis(2500))
    );

    // Past the deadline there is nothing left to wait.
    tokio::time::advance(Duration::from_secs(60)).await;
    let now = Instant::now();
    assert_eq!(budget.remaining(), Some(Duration::ZERO));
    assert_eq!(budget.wait_until(Duration::from_secs(5)), Some(now));
}

#[tokio::test(start_paused = true)]
async fn an_unbounded_budget_has_no_deadline_and_no_wait_limit() {
    let budget = CallBudget::unbounded();

    assert_eq!(budget.deadline(), None);
    assert_eq!(budget.remaining(), None);
    assert_eq!(budget.wait_until(Duration::from_secs(5)), None);
}

#[tokio::test(start_paused = true)]
async fn the_backstop_drops_a_call_at_the_deadline() {
    let budget = CallBudget::until(Instant::now() + DEADLINE);
    let started = Instant::now();

    let outcome = budget.run(std::future::pending::<()>()).await;

    assert!(outcome.is_err());
    let elapsed = started.elapsed();
    assert!(
        elapsed >= DEADLINE && elapsed <= DEADLINE + TIMER_SLACK,
        "{elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unbounded_call_is_never_dropped() {
    let slow = async {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        7
    };

    let outcome = CallBudget::unbounded().run(slow).await;

    assert_eq!(outcome.ok(), Some(7));
}

#[test]
fn the_deadline_result_is_an_error_result_that_names_the_deadline() {
    let result = deadline_result("find_definition", DEADLINE);

    let value = serde_json::to_value(&result).expect("serialize the result");
    assert_eq!(value["isError"], true);
    assert_eq!(
        text_of(&value),
        "find_definition failed: no answer within the 25 s call deadline\n\
         next: use local Read/Grep for this request."
    );
}

/// The text of the first content block of a `CallToolResult`.
fn text_of(result: &Value) -> &str {
    result["content"][0]["text"]
        .as_str()
        .expect("the result carries text")
}

/// A server with no codebase binding, for one session whose call deadline is
/// `tool_deadline`.
fn server_with(base: Client, tool_deadline: Duration) -> McpServer {
    let mut context = SessionContext::for_test();
    context.tool_deadline = tool_deadline;
    McpServer::with_parts(
        context,
        base,
        PathBuf::from("launch"),
        true,
        Engine::for_test(Arc::new(IdleReconciler)),
    )
}

/// An MCP host over an in-memory pipe, speaking JSON-RPC lines to a server.
struct Host {
    write: WriteHalf<DuplexStream>,
    read: Lines<BufReader<ReadHalf<DuplexStream>>>,
    next_id: u64,
}

impl Host {
    /// Start `server` and complete the MCP handshake.
    async fn connect(server: McpServer) -> Self {
        let (host_end, server_end) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let running = server.serve(server_end).await.expect("serve the session");
            let _ = running.waiting().await;
        });
        let (read, write) = tokio::io::split(host_end);
        let mut host = Self {
            write,
            read: BufReader::new(read).lines(),
            next_id: 0,
        };
        let id = host
            .start(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "test-host", "version": "0"},
                }),
            )
            .await;
        host.response(id).await;
        host.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        host
    }

    async fn send(&mut self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.write
            .write_all(line.as_bytes())
            .await
            .expect("write to the server");
    }

    /// Send a request and return its id without waiting for the answer.
    async fn start(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    /// Read until the answer to request `id` arrives. Return its `result`.
    async fn response(&mut self, id: u64) -> Value {
        loop {
            let line = self
                .read
                .next_line()
                .await
                .expect("read from the server")
                .expect("the server closed before it answered");
            let message: Value = serde_json::from_str(&line).expect("a JSON-RPC line");
            if message["id"] == id {
                assert!(message.get("error").is_none(), "{message}");
                return message["result"].clone();
            }
        }
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let id = self
            .start("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        self.response(id).await
    }
}

#[tokio::test]
async fn a_read_tool_that_meets_a_server_still_loading_fails_with_a_retry_before_the_deadline() {
    let stub = Stub::serve(vec![Reply::loading("1")]).await;
    let server = server_with(Client::for_test_server(&stub.url), Duration::from_secs(3));
    let mut host = Host::connect(server).await;
    let started = Instant::now();

    let result = host.call_tool("list_domains", json!({})).await;

    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the failure must arrive before the deadline: {:?}",
        started.elapsed()
    );
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(text.starts_with("list_domains failed: "), "{text}");
    assert!(
        text.ends_with(
            "next: retry this call once after 1 s. If it fails again, use local Read/Grep for \
             this request."
        ),
        "{text}"
    );
}

/// A call that waits on state of its own, with no request in flight, has only
/// the backstop to end it.
#[tokio::test(start_paused = true)]
async fn a_call_that_no_request_bounds_ends_at_the_deadline_with_an_error_result() {
    let server = server_with(Client::for_test("codebase", None), DEADLINE);
    // A bind in progress holds this lock, and a later call queues behind it.
    let held = server.shared.bound.lock().await;
    let mut host = Host::connect(server.clone()).await;
    let started = Instant::now();

    // Without the backstop this call never ends, so the test bounds its own wait.
    let result = tokio::time::timeout(
        DEADLINE * 2,
        host.call_tool("find_definition", json!({"symbol": "main"})),
    )
    .await
    .expect("the call must end at its deadline");

    let elapsed = started.elapsed();
    assert!(
        elapsed >= DEADLINE && elapsed <= DEADLINE + TIMER_SLACK,
        "{elapsed:?}"
    );
    assert_eq!(result["isError"], true);
    assert_eq!(
        text_of(&result),
        "find_definition failed: no answer within the 25 s call deadline\n\
         next: use local Read/Grep for this request."
    );
    drop(held);
}

#[tokio::test(start_paused = true)]
async fn an_edit_tool_is_never_cut_off_by_the_call_deadline() {
    let server = server_with(Client::for_test("codebase", None), DEADLINE);
    let held = server.shared.bound.lock().await;
    let mut host = Host::connect(server.clone()).await;
    let id = host
        .start(
            "tools/call",
            json!({"name": "undo_edit", "arguments": {"edit_id": "e"}}),
        )
        .await;

    // An hour is far past the call deadline. A bounded tool would have answered.
    let answered = tokio::time::timeout(Duration::from_secs(3600), host.response(id)).await;

    assert!(
        answered.is_err(),
        "an edit tool must wait for its own work, not for a deadline"
    );
    drop(held);
}

/// A coordinator keeps the client it is given for the life of the session. A
/// deadline on that client would fail every request after the first call ended.
#[tokio::test(start_paused = true)]
async fn the_binding_a_server_keeps_never_carries_a_call_deadline() {
    let server = server_with(Client::for_test("codebase", None), DEADLINE);
    let budget = CallBudget::until(Instant::now() + DEADLINE);

    let client = server
        .bound_unchecked("find_definition", &budget)
        .await
        .expect("the pinned binding resolves");

    assert_eq!(client.deadline(), budget.deadline());
    let kept = server
        .shared
        .bound
        .lock()
        .await
        .clone()
        .expect("the server keeps the binding");
    assert_eq!(kept.deadline(), None);
    let unbounded = CallBudget::unbounded();
    let later = server
        .bound_unchecked("rename_symbol", &unbounded)
        .await
        .expect("the kept binding is reused");
    assert_eq!(
        later.deadline(),
        None,
        "no deadline carries over to a later call"
    );
}
