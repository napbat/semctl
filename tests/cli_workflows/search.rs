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
        ("GET", "/v1/codebases/A/files/content?path=src%2Flib.rs&byteStart=0&byteEnd=1") => json!({
            "success": true, "data": {
                "path": "src/lib.rs", "contentHash": "old-index-hash",
                "content": "f", "encoding": "utf-8", "isBinary": false,
                "size": 17, "byteStart": 0, "byteEnd": 1, "truncated": true
            }
        }),
        _ => panic!("unexpected search request: {request:?}"),
    }
}

#[tokio::test]
async fn cross_codebase_search_keeps_remote_paths_relative_and_unmarked() {
    for (selector, field, value) in [
        (vec!["--codebase-id", "B"], "codebaseIds", json!(["B"])),
        (vec!["--scope", "personal"], "scope", json!("Personal")),
    ] {
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
        assert!(text.contains("src/lib.rs:1-1"), "{text}");
        assert!(text.contains("fn remote() {}"), "{text}");
        assert!(!text.contains("checkout-a"), "{text}");
        assert!(!text.contains("stale"), "{text}");
        let requests = server.requests();
        assert_eq!(requests.len(), 1, "remote hits must not probe local source");
        assert!(requests[0].body.get("codebaseId").is_none());
        assert_eq!(requests[0].body[field], value);
    }
}

#[tokio::test]
async fn mixed_search_marks_only_hits_bound_to_the_local_checkout() {
    let checkout = Checkout::new();
    let server = Server::start(|request| {
        let hits: Vec<_> = [
            (Some("A"), "fn local_bound() {}"),
            (Some("B"), "fn remote_bound() {}"),
            (None, "fn unidentified_bound() {}"),
        ]
        .into_iter()
        .map(|(codebase, source)| {
            let mut hit = search_hit(codebase);
            hit["snippet"] = json!(source);
            hit
        })
        .collect();
        search_response(request, &json!(hits))
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
    // Indexed source distinguishes the hits without depending on heading or
    // metadata layout. All hits deliberately share the same relative path.
    let (local, remaining) = text
        .split_once("fn local_bound() {}")
        .expect("the local indexed source is retained");
    let (remote, remaining) = remaining
        .split_once("fn remote_bound() {}")
        .expect("the remote indexed source is retained");
    let (unidentified, _) = remaining
        .split_once("fn unidentified_bound() {}")
        .expect("the unidentified indexed source is retained");
    let local_location = format!("{}:1-1", checkout.root.join("src/lib.rs").display());
    let local_location = local_location
        .strip_prefix(r"\\?\")
        .unwrap_or(&local_location);
    assert!(local.contains(local_location), "{text}");
    for nonlocal in [remote, unidentified] {
        assert!(nonlocal.contains("src/lib.rs:1-1"), "{text}");
        assert!(!nonlocal.contains("checkout-a"), "{text}");
        assert!(!nonlocal.contains("stale"), "{text}");
    }
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
    assert!(text.contains("src/lib.rs:1-1"), "{text}");
    assert!(text.contains("fn remote() {}"), "{text}");
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
        } else {
            assert_eq!(response["result"]["isError"], false, "{response}");
            let text = response["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("src/lib.rs:1-1"), "{text}");
            assert!(text.contains("fn remote() {}"), "{text}");
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
    assert!(
        searches
            .iter()
            .all(|request| !request.headers.contains_key("x-semctx-source-id"))
    );
    assert!(
        requests
            .iter()
            .all(|request| !request.path.starts_with("/v1/codebases/A/files"))
    );
}

#[tokio::test]
async fn search_keeps_old_evidence_visible_when_source_is_pending_despite_current_catalog() {
    let checkout = Checkout::new();
    let current = "fn current_name() {}\n";
    fs::write(checkout.root.join("src/lib.rs"), current).unwrap();
    let server = Server::start(move |request| match request.path.as_str() {
        "/v1/search" => {
            let mut hit = search_hit(Some("A"));
            hit["snippet"] = json!("fn old_name() {}");
            hit["snippetByteStart"] = json!(0);
            hit["snippetByteEnd"] = json!(16);
            json!({"success": true, "data": [hit]})
        }
        "/v1/codebases/A/files?page=0&pageSize=1000" => json!({
            "success": true, "data": [{
                "path": "src/lib.rs", "size": current.len(),
                "contentHash": blake3::hash(current.as_bytes()).to_hex().to_string()
            }], "page": 0, "pageSize": 1000, "total": 1
        }),
        "/v1/codebases/A/files/content?path=src%2Flib.rs&byteStart=0&byteEnd=1" => json!({
            "success": false,
            "errors": [{"code": "FileNotIndexed", "message": "source unavailable", "details": {"status": "Pending"}}]
        }),
        _ => panic!("unexpected search request: {request:?}"),
    });
    let output = checkout
        .run(
            &server,
            &checkout.root,
            &["--codebase", "A", "search", "old function"],
        )
        .await;
    assert!(output.status.success(), "{:?}", output.stderr);
    let text = String::from_utf8(output.stdout).unwrap();
    let local_location = format!("{}:1-1", checkout.root.join("src/lib.rs").display());
    let local_location = local_location
        .strip_prefix(r"\\?\")
        .unwrap_or(&local_location);
    assert!(
        text.contains("FileNotIndexed") && text.contains("Pending"),
        "{text}"
    );
    assert!(
        text.contains(local_location) && text.contains("fn old_name() {}"),
        "{text}"
    );
    assert!(!text.contains(current.trim()), "{text}");
}

#[tokio::test]
async fn automatic_candidate_hints_do_not_silently_accept_pending_source() {
    let checkout = Checkout::new();
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/codebases/A" => json!({"id": "A", "slug": "a", "sourceKind": "Local"}),
        "/v1/search" => json!({"success": true, "data": [search_hit(Some("A"))]}),
        "/v1/codebases/A/files/content?path=src%2Flib.rs&byteStart=0&byteEnd=1" => json!({
            "success": false,
            "errors": [{"code": "FileNotIndexed", "message": "source unavailable", "details": {"status": "Pending"}}]
        }),
        _ => panic!("unexpected hook request: {request:?}"),
    });
    let mut command = checkout.command(&server, &checkout.root, &["hook"]);
    command
        .stdin(Stdio::piped())
        .env_remove("SEMCTX_HOOK_DISABLE")
        .env_remove("SEMCTX_HOOK_MIN_SCORE")
        .env("SEMCTX_HOOK_TOP_K", "1");
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(
            &serde_json::to_vec(&json!({
                "hook_event_name": "UserPromptSubmit",
                "cwd": checkout.root, "prompt": "Where is repository source handling implemented?"
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let text = result["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("src/lib.rs:1-1") && text.contains("remote"),
        "{text}"
    );
    assert!(
        text.contains("FileNotIndexed") && text.contains("Pending"),
        "{text}"
    );
}
