use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::*;

fn hit(id: &str) -> api::SearchHit {
    serde_json::from_value(json!({
        "domainId":"code", "id":id, "score":0.0, "symbol":id,
        "qualifiedSymbol":format!("crate::{id}"), "path":"src/lib.rs",
        "lineStart":2, "lineEnd":3, "byteStart":8, "byteEnd":20,
        "snippet":"fn source() {}", "codebaseId":"A"
    }))
    .unwrap()
}

fn view(input: GraphInput<'_>) -> Value {
    let client = Client::for_test("A", None);
    let value = project_graph(&client, input).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["kind"], "graph");
    assert!(serde_json::to_vec(&value).unwrap().len() <= MAX_DISPLAY_BYTES);
    let nodes = value["nodes"].as_array().unwrap();
    let ids: BTreeSet<_> = nodes
        .iter()
        .map(|node| node["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), nodes.len());
    assert!(ids.len() <= MAX_NODES);
    let edges = value["edges"].as_array().unwrap();
    assert!(edges.len() <= MAX_EDGES);
    for edge in edges {
        assert!(ids.contains(edge["from"].as_str().unwrap()));
        assert!(ids.contains(edge["to"].as_str().unwrap()));
        assert!(edge.get("kind").is_none());
        assert!(edge.get("symbol").is_none());
        assert!(edge.get("moniker").is_none());
    }
    for root in value["roots"].as_array().unwrap() {
        assert!(ids.contains(root.as_str().unwrap()));
    }
    let previews: Vec<_> = nodes.iter().filter(|node| !node["hit"].is_null()).collect();
    assert!(previews.len() <= MAX_DISPLAY_HITS);
    for node in previews {
        assert_eq!(node["hit"]["binding"]["state"], "not-probed");
        assert!(node["hit"]["score"].is_null());
    }
    value
}

fn has_edge(value: &Value, from: &str, to: &str) -> bool {
    value["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|edge| edge["from"] == from && edge["to"] == to)
}

fn node<'a>(value: &'a Value, id: &str) -> &'a Value {
    value["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == id)
        .unwrap()
}

#[test]
fn calls_preserve_cycles_shared_parents_and_disconnected_navigation() {
    let graph = api::CallGraph {
        nodes: ["a", "b", "c", "isolated", "x", "y"].map(hit).into(),
        edges: [("a", "c"), ("b", "c"), ("c", "a"), ("x", "y"), ("y", "x")]
            .map(|(from, to)| api::CallEdge {
                from: from.into(),
                to: to.into(),
            })
            .into(),
    };
    let value = view(GraphInput::CallGraph {
        symbol: "a",
        graph: &graph,
    });
    assert!(has_edge(&value, "a", "c"));
    assert!(has_edge(&value, "b", "c"));
    assert!(has_edge(&value, "c", "a"));
    assert!(has_edge(&value, "x", "y"));
    assert!(has_edge(&value, "y", "x"));
    let mut reached: BTreeSet<_> = value["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|root| root.as_str().unwrap())
        .collect();
    loop {
        let before = reached.len();
        for edge in value["edges"].as_array().unwrap() {
            if reached.contains(edge["from"].as_str().unwrap()) {
                reached.insert(edge["to"].as_str().unwrap());
            }
        }
        if before == reached.len() {
            break;
        }
    }
    assert_eq!(reached.len(), graph.nodes.len());
    assert_eq!(node(&value, "a")["hit"]["qualifiedName"], "crate::a");
    assert_eq!(node(&value, "a")["hit"]["startByte"], 8);
    assert_eq!(
        view(GraphInput::CallGraph {
            symbol: "a",
            graph: &graph
        })["roots"],
        value["roots"]
    );
}

#[test]
fn trace_preserves_query_subject_without_inventing_ambiguous_definition_bindings() {
    let result = api::TraceResult {
        definition: vec![hit("one"), hit("two")],
        callers: vec![hit("caller")],
        callees: vec![hit("callee")],
    };
    let value = view(GraphInput::Trace {
        symbol: "run",
        result: &result,
    });
    assert!(has_edge(&value, "caller", "query:run"));
    assert!(has_edge(&value, "query:run", "callee"));
    assert!(has_edge(&value, "query:run", "one"));
    assert!(has_edge(&value, "query:run", "two"));
    assert!(!has_edge(&value, "caller", "one"));
    assert!(!has_edge(&value, "caller", "two"));
}

#[test]
fn paths_distinguish_direct_calls_from_ordered_flow_witnesses() {
    let hits = [hit("a"), hit("b"), hit("c")];
    let calls = view(GraphInput::CallPath {
        from: "a",
        to: "c",
        hits: &hits,
    });
    let flow = view(GraphInput::FlowPath {
        from: "a",
        to: "c",
        hits: &hits,
    });
    for value in [&calls, &flow] {
        assert!(has_edge(value, "a", "b"));
        assert!(has_edge(value, "b", "c"));
        assert!(!has_edge(value, "a", "c"));
        assert_eq!(value["roots"][0], "a");
    }
    assert!(
        calls["edges"][0]["label"]
            .as_str()
            .unwrap()
            .contains("calls")
    );
    assert!(
        !flow["edges"][0]["label"]
            .as_str()
            .unwrap()
            .contains("calls")
    );
    assert!(flow["caveats"].as_array().unwrap().iter().any(|caveat| {
        caveat
            .as_str()
            .unwrap()
            .contains("do not prove a direct call")
    }));
    let targets = vec!["target".into()];
    let outgoing = view(GraphInput::Boundaries {
        boundary: "origin",
        targets: &targets,
        incoming: false,
    });
    let incoming = view(GraphInput::Boundaries {
        boundary: "origin",
        targets: &targets,
        incoming: true,
    });
    assert!(has_edge(&outgoing, "origin", "target"));
    assert!(has_edge(&incoming, "target", "origin"));
}

#[test]
fn type_hierarchy_preserves_external_identity_origin_depth_and_incompleteness() {
    let hierarchy: api::TypeHierarchy = serde_json::from_value(json!({
        "roots":["Child"], "nodes":[
            {"identity":"Child","name":"Child","qualifiedName":"app::Child","kind":"class","external":false,"path":"app.rs","lineStart":5,"lineEnd":9},
            {"identity":"pkg::Base","name":"Base","qualifiedName":"pkg::Base","kind":"class","external":true}
        ],
        "edges":[{"subtype":"Child","supertype":"pkg::Base","relation":"extends","origin":"declared","depth":2}],
        "complete":false,"caveat":"Unavailable package graph"
    })).unwrap();
    let value = view(GraphInput::Types {
        symbol: "Child",
        hierarchy: &hierarchy,
    });
    assert!(has_edge(&value, "Child", "pkg::Base"));
    let label = value["edges"][0]["label"].as_str().unwrap();
    assert!(label.contains("extends"));
    assert!(label.contains("declared"));
    assert!(label.contains("depth=2"));
    assert_eq!(
        node(&value, "pkg::Base")["hit"]["externalTarget"],
        "pkg::Base"
    );
    assert!(node(&value, "pkg::Base")["hit"]["path"].is_null());
    assert!(
        value["caveats"]
            .as_array()
            .unwrap()
            .contains(&json!("Unavailable package graph"))
    );
    assert_eq!(node(&value, "Child")["hit"]["startLine"], 5);
}

#[test]
fn clusters_are_membership_only_and_unused_preserves_current_and_legacy_page_facts() {
    let clusters = [api::GraphCluster {
        index: 7,
        size: 3,
        chunks: vec![hit("a"), hit("b")],
        hash: Some("same-hash".into()),
    }];
    let value = view(GraphInput::Clusters {
        label: "Duplicate groups",
        clusters: &clusters,
    });
    assert_eq!(value["edges"].as_array().unwrap().len(), 2);
    assert!(
        value["edges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|edge| edge["label"] == "member")
    );
    assert!(!has_edge(&value, "a", "b"));
    assert!(
        node(&value, "cluster:7")["detail"]
            .as_str()
            .unwrap()
            .contains("same-hash")
    );
    for carrier in ["data", "items"] {
        let mut source = json!({"total":9,"page":2,"pageSize":1});
        source[carrier] = json!([{
            "definition":{"domainId":"code","id":"unused","score":0,"snippet":"","symbol":"unused"},
            "reason":"No resolved incoming references", "completenessCaveat":"Dynamic references are not covered"
        }]);
        let page: api::Page<api::UnusedDefinition> = serde_json::from_value(source).unwrap();
        let value = view(GraphInput::Unused { page: &page });
        assert!(value["edges"].as_array().unwrap().is_empty());
        assert_eq!(
            node(&value, "unused")["detail"],
            "No resolved incoming references"
        );
        assert!(
            value["caveats"]
                .as_array()
                .unwrap()
                .contains(&json!("Dynamic references are not covered"))
        );
    }
}

#[test]
fn file_and_external_relationships_preserve_source_direction_and_target_identity() {
    let imports = [api::ImportEdge {
        from: "a.rs".into(),
        to: "b.rs".into(),
        import_path: "crate::b".into(),
    }];
    let value = view(GraphInput::Imports {
        edges: &imports,
        total: 10,
    });
    assert!(has_edge(&value, "a.rs", "b.rs"));
    assert_eq!(value["edges"][0]["label"], "imports crate::b");
    let symbols = [api::SymbolEdge {
        from_file: "a.rs".into(),
        to_file: "b.rs".into(),
        name: "run".into(),
        moniker: "crate::run".into(),
    }];
    let value = view(GraphInput::SymbolEdges {
        edges: &symbols,
        total: 1,
    });
    assert!(has_edge(&value, "a.rs", "b.rs#crate::run"));
    assert_eq!(
        node(&value, "b.rs#crate::run")["hit"]["qualifiedName"],
        "crate::run"
    );
    let links = [api::ExternalLink {
        from_file: "a.rs".into(),
        import_path: "dep::run".into(),
        ecosystem: "cargo".into(),
        target_package: "dep@1".into(),
        descriptor: "dep::run".into(),
        target_codebase_id: "B".into(),
        target_file: "lib.rs".into(),
        target_name: "run".into(),
        confidence: 91,
    }];
    let value = view(GraphInput::ExternalLinks {
        links: &links,
        total: 1,
    });
    let target = node(&value, "B:lib.rs#dep::run");
    assert!(has_edge(&value, "a.rs", "B:lib.rs#dep::run"));
    assert_eq!(target["hit"]["codebaseId"], "B");
    assert_eq!(target["hit"]["externalTarget"], "dep::run");
    assert!(target["detail"].as_str().unwrap().contains("confidence=91"));
    assert!(target["detail"].as_str().unwrap().contains("dep@1"));
    assert!(value["identity"]["localRoot"].is_null());
}

#[test]
fn project_membership_and_outline_depth_do_not_invent_parentage_or_source_ranges() {
    let projects: api::ProjectGraph = serde_json::from_value(json!({
        "projects":[{"kind":"cargo","name":"app","root":"app","configFile":"app/Cargo.toml","fileCount":8}],
        "groups":[{"kind":"workspace","name":"root","root":".","children":["app","unknown"]}]
    })).unwrap();
    let value = view(GraphInput::Projects(&projects));
    assert!(has_edge(&value, "group:.", "project:app"));
    assert!(has_edge(&value, "group:.", "child:unknown"));
    assert_eq!(node(&value, "project:app")["hit"]["project"], "app");
    assert_eq!(node(&value, "project:app")["hit"]["path"], "app/Cargo.toml");
    assert!(node(&value, "project:app")["hit"]["startLine"].is_null());
    let outline: api::FileOutline = serde_json::from_value(json!({
        "path":"src/lib.rs","entries":[
            {"chunkId":"parent","lineStart":1,"lineEnd":9,"kind":"container","depth":0},
            {"chunkId":"child","lineStart":3,"lineEnd":4,"kind":"function","symbol":"run","depth":1,"body":"fn run() {}"}
        ]
    })).unwrap();
    let value = view(GraphInput::Outline(&outline));
    assert!(value["edges"].as_array().unwrap().is_empty());
    assert_eq!(node(&value, "child")["hit"]["snippet"], "fn run() {}");
    assert_eq!(node(&value, "child")["hit"]["startLine"], 3);
    assert!(node(&value, "child")["hit"]["startByte"].is_null());
    assert!(
        node(&value, "child")["detail"]
            .as_str()
            .unwrap()
            .contains("depth=1")
    );
}

#[test]
fn bounds_keep_real_endpoints_and_allow_structural_views_without_large_previews() {
    let nodes: Vec<_> = (0..70).map(|index| hit(&index.to_string())).collect();
    let edges: Vec<_> = (0..140)
        .map(|index| api::CallEdge {
            from: (index / 63).to_string(),
            to: (index % 63).to_string(),
        })
        .collect();
    let graph = api::CallGraph { nodes, edges };
    let value = view(GraphInput::CallGraph {
        symbol: "0",
        graph: &graph,
    });
    assert_eq!(value["nodes"].as_array().unwrap().len(), MAX_NODES);
    assert_eq!(value["edges"].as_array().unwrap().len(), MAX_EDGES);
    assert_eq!(value["truncated"], true);
    let mut huge = hit("huge");
    huge.snippet = "full source\n".repeat(MAX_DISPLAY_BYTES);
    let original = huge.snippet.clone();
    let graph = api::CallGraph {
        nodes: vec![huge],
        edges: vec![],
    };
    let value = view(GraphInput::CallGraph {
        symbol: "huge",
        graph: &graph,
    });
    assert!(node(&value, "huge")["hit"].is_null());
    assert_eq!(graph.nodes[0].snippet, original);
    let client = Client::for_test("A", None);
    let empty = api::CallGraph {
        nodes: vec![],
        edges: vec![],
    };
    assert!(
        project_graph(
            &client,
            GraphInput::CallGraph {
                symbol: "missing",
                graph: &empty
            }
        )
        .is_none()
    );
    let mut huge = hit("huge-label");
    huge.qualified_symbol = Some("x".repeat(MAX_DISPLAY_BYTES + 1));
    let graph = api::CallGraph {
        nodes: vec![huge],
        edges: vec![],
    };
    assert!(
        project_graph(
            &client,
            GraphInput::CallGraph {
                symbol: "huge",
                graph: &graph
            }
        )
        .is_none()
    );
    let mut escaped = hit("escaped");
    escaped.qualified_symbol = Some("\n".repeat(MAX_DISPLAY_BYTES / 2));
    let graph = api::CallGraph {
        nodes: vec![escaped],
        edges: vec![],
    };
    assert!(
        project_graph(
            &client,
            GraphInput::CallGraph {
                symbol: "escaped",
                graph: &graph
            }
        )
        .is_none()
    );
}

#[test]
fn dangling_source_endpoints_are_not_replaced_with_invented_nodes_or_edges() {
    let graph = api::CallGraph {
        nodes: vec![hit("known")],
        edges: vec![api::CallEdge {
            from: "known".into(),
            to: "missing-source-node".into(),
        }],
    };
    let value = view(GraphInput::CallGraph {
        symbol: "known",
        graph: &graph,
    });
    assert_eq!(value["nodes"].as_array().unwrap().len(), 1);
    assert!(value["edges"].as_array().unwrap().is_empty());
    assert_eq!(value["truncated"], true);
}
