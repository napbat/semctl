use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout, Command};

async fn send(stdin: &mut ChildStdin, message: Value) {
    let mut bytes = serde_json::to_vec(&message).expect("serialize MCP message");
    bytes.push(b'\n');
    stdin.write_all(&bytes).await.expect("write MCP message");
}

async fn receive(stdout: &mut Lines<BufReader<ChildStdout>>) -> Value {
    let line = tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
        .await
        .expect("MCP response timed out")
        .expect("read MCP response")
        .expect("MCP server closed stdout before responding");
    serde_json::from_str(&line).expect("parse MCP response")
}

#[tokio::test]
async fn mcp_exposes_display_metadata_and_preserves_tool_error_text() {
    let directory = tempfile::tempdir().expect("create isolated MCP directory");
    // A pinned codebase and disabled update check keep metadata discovery offline.
    let mut child = Command::new(env!("CARGO_BIN_EXE_semctl"))
        .args([
            "--server",
            "http://127.0.0.1:9",
            "--codebase",
            "metadata-test",
            "mcp",
        ])
        .current_dir(directory.path())
        .env("XDG_CONFIG_HOME", directory.path())
        // This test measures the server in this process. Pin the role so the
        // default of `SEMCTX_MCP_DAEMON` cannot move it into a daemon.
        .env("SEMCTX_MCP_DAEMON", "off")
        .env("SEMCTX_MCP_UPDATE_CHECK", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start MCP server");
    let mut stdin = child.stdin.take().expect("piped MCP stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped MCP stdout")).lines();

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": rmcp::model::ProtocolVersion::default(),
                "capabilities": {},
                "clientInfo": { "name": "metadata-test", "version": "1" }
            }
        }),
    )
    .await;
    let initialized = receive(&mut stdout).await;
    assert_eq!(initialized["id"], 1);
    assert!(initialized["result"]["capabilities"]["tools"].is_object());
    let server_info = &initialized["result"]["serverInfo"];
    assert_eq!(server_info["name"], "semctx");
    assert_eq!(server_info["title"], "semctx code intelligence");
    assert_eq!(server_info["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        initialized["result"].get("instructions").is_none(),
        "server instructions can be prepended to every tool description by the host"
    );

    send(
        &mut stdin,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;

    send(
        &mut stdin,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;
    let listed = receive(&mut stdout).await;
    assert_eq!(listed["id"], 2);
    let tools = listed["result"]["tools"].as_array().expect("tool list");
    assert!(!tools.is_empty());
    for tool in tools {
        let title = tool["title"].as_str().expect("human-readable tool title");
        assert!(!title.is_empty());
        assert!(!title.contains('_'), "{title}");
        assert_eq!(tool["annotations"]["title"], title);
    }
    let search = tools
        .iter()
        .find(|tool| tool["name"] == "search_codebase")
        .expect("stable search tool name");
    assert_eq!(search["title"], "Search code");

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "name": "read_source",
                "arguments": { "path": "src/lib.rs", "line_start": 1 }
            }
        }),
    )
    .await;
    let failed = receive(&mut stdout).await;
    assert_eq!(failed["id"], 3);
    assert_eq!(failed["result"]["isError"], true);
    assert_eq!(
        failed["result"]["content"][0]["text"],
        "read_source failed: line_start and line_end must be supplied together"
    );

    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("MCP shutdown timed out")
        .expect("wait for MCP shutdown");
    assert!(status.success());
}
