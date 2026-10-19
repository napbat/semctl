use serde_json::{Value, json};

use super::*;

fn projects() -> api::ProjectGraph {
    serde_json::from_value(json!({"projects":[{"kind":"cargo","name":"library","root":"src","configFile":null,"fileCount":2}],
        "groups":[{"kind":"workspace","name":"workspace","root":".","children":["src"]}]})).unwrap()
}
fn tree() -> Vec<api::FileTreeNode> {
    serde_json::from_value(json!([
        {"name":"README.md","path":"README.md","isDirectory":false,"size":2},
        {"name":"src","path":"src","isDirectory":true,"children":[
            {"name":"main.rs","path":"src/main.rs","isDirectory":false,"size":12},
            {"name":"lib.rs","path":"src/lib.rs","isDirectory":false,"size":10}
        ]}
    ]))
    .unwrap()
}
fn node<'a>(value: &'a Value, category: &str, path: &str) -> &'a Value {
    value["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["category"] == category && node["path"] == path)
        .unwrap()
}
fn linked(value: &Value, from: &Value, to: &Value, label: &str) -> bool {
    value["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|edge| edge["from"] == from["id"] && edge["to"] == to["id"] && edge["label"] == label)
}

#[test]
fn retains_isolated_root_files_directory_containment_and_actual_parallel_self_imports() {
    let client = Client::for_test("A", None);
    let imports: Vec<api::ImportEdge> = serde_json::from_value(json!([
        {"from":"src/main.rs","to":"src/lib.rs","importPath":"crate::library"},
        {"from":"src/main.rs","to":"src/lib.rs","importPath":"crate::alias"},
        {"from":"src/main.rs","to":"src/main.rs","importPath":"self"}
    ]))
    .unwrap();
    let value = project_overview(&client, &tree(), &projects(), &imports, 3, &[], 0).unwrap();
    let root = node(&value, "directory", "");
    let source = node(&value, "directory", "src");
    let main = node(&value, "file", "src/main.rs");
    let lib = node(&value, "file", "src/lib.rs");
    let isolated = node(&value, "file", "README.md");
    assert!(linked(&value, root, isolated, "contains"));
    assert!(linked(&value, root, source, "contains"));
    assert!(linked(&value, source, main, "contains"));
    assert!(linked(&value, main, lib, "imports · crate::library"));
    assert!(linked(&value, main, lib, "imports · crate::alias"));
    assert!(linked(&value, main, main, "imports · self"));
    let project = node(&value, "project", "src");
    assert!(linked(&value, project, source, "project root"));
    assert!(linked(&value, project, main, "within project root"));
    assert!(linked(
        &value,
        node(&value, "group", ""),
        project,
        "contains reported child"
    ));
    assert_eq!(main["project"], "src");
    assert!(isolated.get("project").is_none());
    assert_eq!(value["truncated"], false);
    assert!(
        value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node["hit"].is_null())
    );
    assert_eq!(
        value,
        project_overview(&client, &tree(), &projects(), &imports, 3, &[], 0).unwrap()
    );
}

#[test]
fn resolves_reported_manifest_children_to_their_project_without_guessing_a_root() {
    let client = Client::for_test("A", None);
    let mut source = projects();
    source.projects[0].config_file = Some("src/Cargo.toml".into());
    source.groups[0].children = vec!["src/Cargo.toml".into()];
    let value = project_overview(&client, &tree(), &source, &[], 0, &[], 0).unwrap();
    assert!(linked(
        &value,
        node(&value, "group", ""),
        node(&value, "project", "src"),
        "contains reported child",
    ));
    assert!(
        value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node["category"].is_string())
    );
}

#[test]
fn thousands_of_isolated_files_survive_beyond_the_mcp_carrier_budget() {
    let client = Client::for_test("A", None);
    let files: Vec<_> = (0..5000)
        .map(|index| api::FileTreeNode {
            name: format!("file-{index}.rs"),
            path: format!("file-{index}.rs"),
            is_directory: false,
            size: Some(0),
            children: None,
        })
        .collect();
    let empty = api::ProjectGraph {
        projects: vec![],
        groups: vec![],
    };
    let value = project_overview(&client, &files, &empty, &[], 0, &[], 0).unwrap();
    assert_eq!(value["nodes"].as_array().unwrap().len(), 5001);
    assert_eq!(value["edges"].as_array().unwrap().len(), 5000);
    assert!(node(&value, "file", "file-4999.rs")["hit"].is_null());
    let bytes = serde_json::to_vec(&value).unwrap().len();
    assert!(bytes > super::super::MAX_DISPLAY_BYTES && bytes <= MAX_OVERVIEW_BYTES);
}

#[test]
fn oversized_nodes_edges_or_bytes_fail_instead_of_returning_partial_success() {
    let client = Client::for_test("A", None);
    let empty = api::ProjectGraph {
        projects: vec![],
        groups: vec![],
    };
    let files: Vec<_> = (0..MAX_OVERVIEW_NODES)
        .map(|index| api::FileTreeNode {
            name: format!("{index}.rs"),
            path: format!("{index}.rs"),
            is_directory: false,
            size: None,
            children: None,
        })
        .collect();
    assert!(
        project_overview(&client, &files, &empty, &[], 0, &[], 0)
            .unwrap_err()
            .summary()
            .contains("12000-node")
    );
    let huge: Vec<_> = (0..800)
        .map(|index| {
            let path = format!("{}-{index}.rs", "x".repeat(3000));
            api::FileTreeNode {
                name: path.clone(),
                path,
                is_directory: false,
                size: None,
                children: None,
            }
        })
        .collect();
    assert!(
        project_overview(&client, &huge, &empty, &[], 0, &[], 0)
            .unwrap_err()
            .summary()
            .contains("3 MiB")
    );
    let imports: Vec<_> = (0..MAX_OVERVIEW_EDGES)
        .map(|_| api::ImportEdge {
            from: "0.rs".into(),
            to: "0.rs".into(),
            import_path: "self".into(),
        })
        .collect();
    assert!(
        project_overview(
            &client,
            &files[..1],
            &empty,
            &imports,
            u32::try_from(imports.len()).unwrap(),
            &[],
            0,
        )
        .unwrap_err()
        .summary()
        .contains("48000-edge")
    );
}

#[test]
fn corrupt_tree_missing_import_endpoints_and_incomplete_import_pages_are_errors() {
    let client = Client::for_test("A", None);
    for bad in [
        json!([{"name":"bad.rs","path":"../bad.rs","isDirectory":false}]),
        json!([{"name":"a","path":"a","isDirectory":false},{"name":"a","path":"a","isDirectory":false}]),
        json!([{"name":"src","path":"src","isDirectory":true,"children":[{"name":"x.rs","path":"other/x.rs","isDirectory":false}]}]),
        json!([{"name":"a","path":"a","isDirectory":false,"size":-1}]),
        json!([{"name":"a","path":"a","isDirectory":false,"children":[{"name":"x","path":"a/x","isDirectory":false}]}]),
    ] {
        let bad: Vec<api::FileTreeNode> = serde_json::from_value(bad).unwrap();
        assert!(project_overview(&client, &bad, &projects(), &[], 0, &[], 0).is_err());
    }
    let import = api::ImportEdge {
        from: "src/main.rs".into(),
        to: "missing.rs".into(),
        import_path: "missing".into(),
    };
    assert!(
        project_overview(&client, &tree(), &projects(), &[import], 1, &[], 0)
            .unwrap_err()
            .summary()
            .contains("absent")
    );
    assert!(
        project_overview(&client, &tree(), &projects(), &[], 1, &[], 0)
            .unwrap_err()
            .summary()
            .contains("incomplete")
    );
}

#[test]
fn unresolved_project_child_tokens_are_preserved_without_inventing_project_identity() {
    let client = Client::for_test("A", None);
    let mut source = projects();
    source.groups[0].children.push("unknown".into());
    let value = project_overview(&client, &tree(), &source, &[], 0, &[], 0).unwrap();
    let token = value["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["path"] == "unknown")
        .unwrap();
    assert!(token.get("category").is_none());
    assert!(linked(
        &value,
        node(&value, "group", ""),
        token,
        "contains reported child"
    ));
    assert!(
        value["caveats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|text| text.as_str().unwrap().contains("unresolved"))
    );
}

#[test]
fn reference_bindings_preserve_exact_names_monikers_parallel_duplicates_and_self_links() {
    let client = Client::for_test("A", None);
    let references: Vec<api::SymbolEdge> = serde_json::from_value(json!([
        {"fromFile":"src/main.rs","toFile":"src/lib.rs","name":"WireFormat","moniker":"src/lib.rs#10"},
        {"fromFile":"src/main.rs","toFile":"src/lib.rs","name":"WireFormat","moniker":"src/lib.rs#20"},
        {"fromFile":"src/main.rs","toFile":"src/lib.rs","name":"WireFormat","moniker":"src/lib.rs#10"},
        {"fromFile":"src/main.rs","toFile":"src/main.rs","name":"response","moniker":"src/main.rs#4"}
    ])).unwrap();
    let value = project_overview(&client, &tree(), &projects(), &[], 0, &references, 4).unwrap();
    let edges: Vec<_> = value["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|edge| edge["kind"] == "reference")
        .collect();
    assert_eq!(edges.len(), 4);
    let main = node(&value, "file", "src/main.rs");
    let lib = node(&value, "file", "src/lib.rs");
    for edge in &edges[..3] {
        assert_eq!(edge["from"], main["id"]);
        assert_eq!(edge["to"], lib["id"]);
        assert_eq!(edge["label"], "references");
        assert_eq!(edge["symbol"], "WireFormat");
    }
    assert_eq!(edges[0]["moniker"], "src/lib.rs#10");
    assert_eq!(edges[1]["moniker"], "src/lib.rs#20");
    assert_eq!(edges[0], edges[2]);
    assert_eq!(edges[3]["from"], main["id"]);
    assert_eq!(edges[3]["to"], main["id"]);
    assert_eq!(edges[3]["symbol"], "response");
    assert!(node(&value, "file", "README.md")["id"].is_string());
    for edge in value["edges"].as_array().unwrap() {
        assert!(edge["kind"].is_string());
        if edge["kind"] != "reference" {
            assert!(edge.get("symbol").is_none());
            assert!(edge.get("moniker").is_none());
        }
    }
}

#[test]
fn references_require_complete_catalog_scoped_and_bounded_evidence() {
    let client = Client::for_test("A", None);
    assert!(
        project_overview(&client, &tree(), &projects(), &[], 0, &[], 1)
            .unwrap_err()
            .summary()
            .contains("symbol bindings are incomplete")
    );
    for (from, to, name, moniker) in [
        ("src/main.rs", "missing.rs", "run", "missing.rs#1"),
        ("src/main.rs", "src", "run", "src#1"),
        ("../outside.rs", "src/lib.rs", "run", "src/lib.rs#1"),
        ("src/main.rs", "src/lib.rs", "", "src/lib.rs#1"),
        ("src/main.rs", "src/lib.rs", "run", ""),
        ("src/main.rs", "src/lib.rs", "run\u{0000}", "src/lib.rs#1"),
    ] {
        let reference = api::SymbolEdge {
            from_file: from.into(),
            to_file: to.into(),
            name: name.into(),
            moniker: moniker.into(),
        };
        assert!(project_overview(&client, &tree(), &projects(), &[], 0, &[reference], 1).is_err());
    }
    let reference = api::SymbolEdge {
        from_file: "src/main.rs".into(),
        to_file: "src/lib.rs".into(),
        name: "run".into(),
        moniker: "x".repeat(4097),
    };
    assert!(
        project_overview(&client, &tree(), &projects(), &[], 0, &[reference], 1)
            .unwrap_err()
            .summary()
            .contains("evidence")
    );
}
