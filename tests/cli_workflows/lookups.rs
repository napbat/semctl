use std::{process::Stdio, time::Duration};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

use super::{Checkout, Request, Server, mcp};

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u32,
}

impl Session {
    async fn start(checkout: &Checkout, server: &Server) -> Self {
        // Query-only sessions must not opt into a background checkout reconcile.
        std::fs::write(checkout.config.join("semctl/config.toml"), "").unwrap();
        let mut child = checkout
            .command(server, &checkout.root, &["--codebase", "A", "mcp"])
            .env("SEMCTX_MCP_UPDATE_CHECK", "0")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut session = Self {
            child,
            stdin,
            stdout,
            next_id: 2,
        };
        session
            .send(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": rmcp::model::ProtocolVersion::default(),
                    "capabilities": {},
                    "clientInfo": {"name": "lookup-test", "version": "1"}
                }
            }))
            .await;
        let initialized = session.receive().await;
        assert_eq!(initialized["id"], 1, "{initialized}");
        session
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        session
    }

    async fn send(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await.unwrap();
    }

    async fn receive(&mut self) -> Value {
        mcp::response(
            &mut self.stdout,
            Duration::from_secs(10),
            "MCP lookup timed out",
        )
        .await
        .1
    }

    async fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        }))
        .await;
        let response = self.receive().await;
        assert_eq!(response["id"], id, "{response}");
        response["result"].clone()
    }

    async fn finish(mut self) {
        drop(self.stdin);
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
    }
}

fn hit(symbol: &str) -> Value {
    json!({
        "domainId": "code", "id": symbol, "score": 0,
        "codebaseId": "A", "path": "src/lib.rs", "lineStart": 1, "lineEnd": 2,
        "byteStart": 0, "byteEnd": 24, "focusLine": 2, "focusByte": 12,
        "snippetLineStart": 1, "snippetLineEnd": 2,
        "snippetByteStart": 0, "snippetByteEnd": 24,
        "language": "rust", "symbol": symbol, "qualifiedSymbol": format!("crate::{symbol}"),
        "enclosingSymbol": "run", "isWrite": true,
        "referenceNamespace": "Value", "referenceKind": "Assignment",
        "externalTarget": "external::target", "kind": "function",
        "snippet": "fn run() {\n    target = 1;\n}"
    })
}

fn response(request: &Request, data: Value) -> Value {
    assert_eq!(request.method, "GET", "{request:?}");
    if request.path == "/v1/whoami" {
        json!({"success": true, "data": {"capabilities": []}})
    } else {
        let mut envelope = json!({"success": true});
        envelope["data"] = data;
        envelope
    }
}

fn lookup_requests(server: &Server) -> Vec<Request> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path != "/v1/whoami")
        .collect()
}

fn assert_trace_topology(nodes: &[Value], edges: &[Value]) {
    let subject = nodes
        .iter()
        .find(|node| node["label"] == "a" && node["hit"].is_null())
        .expect("trace has an explicit query subject");
    let definition = nodes
        .iter()
        .find(|node| node["hit"]["symbol"] == "a")
        .expect("trace retains its definition match");
    let incoming = nodes
        .iter()
        .find(|node| node["hit"]["symbol"] == "b")
        .expect("trace retains its caller");
    let outgoing = nodes
        .iter()
        .find(|node| node["hit"]["symbol"] == "c")
        .expect("trace retains its callee");
    let pairs: Vec<_> = edges
        .iter()
        .map(|edge| (&edge["from"], &edge["to"]))
        .collect();
    assert_eq!(pairs.len(), 3);
    assert!(pairs.contains(&(&subject["id"], &definition["id"])));
    assert!(pairs.contains(&(&incoming["id"], &subject["id"])));
    assert!(pairs.contains(&(&subject["id"], &outgoing["id"])));
}

#[tokio::test]
async fn lookup_tools_keep_cli_text_and_add_unprobed_native_matches_without_requests() {
    let checkout = Checkout::new();
    let server = Server::start(|request| response(request, json!([hit("target")])));
    let mut session = Session::start(&checkout, &server).await;
    for (tool, command, endpoint, arguments) in [
        (
            "find_definition",
            "definitions",
            "definitions?symbol=target",
            json!({"symbol": "target"}),
        ),
        (
            "find_references",
            "references",
            "references?symbol=target&referenceNamespace=Value",
            json!({"symbol": "target", "namespace": "Value"}),
        ),
        (
            "who_calls",
            "who-calls",
            "who-calls?symbol=target",
            json!({"symbol": "target"}),
        ),
        (
            "implementations_of",
            "implementations",
            "implementations?symbol=target",
            json!({"symbol": "target"}),
        ),
    ] {
        let before = lookup_requests(&server).len();
        let result = session.call(tool, arguments).await;
        assert_eq!(result["isError"], false, "{result}");
        let display = &result["_meta"]["semctlDisplay"];
        assert_eq!(display["version"], 1);
        assert_eq!(display["kind"], "batch");
        assert_eq!(display["symbols"][0]["symbol"], "target");
        let projected = &display["symbols"][0]["hits"][0];
        assert_eq!(projected["binding"]["state"], "not-probed");
        assert_eq!(projected["score"], Value::Null);
        assert_eq!(projected["qualifiedName"], "crate::target");
        assert_eq!(projected["enclosingSymbol"], "run");
        assert_eq!(projected["isWrite"], true);
        assert_eq!(projected["referenceNamespace"], "Value");
        assert_eq!(projected["referenceKind"], "Assignment");
        assert_eq!(projected["externalTarget"], "external::target");
        assert_eq!(projected["startByte"], 0);
        assert_eq!(projected["focusByte"], 12);
        assert_eq!(projected["snippet"], hit("target")["snippet"]);
        assert!(serde_json::to_vec(display).unwrap().len() <= 32 * 1024);
        let requests = lookup_requests(&server);
        assert_eq!(requests.len(), before + 1, "{requests:?}");
        assert_eq!(
            requests[before].path,
            format!("/v1/codebases/A/graph/{endpoint}")
        );

        let mut arguments = vec!["--codebase", "A", "graph", command, "target"];
        if tool == "find_references" {
            arguments.extend(["--namespace", "Value"]);
        }
        let output = checkout.run(&server, &checkout.root, &arguments).await;
        assert!(output.status.success(), "{:?}", output.stderr);
        let cli_text = String::from_utf8(output.stdout).unwrap();
        let model_text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(cli_text.trim_end(), model_text.trim_end());
        assert_eq!(lookup_requests(&server).len(), before + 2);
    }
    session.finish().await;
}

#[tokio::test]
async fn expansion_keeps_its_path_and_call_path_graphs_preserve_directed_sequence() {
    let checkout = Checkout::new();
    let server = Server::start(|request| {
        let hits = if request.path.contains("/call-path?") {
            json!([hit("run"), hit("target")])
        } else {
            json!([hit("target")])
        };
        response(request, hits)
    });
    let mut session = Session::start(&checkout, &server).await;
    let before = lookup_requests(&server).len();
    let expanded = session
        .call(
            "expand_chunk",
            json!({
                "path": "src/lib.rs", "line_start": 1, "line_end": 2
            }),
        )
        .await;
    let display = &expanded["_meta"]["semctlDisplay"];
    assert_eq!(display["kind"], "batch");
    assert_eq!(display["symbols"][0]["symbol"], "src/lib.rs");
    assert_eq!(display["symbols"][0]["hits"][0]["score"], Value::Null);
    assert_eq!(
        display["symbols"][0]["hits"][0]["binding"]["state"],
        "not-probed"
    );
    let chain = session
        .call("call_path", json!({"from": "run", "to": "target"}))
        .await;
    assert_eq!(chain["isError"], false);
    let graph = &chain["_meta"]["semctlDisplay"];
    assert_eq!(graph["kind"], "graph");
    let nodes = graph["nodes"].as_array().unwrap();
    let from = nodes
        .iter()
        .find(|node| node["hit"]["symbol"] == "run")
        .unwrap();
    let to = nodes
        .iter()
        .find(|node| node["hit"]["symbol"] == "target")
        .unwrap();
    assert_eq!(graph["roots"][0], from["id"]);
    let edges = graph["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["from"], from["id"]);
    assert_eq!(edges[0]["to"], to["id"]);
    let requests = lookup_requests(&server);
    assert_eq!(requests.len(), before + 2, "{requests:?}");
    assert_eq!(
        requests[before].path,
        "/v1/codebases/A/graph/expand?path=src%2Flib.rs&lineStart=1&lineEnd=2"
    );
    assert_eq!(
        requests[before + 1].path,
        "/v1/codebases/A/graph/call-path?from=run&to=target"
    );
    session.finish().await;
}

#[tokio::test]
async fn empty_exact_lookups_keep_near_miss_suggestions_in_the_text_fallback() {
    let checkout = Checkout::new();
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/search" => {
            assert_eq!(request.method, "POST");
            assert_eq!(request.body["query"], "Target");
            assert_eq!(request.body["topK"], 10);
            json!({"success": true, "data": [hit("target")]})
        }
        _ => response(request, json!([])),
    });
    let mut session = Session::start(&checkout, &server).await;
    for (tool, endpoint) in [
        ("find_definition", "definitions"),
        ("find_references", "references"),
    ] {
        let before = lookup_requests(&server).len();
        let result = session.call(tool, json!({"symbol": "Target"})).await;
        assert_eq!(result["isError"], false, "{result}");
        assert!(result.get("_meta").is_none(), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("Did you mean") && text.contains("`target`"),
            "{text}"
        );
        let requests = lookup_requests(&server);
        assert_eq!(requests.len(), before + 2, "{requests:?}");
        assert_eq!(
            requests[before].path,
            format!("/v1/codebases/A/graph/{endpoint}?symbol=Target")
        );
        assert_eq!(requests[before + 1].path, "/v1/search");
    }
    session.finish().await;
}

#[tokio::test]
async fn lookup_display_limits_fall_back_without_truncating_model_evidence() {
    let checkout = Checkout::new();
    let server = Server::start(|request| {
        let data = if request.path.contains("symbol=many") {
            (0..33)
                .map(|index| hit(&format!("target_{index}")))
                .collect::<Vec<_>>()
        } else {
            let mut oversized = hit("large");
            oversized["snippet"] = json!(format!("{}END_OF_EVIDENCE", "x".repeat(32 * 1024)));
            vec![oversized]
        };
        response(request, json!(data))
    });
    let mut session = Session::start(&checkout, &server).await;
    for (symbol, final_evidence) in [("many", "target_32"), ("large", "END_OF_EVIDENCE")] {
        let before = lookup_requests(&server).len();
        let result = session
            .call("find_references", json!({"symbol": symbol}))
            .await;
        assert_eq!(result["isError"], false, "{result}");
        assert!(result.get("_meta").is_none());
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(final_evidence)
        );
        assert_eq!(lookup_requests(&server).len(), before + 1);
    }
    session.finish().await;
}

#[tokio::test]
async fn symbol_search_keeps_rank_filters_and_cli_text_without_source_probes() {
    let checkout = Checkout::new();
    let server = Server::start(|request| {
        response(
            request,
            json!([{
                "codebaseId": "A", "path": "src/lib.rs", "lineStart": 1, "lineEnd": 2,
                "byteStart": 0, "byteEnd": 24, "name": "target", "qualifiedName": "crate::target",
                "kind": "function", "project": "core", "language": "rust", "score": 97
            }]),
        )
    });
    let mut session = Session::start(&checkout, &server).await;
    let before = lookup_requests(&server).len();
    let result = session
        .call(
            "search_symbols",
            json!({
                "query": "target", "mode": "Prefix", "kinds": ["function"],
                "path_prefix": "src/", "project": "core", "language": "rust", "limit": 7
            }),
        )
        .await;
    assert_eq!(result["isError"], false, "{result}");
    let display = &result["_meta"]["semctlDisplay"];
    assert_eq!(display["kind"], "batch");
    assert_eq!(display["symbols"][0]["symbol"], "target");
    let projected = &display["symbols"][0]["hits"][0];
    assert_eq!(projected["score"], json!(97.0));
    assert_eq!(projected["project"], "core");
    assert_eq!(projected["binding"]["state"], "not-probed");
    let requests = lookup_requests(&server);
    assert_eq!(requests.len(), before + 1, "{requests:?}");
    assert_eq!(
        requests[before].path,
        "/v1/codebases/A/graph/symbols?query=target&mode=Prefix&limit=7&kinds=function&pathPrefix=src%2F&project=core&language=rust"
    );
    let output = checkout
        .run(
            &server,
            &checkout.root,
            &[
                "--codebase",
                "A",
                "graph",
                "search-symbols",
                "target",
                "--mode",
                "Prefix",
                "--kind",
                "function",
                "--path-prefix",
                "src/",
                "--project",
                "core",
                "--language",
                "rust",
                "--limit",
                "7",
            ],
        )
        .await;
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        result["content"][0]["text"].as_str().unwrap().trim_end()
    );
    assert_eq!(lookup_requests(&server).len(), before + 2);
    session.finish().await;
}

#[tokio::test]
async fn lookup_tool_errors_keep_the_mcp_error_route_without_display_metadata() {
    let checkout = Checkout::new();
    let server = Server::start(|request| {
        if request.path == "/v1/whoami" {
            response(request, Value::Null)
        } else {
            json!({"success": false, "errors": [{"code": "InvalidArgument", "message": "lookup rejected"}]})
        }
    });
    let mut session = Session::start(&checkout, &server).await;
    for (tool, arguments) in [
        ("find_definition", json!({"symbol": "target"})),
        ("find_references", json!({"symbol": "target"})),
        ("who_calls", json!({"symbol": "target"})),
        ("implementations_of", json!({"symbol": "target"})),
        (
            "expand_chunk",
            json!({"path": "src/lib.rs", "line_start": 1, "line_end": 2}),
        ),
        ("search_symbols", json!({"query": "target"})),
        (
            "symbol_at_position",
            json!({"path": "src/lib.rs", "line": 1}),
        ),
        ("trace", json!({"symbol": "target"})),
        ("call_graph", json!({"symbol": "target"})),
        ("call_path", json!({"from": "run", "to": "target"})),
        ("reaches", json!({"from": "input"})),
        ("flows_into", json!({"to": "output"})),
        ("flows_between", json!({"from": "input", "to": "output"})),
        ("type_hierarchy", json!({"symbol": "target"})),
        ("cycles", json!({})),
        ("duplicates", json!({})),
        ("unused", json!({})),
        ("imports", json!({})),
        ("symbol_edges", json!({})),
        ("external_links", json!({})),
        ("list_projects", json!({})),
        ("file_outline", json!({"path": "src/lib.rs"})),
    ] {
        let before = lookup_requests(&server).len();
        let result = session.call(tool, arguments).await;
        assert_eq!(result["isError"], true, "{result}");
        assert!(result.get("_meta").is_none(), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with(&format!("{tool} failed:")), "{text}");
        assert!(text.contains("lookup rejected"), "{text}");
        assert_eq!(lookup_requests(&server).len(), before + 1);
    }
    session.finish().await;
}

fn graph_response(request: &Request) -> Value {
    let path = request.path.as_str();
    if path == "/v1/whoami" {
        return response(request, Value::Null);
    }
    let data = if path.contains("/graph/call-graph?") {
        json!({
            "nodes": [hit("a"), hit("b"), hit("c")],
            "edges": [
                {"from": "a", "to": "b"}, {"from": "a", "to": "c"},
                {"from": "b", "to": "c"}, {"from": "c", "to": "a"}
            ]
        })
    } else if path.contains("/graph/trace?") {
        json!({"definition": [hit("a")], "callers": [hit("b")], "callees": [hit("c")]})
    } else if path.contains("/graph/type-hierarchy?") {
        json!({
            "roots": ["Base"], "complete": false, "caveat": "partial providers",
            "nodes": [
                {"identity": "Base", "name": "Base", "qualifiedName": "Base", "kind": "trait", "external": true},
                {"identity": "Child", "name": "Child", "qualifiedName": "Child", "kind": "struct", "external": false,
                 "path": "src/lib.rs", "lineStart": 1, "lineEnd": 2}
            ],
            "edges": [{"subtype": "Child", "supertype": "Base", "relation": "implements", "origin": "declared", "depth": 1}]
        })
    } else if path.contains("/graph/call-path?") || path.contains("/graph/flow/between?") {
        json!([hit("a"), hit("b")])
    } else if path.contains("/graph/flow/") {
        json!(["external::sink"])
    } else if path.ends_with("/graph/cycles") || path.ends_with("/graph/duplicates") {
        json!([{"index": 1, "size": 2, "chunks": [hit("a"), hit("b")], "hash": "shared-hash"}])
    } else if path.contains("/graph/unused?") {
        json!([{"definition": hit("a"), "reason": "no incoming references", "completenessCaveat": "partial providers"}])
    } else if path.contains("/graph/imports?") {
        json!([{"from": "src/a.rs", "to": "src/b.rs", "importPath": "crate::b"}])
    } else if path.contains("/graph/symbol-edges?") {
        json!([{"fromFile": "src/a.rs", "name": "b", "toFile": "src/b.rs", "moniker": "crate::b"}])
    } else if path.contains("/graph/external-links?") {
        json!([{
            "fromFile": "src/a.rs", "importPath": "remote::b", "ecosystem": "cargo",
            "targetPackage": "remote", "descriptor": "b", "targetCodebaseId": "B",
            "targetFile": "src/b.rs", "targetName": "b", "confidence": 100
        }])
    } else if path.ends_with("/projects") {
        json!({
            "projects": [{"kind": "cargo", "name": "core", "root": "core", "configFile": "core/Cargo.toml", "fileCount": 3}],
            "groups": [{"kind": "workspace", "name": "workspace", "root": ".", "children": ["core"]}]
        })
    } else if path.contains("/graph/outline?") {
        json!({
            "path": "src/lib.rs", "entries": [{
                "chunkId": "a", "lineStart": 1, "lineEnd": 2, "kind": "function",
                "symbol": "a", "qualifiedSymbol": "crate::a", "symbolKind": "function",
                "depth": 0, "body": "fn a() {}"
            }]
        })
    } else if path.contains("/graph/symbol-at-position?") {
        hit("a")
    } else {
        panic!("unexpected graph request: {request:?}");
    };
    let mut envelope = response(request, data);
    if path.contains("pageSize=") {
        envelope["page"] = json!(0);
        envelope["pageSize"] = json!(1000);
        envelope["total"] = json!(1);
    }
    envelope
}

fn graph_pairs(graph: &Value) -> Vec<(String, String)> {
    let nodes = graph["nodes"].as_array().unwrap();
    let symbol = |id: &Value| {
        let node = nodes.iter().find(|node| &node["id"] == id).unwrap();
        node["hit"]["symbol"].as_str().unwrap().to_string()
    };
    graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| (symbol(&edge["from"]), symbol(&edge["to"])))
        .collect()
}

#[tokio::test]
async fn graph_tools_retain_structure_caveats_and_pagination_without_source_probes() {
    let checkout = Checkout::new();
    let server = Server::start(graph_response);
    let mut session = Session::start(&checkout, &server).await;
    for (tool, arguments) in [
        ("trace", json!({"symbol": "a"})),
        ("call_graph", json!({"symbol": "a"})),
        ("call_path", json!({"from": "a", "to": "b"})),
        ("reaches", json!({"from": "input"})),
        ("flows_into", json!({"to": "output"})),
        ("flows_between", json!({"from": "input", "to": "output"})),
        ("type_hierarchy", json!({"symbol": "Base"})),
        ("cycles", json!({})),
        ("duplicates", json!({})),
        ("unused", json!({"page": 0, "page_size": 1})),
        ("imports", json!({})),
        ("symbol_edges", json!({})),
        ("external_links", json!({})),
        ("list_projects", json!({})),
        (
            "file_outline",
            json!({"path": "src/lib.rs", "include_body": true}),
        ),
    ] {
        let before = lookup_requests(&server).len();
        let result = session.call(tool, arguments).await;
        assert_eq!(result["isError"], false, "{tool}: {result}");
        let graph = &result["_meta"]["semctlDisplay"];
        assert_eq!(graph["kind"], "graph", "{tool}: {result}");
        assert!(serde_json::to_vec(graph).unwrap().len() <= 32 * 1024);
        let nodes = graph["nodes"].as_array().unwrap();
        assert!(nodes.len() <= 64);
        for node in nodes {
            if !node["hit"].is_null() {
                assert_eq!(
                    node["hit"]["binding"]["state"], "not-probed",
                    "{tool}: {node}"
                );
                assert!(node["hit"]["score"].is_null(), "{tool}: {node}");
            }
        }
        let edges = graph["edges"].as_array().unwrap();
        assert!(edges.len() <= 128);
        for edge in edges {
            for endpoint in ["from", "to"] {
                assert!(
                    nodes.iter().any(|node| node["id"] == edge[endpoint]),
                    "{tool}: {edge}"
                );
            }
        }
        if tool == "call_graph" {
            let mut pairs = graph_pairs(graph);
            pairs.sort();
            assert_eq!(
                pairs,
                [
                    ("a".into(), "b".into()),
                    ("a".into(), "c".into()),
                    ("b".into(), "c".into()),
                    ("c".into(), "a".into())
                ]
            );
        } else if tool == "trace" {
            assert_trace_topology(nodes, edges);
        } else if tool == "type_hierarchy" || tool == "unused" {
            assert!(
                graph.to_string().contains("partial providers"),
                "{tool}: {graph}"
            );
        } else if tool == "flows_between" {
            assert!(edges.iter().all(|edge| edge["label"] != "calls"), "{graph}");
        }
        assert_eq!(
            lookup_requests(&server).len(),
            before + 1,
            "{tool}: {:?}",
            lookup_requests(&server)
        );
        if tool == "unused" {
            assert_eq!(
                lookup_requests(&server)[before].path,
                "/v1/codebases/A/graph/unused?page=0&pageSize=1"
            );
        }
    }
    session.finish().await;
}

#[tokio::test]
async fn graph_json_and_plain_cli_share_text_and_keep_project_membership() {
    let checkout = Checkout::new();
    let server = Server::start(graph_response);
    for operation in [vec!["call-graph", "a"], vec!["projects"]] {
        let mut plain_arguments = vec!["--codebase", "A", "graph"];
        plain_arguments.extend(operation.iter().copied());
        let plain = checkout
            .run(&server, &checkout.root, &plain_arguments)
            .await;
        assert!(plain.status.success(), "{:?}", plain.stderr);
        let mut json_arguments = vec!["--codebase", "A", "graph", "--json"];
        json_arguments.extend(operation.iter().copied());
        let output = checkout.run(&server, &checkout.root, &json_arguments).await;
        let native = super::successful_json(&output);
        assert_eq!(
            String::from_utf8(plain.stdout).unwrap().trim_end(),
            native["text"].as_str().unwrap().trim_end()
        );
        let graph = &native["display"];
        assert_eq!(graph["kind"], "graph", "{native}");
        if operation[0] == "projects" {
            let nodes = graph["nodes"].as_array().unwrap();
            let project = nodes.iter().find(|node| node["label"] == "core").unwrap();
            let group = nodes
                .iter()
                .find(|node| node["label"] == "workspace")
                .unwrap();
            assert!(
                graph["edges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|edge| edge["from"] == group["id"] && edge["to"] == project["id"])
            );
        } else {
            assert_eq!(graph_pairs(graph).len(), 4);
        }
    }
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn symbol_at_position_keeps_input_coordinates_and_compact_unprobed_matches() {
    let checkout = Checkout::new();
    let server = Server::start(graph_response);
    let mut session = Session::start(&checkout, &server).await;
    let before = lookup_requests(&server).len();
    let result = session
        .call(
            "symbol_at_position",
            json!({
                "path": "src/lib.rs", "line": 1, "column": 7
            }),
        )
        .await;
    assert_eq!(result["isError"], false, "{result}");
    let display = &result["_meta"]["semctlDisplay"];
    assert_eq!(display["kind"], "batch");
    assert_eq!(display["symbols"][0]["symbol"], "src/lib.rs:1:7");
    assert_eq!(display["symbols"][0]["hits"][0]["symbol"], "a");
    assert_eq!(
        display["symbols"][0]["hits"][0]["binding"]["state"],
        "not-probed"
    );
    assert!(display["symbols"][0]["hits"][0]["score"].is_null());
    let requests = lookup_requests(&server);
    assert_eq!(requests.len(), before + 1);
    assert_eq!(
        requests[before].path,
        "/v1/codebases/A/graph/symbol-at-position?path=src%2Flib.rs&line=1&column=7"
    );
    let output = checkout
        .run(
            &server,
            &checkout.root,
            &[
                "--codebase",
                "A",
                "graph",
                "symbol-at-position",
                "src/lib.rs",
                "1",
                "--column",
                "7",
            ],
        )
        .await;
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        result["content"][0]["text"].as_str().unwrap().trim_end()
    );
    assert_eq!(lookup_requests(&server).len(), before + 2);
    session.finish().await;
}
