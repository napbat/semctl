use std::path::Path;
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
async fn mcp_metadata_exposes_tool_docs_without_shared_instructions() {
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
    assert!(
        initialized["result"].get("instructions").is_none(),
        "server instructions can be prepended to every tool description by the host"
    );

    send(
        &mut stdin,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    for id in [2, 3] {
        send(
            &mut stdin,
            json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" }),
        )
        .await;
        let response = receive(&mut stdout).await;
        assert_eq!(response["id"], id);
        let tools = response["result"]["tools"]
            .as_array()
            .expect("tool catalog");
        assert!(!tools.is_empty());
        for tool in tools {
            let name = tool["name"].as_str().expect("tool name");
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/mcp/docs/tools")
                .join(format!("{name}.md"));
            let doc = std::fs::read_to_string(path).expect("read tool documentation");
            assert_eq!(tool["description"].as_str(), Some(doc.as_str()), "{name}");
            assert!(tool["inputSchema"].is_object(), "{name}");
        }
    }

    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("MCP shutdown timed out")
        .expect("wait for MCP shutdown");
    assert!(status.success());
}
