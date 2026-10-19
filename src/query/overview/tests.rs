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

#[tokio::test]
async fn overview_fetches_all_import_pages_and_preserves_isolated_files_without_source_requests() {
    let stub = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
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
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let output = overview(&client, QueryPresentation::Inspector)
        .await
        .unwrap();
    let display = output.display.unwrap();
    assert_eq!(display["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(
        display["edges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|edge| edge["label"].as_str().unwrap().starts_with("imports"))
            .count(),
        2
    );
    assert!(
        display["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["path"] == "isolated.rs")
    );
    let heads = stub.request_lines();
    assert_eq!(heads.len(), 6);
    assert!(heads[0].starts_with("GET /v1/codebases/A/files/tree "));
    assert!(heads[1].starts_with("GET /v1/codebases/A/projects "));
    assert!(heads[2].contains("/graph/imports?page=0&pageSize=1000"));
    assert!(heads[3].contains("/graph/imports?page=1&pageSize=1000"));
    assert!(heads[4].contains("/graph/symbol-edges?page=0&pageSize=1000"));
    assert!(heads[5].contains("/graph/symbol-edges?page=1&pageSize=1000"));
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
    assert!(output.text.contains("2 resolved symbol bindings"));
    assert!(output.text.contains("3 files"));
}

#[tokio::test]
async fn incomplete_imports_and_backend_capability_failures_are_not_successful_empty_graphs() {
    let stub = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
        page(&json!([]), 1, 0),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    assert!(
        overview(&client, QueryPresentation::Inspector)
            .await
            .unwrap_err()
            .summary()
            .contains("incomplete")
    );
    let missing = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
        Reply::gateway("404 Not Found"),
    ])
    .await;
    let client = Client::for_test_server(&missing.url).with_codebase("A".into());
    assert!(
        overview(&client, QueryPresentation::Inspector)
            .await
            .is_err()
    );
    assert_eq!(missing.requests(), 3);
}

#[tokio::test]
async fn missing_or_incomplete_symbol_bindings_never_fall_back_to_import_only_concepts() {
    let incomplete = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
        page(&json!([]), 0, 0),
        page(&json!([]), 2, 0),
    ])
    .await;
    let client = Client::for_test_server(&incomplete.url).with_codebase("A".into());
    let error = overview(&client, QueryPresentation::Inspector)
        .await
        .unwrap_err()
        .summary();
    assert!(error.contains("Resolved symbol bindings are incomplete"));
    let missing = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
        page(&json!([]), 0, 0),
        Reply::gateway("404 Not Found"),
    ])
    .await;
    let client = Client::for_test_server(&missing.url).with_codebase("A".into());
    assert!(
        overview(&client, QueryPresentation::Inspector)
            .await
            .is_err()
    );
    assert_eq!(missing.requests(), 4);
    assert!(missing.request_lines()[3].contains("/graph/symbol-edges?"));
}

#[tokio::test]
async fn truly_empty_semantic_index_is_reported_as_zero_bindings_not_inferred_concepts() {
    let stub = Stub::serve(vec![
        Reply::ok(TREE),
        Reply::ok(PROJECTS),
        page(&json!([]), 0, 0),
        page(&json!([]), 0, 0),
    ])
    .await;
    let client = Client::for_test_server(&stub.url).with_codebase("A".into());
    let output = overview(&client, QueryPresentation::Inspector)
        .await
        .unwrap();
    assert!(output.text.contains("0 resolved symbol bindings"));
    assert!(
        output.display.unwrap()["edges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|edge| edge["kind"] == "containment")
    );
}
