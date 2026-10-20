use serde_json::json;

use super::*;
use crate::client::stub::{Reply, Stub};

const TREE: &str = r#"[{"name":"a.rs","path":"a.rs","isDirectory":false},{"name":"b.rs","path":"b.rs","isDirectory":false},{"name":"isolated.rs","path":"isolated.rs","isDirectory":false}]"#;
const PROJECTS: &str = r#"{"projects":[],"groups":[]}"#;
fn page(data: &serde_json::Value, total: u32, number: u32) -> Reply {
    Reply::Answer {
        status: "200 OK",
        headers: Vec::new(),
        body: json!({"success":true,"data":data,"total":total,"page":number,"pageSize":1000})
            .to_string(),
    }
}
fn files() -> Reply {
    page(
        &json!([
            {"path":"a.rs","size":1,"status":"Pending"},
            {"path":"b.rs","size":1,"status":"Failed"},
            {"path":"isolated.rs","size":1,"status":"Embedded"}
        ]),
        3,
        0,
    )
}
fn failure(status: &'static str, code: &str) -> Reply {
    Reply::Answer {
        status,
        headers: Vec::new(),
        body: json!({"success":false,"errors":[{"code":code}]}).to_string(),
    }
}
fn node<'a>(display: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    display["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["path"] == path)
        .unwrap()
}

#[tokio::test]
async fn pending_and_failed_embeddings_preserve_all_available_structure_and_exact_bindings() {
    let stub = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(
            r#"{"projects":[{"kind":"cargo","name":"root","root":".","fileCount":3}],"groups":[]}"#,
        ),
        page(
            &json!([{"from":"a.rs","to":"b.rs","importPath":"first"}]),
            2,
            0,
        ),
        page(
            &json!([{"from":"b.rs","to":"a.rs","importPath":"back"}]),
            2,
            1,
        ),
        page(
            &json!([{"fromFile":"a.rs","toFile":"b.rs","name":"First","moniker":"b.rs#10"}]),
            2,
            0,
        ),
        page(
            &json!([{"fromFile":"b.rs","toFile":"a.rs","name":"Back","moniker":"a.rs#20"}]),
            2,
            1,
        ),
        files(),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let output = overview(&client, QueryPresentation::Inspector)
        .await
        .unwrap();
    let display = output.display.unwrap();
    assert_eq!(display["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(node(&display, "a.rs")["indexing"], "pending");
    assert_eq!(node(&display, "b.rs")["indexing"], "failed");
    assert_eq!(node(&display, "isolated.rs")["indexing"], "ready");
    assert!(node(&display, "b.rs").get("indexingError").is_none());
    let references: Vec<_> = display["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|edge| edge["kind"] == "reference")
        .collect();
    assert_eq!(references.len(), 2);
    assert_eq!(references[0]["symbol"], "First");
    assert_eq!(references[0]["moniker"], "b.rs#10");
    assert_eq!(references[1]["symbol"], "Back");
    assert!(output.text.contains("2 of 2 resolved symbol bindings"));
    assert!(output.text.contains("3 files"));
    assert_eq!(display["truncated"], true);
    assert!(display.get("loading").is_none());
    let heads = stub.request_lines();
    assert_eq!(heads.len(), 7);
    assert!(heads[0].starts_with("GET /v1/codebases/A/files/tree "));
    assert!(heads[1].starts_with("GET /v1/codebases/A/projects "));
    assert!(heads[2].contains("/graph/imports?page=0&pageSize=1000"));
    assert!(heads[3].contains("/graph/imports?page=1&pageSize=1000"));
    assert!(heads[4].contains("/graph/symbol-edges?page=0&pageSize=1000"));
    assert!(heads[5].contains("/graph/symbol-edges?page=1&pageSize=1000"));
    assert!(heads[6].contains("/files?page=0&pageSize=1000"));
}

#[tokio::test]
async fn loading_layers_return_catalog_immediately_without_waiting_or_inventing_status() {
    let stub = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::loading("5"),
        Reply::loading("5"),
        Reply::loading("5"),
        page(
            &json!([{ "path":"a.rs", "size":1, "status":987 }, { "path":"b.rs", "size":1 }]),
            2,
            0,
        ),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        overview(&client, QueryPresentation::Inspector),
    )
    .await
    .unwrap()
    .unwrap();
    let display = output.display.unwrap();
    assert_eq!(display["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(display["truncated"], true);
    assert_eq!(display["loading"], true);
    assert_eq!(node(&display, "a.rs")["indexing"], "unknown");
    assert!(node(&display, "b.rs").get("indexing").is_none());
    assert!(node(&display, "isolated.rs").get("indexing").is_none());
    assert!(output.text.contains("Projects unavailable"));
    assert!(output.text.contains("Resolved imports unavailable"));
    assert!(output.text.contains("Resolved symbol bindings unavailable"));
    assert_eq!(stub.requests(), 5);
}

#[tokio::test]
async fn incomplete_pages_preserve_available_relationships_with_explicit_caveats() {
    let stub = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
        page(
            &json!([{"from":"a.rs","to":"b.rs","importPath":"kept"}]),
            2,
            0,
        ),
        Reply::loading("1"),
        page(&json!([]), 2, 0),
        files(),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let output = overview(&client, QueryPresentation::Inspector)
        .await
        .unwrap();
    let display = output.display.unwrap();
    assert_eq!(display["truncated"], true);
    assert_eq!(display["loading"], true);
    assert!(
        display["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| edge["label"] == "imports · kept")
    );
    assert!(output.text.contains("Resolved imports incomplete"));
    assert!(output.text.contains("Resolved symbol bindings incomplete"));
}

#[tokio::test]
async fn catalog_files_survive_when_the_tree_is_temporarily_unavailable() {
    let stub = Stub::serve(vec![
        Reply::loading("1"),
        Reply::ok(PROJECTS),
        page(&json!([]), 0, 0),
        page(&json!([]), 0, 0),
        files(),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let output = overview(&client, QueryPresentation::Inspector)
        .await
        .unwrap();
    let display = output.display.unwrap();
    assert_eq!(display["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(display["loading"], true);
    assert_eq!(node(&display, "a.rs")["indexing"], "pending");
    assert_eq!(display["edges"].as_array().unwrap().len(), 0);
    assert!(output.text.contains("File tree unavailable"));
}

#[tokio::test]
async fn auth_transport_unknown_conflicts_unsupported_routes_and_invalid_payloads_remain_fatal() {
    for reply in [
        failure("401 Unauthorized", "Unauthorized"),
        failure("403 Forbidden", "Forbidden"),
        failure("404 Not Found", "NotFound"),
        failure("409 Conflict", "StaleRevision"),
        Reply::gateway("502 Bad Gateway"),
        Reply::ok(r#"{"not":"a project graph"}"#),
    ] {
        let stub = Stub::serve(vec![Reply::ok(TREE), reply]).await;
        let client = Client::for_test_server(&stub.url).with_codebase("A".into());
        assert!(
            overview(&client, QueryPresentation::Inspector)
                .await
                .is_err()
        );
    }
    let stub = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
        page(
            &json!([{"from":"../bad","to":"a.rs","importPath":"bad"}]),
            1,
            0,
        ),
        page(&json!([]), 0, 0),
        files(),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    assert!(
        overview(&client, QueryPresentation::Inspector)
            .await
            .unwrap_err()
            .summary()
            .contains("invalid codebase-relative path")
    );
}

#[tokio::test]
async fn truly_empty_relationship_layers_are_reported_as_zero_not_as_unavailable() {
    let stub = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
        page(&json!([]), 0, 0),
        page(&json!([]), 0, 0),
        page(&json!([]), 0, 0),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let output = overview(&client, QueryPresentation::Inspector)
        .await
        .unwrap();
    assert!(output.text.contains("0 of 0 resolved symbol bindings"));
    assert!(!output.text.contains("unavailable"));
    assert_eq!(output.display.unwrap()["truncated"], false);
}

#[tokio::test]
async fn incomplete_pages_without_typed_loading_do_not_request_loading_refresh() {
    let stub = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
        page(&json!([]), 2, 0),
        page(&json!([]), 0, 0),
        files(),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let output = overview(&client, QueryPresentation::Inspector)
        .await
        .unwrap();
    let display = output.display.unwrap();
    assert_eq!(display["truncated"], true);
    assert!(display.get("loading").is_none());
}
