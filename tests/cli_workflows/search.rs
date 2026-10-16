use std::{fs, process::Stdio, time::Duration};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use super::{Checkout, Request, Server};

fn search_hit(codebase: Option<&str>) -> Value {
    json!({
        "domainId": "code", "id": codebase.unwrap_or("legacy"), "score": 0.9,
        "path": "src/lib.rs", "lineStart": 1, "lineEnd": 1,
        "language": "rust", "symbol": "remote", "kind": "function",
        "snippet": "fn remote() {}", "codebaseId": codebase
    })
}

fn search_response(request: &Request, hits: &Value) -> Value {
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/v1/search") => json!({"success": true, "data": hits}),
        ("GET", "/v1/codebases/A/files?page=0&pageSize=1000") => json!({
            "success": true, "data": [{"path": "src/lib.rs", "size": 17, "contentHash": "old-index-hash"}],
            "page": 0, "pageSize": 1000, "total": 1
        }),
        _ => panic!("unexpected search request: {request:?}"),
    }
}

#[tokio::test]
async fn cross_codebase_search_keeps_remote_paths_relative_and_unmarked() {
    for selector in [vec!["--codebase-id", "B"], vec!["--scope", "personal"]] {
        let checkout = Checkout::new();
        let server =
            Server::start(|request| search_response(request, &json!([search_hit(Some("B"))])));
        let mut arguments = vec!["--codebase", "A", "search", "remote function"];
        arguments.extend(selector);
        let output = checkout.run(&server, &checkout.root, &arguments).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("### `src/lib.rs:1"), "{text}");
        assert!(text.contains("Codebase: `B`"), "{text}");
        assert!(!text.contains("checkout-a"), "{text}");
        assert!(!text.contains("stale"), "{text}");
        let requests = server.requests();
        assert_eq!(
            requests.len(),
            1,
            "remote hits must not read the local catalog"
        );
        assert!(requests[0].body.get("codebaseId").is_none());
    }
}

#[tokio::test]
async fn mixed_search_marks_only_hits_bound_to_the_local_checkout() {
    let checkout = Checkout::new();
    let server = Server::start(|request| {
        search_response(
            request,
            &json!([
                search_hit(Some("A")),
                search_hit(Some("B")),
                search_hit(None),
            ]),
        )
    });
    let output = checkout
        .run(
            &server,
            &checkout.root,
            &[
                "--codebase",
                "A",
                "search",
                "shared path",
                "--scope",
                "personal",
            ],
        )
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let sections: Vec<_> = text.split("\n### ").skip(1).collect();
    let local = sections
        .iter()
        .find(|section| section.contains("Codebase: `A`"))
        .unwrap();
    let remote = sections
        .iter()
        .find(|section| section.contains("Codebase: `B`"))
        .unwrap();
    let unidentified = sections
        .iter()
        .find(|section| !section.contains("Codebase:"))
        .unwrap();
    assert!(local.contains("checkout-a"), "{text}");
    assert!(local.contains("stale"), "{text}");
    assert!(remote.starts_with("`src/lib.rs:1"), "{text}");
    assert!(unidentified.starts_with("`src/lib.rs:1"), "{text}");
    assert!(!remote.contains("stale"), "{text}");
    assert!(!unidentified.contains("stale"), "{text}");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn invalid_search_scope_fails_before_sending_a_request() {
    let checkout = Checkout::new();
    let server = Server::start(|request| panic!("unexpected request: {request:?}"));
    let output = checkout
        .run(
            &server,
            &checkout.root,
            &["--codebase", "A", "search", "query", "--scope", "typo"],
        )
        .await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("search_codebase failed: scope must be"),
        "{stderr}"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn corrupt_installation_identity_prevents_local_search_attribution() {
    let checkout = Checkout::new();
    fs::write(checkout.config.join("semctl/installation-id"), "corrupt").unwrap();
    let server = Server::start(|request| search_response(request, &json!([search_hit(Some("A"))])));
    let output = checkout
        .run(
            &server,
            &checkout.root,
            &["--codebase", "A", "search", "query"],
        )
        .await;
    assert!(output.status.success(), "{:?}", output.stderr);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("### `src/lib.rs:1"), "{text}");
    assert!(text.contains("Codebase: `A`"), "{text}");
    assert!(!text.contains("checkout-a"), "{text}");
    assert!(!text.contains("stale"), "{text}");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("x-semctx-source-id"));
}

#[tokio::test]
async fn mcp_scoped_search_works_from_an_unindexed_directory() {
    let checkout = Checkout::new();
    fs::write(checkout.config.join("semctl/config.toml"), "").unwrap();
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/whoami" => json!({"success": true, "data": {"capabilities": []}}),
        _ => search_response(request, &json!([search_hit(Some("B"))])),
    });
    let mut child = checkout
        .command(&server, &checkout.root, &["mcp"])
        .env("SEMCTX_MCP_UPDATE_CHECK", "0")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let messages = [
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": rmcp::model::ProtocolVersion::default(),
                "capabilities": {},
                "clientInfo": {"name": "scoped-search-test", "version": "1"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "search_codebase", "arguments": {
                "query": "remote", "scope": "personal"
            }}
        }),
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "search_codebase", "arguments": {
                "query": "remote", "codebase_ids": ["B"], "copy": "canonical"
            }}
        }),
        json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "search_codebase", "arguments": {
                "query": "remote", "scope": "personal", "codebase_ids": ["B"]
            }}
        }),
    ];
    for message in messages {
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        stdin.write_all(&bytes).await.unwrap();
        let Some(id) = message.get("id") else {
            continue;
        };
        let line = tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
            .await
            .expect("MCP scoped search timed out")
            .unwrap()
            .expect("MCP server closed stdout");
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(&response["id"], id, "{response}");
        if id == 1 {
            assert!(response["result"]["capabilities"]["tools"].is_object());
        } else if id == 4 {
            assert_eq!(response["result"]["isError"], true, "{response}");
            let text = response["result"]["content"][0]["text"].as_str().unwrap();
            assert!(
                text.starts_with(
                    "search_codebase failed: scope and codebase_ids are mutually exclusive\n"
                ),
                "{text}"
            );
            assert!(
                text.ends_with("next: correct the argument and call again."),
                "{text}"
            );
        } else {
            assert_eq!(response["result"]["isError"], false, "{response}");
            let text = response["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("### `src/lib.rs:1"), "{text}");
            assert!(text.contains("Codebase: `B`"), "{text}");
            assert!(!text.contains("checkout-a"), "{text}");
            assert!(!text.contains("stale"), "{text}");
        }
    }
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("MCP shutdown timed out")
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let requests = server.requests();
    let searches: Vec<_> = requests
        .iter()
        .filter(|request| request.path == "/v1/search")
        .collect();
    assert_eq!(searches.len(), 2);
    assert_eq!(searches[0].body["scope"], "Personal");
    assert_eq!(searches[1].body["codebaseIds"], json!(["B"]));
    assert!(
        searches
            .iter()
            .all(|request| request.body.get("codebaseId").is_none())
    );
}
