use serde_json::json;

use super::*;
use crate::client::stub::{Reply, Stub};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn headings_skip_fences_and_strip_inline_markdown() {
    let text = "\
# Intel® SDM — [VMCS](https://x/y) `layout` ##
Body text.
```c
# not a heading
~~~
```
~~~~
## also hidden
~~~~
   ### **Bold** and _emphasis_ <sup>1</sup> snake_case
#### Level four ![alt text](img.png)
##### Level five
#NoSpace
    # indented code
## Escaped \\# hash [ref link][r]
";
    assert_eq!(
        markdown_headings(text),
        [
            "Intel® SDM — VMCS layout",
            "Bold and emphasis 1 snake_case",
            "Level four alt text",
            "Escaped # hash ref link",
        ]
    );
}

#[test]
fn heading_query_drops_letter_duplicates_and_short_headings() {
    let text = "# PDF page 71\n# PDF page 72\n## pdf-PAGE 73\n# A1\n# VM entry\n# ab\n";
    assert_eq!(
        heading_query(text).as_deref(),
        Some("PDF page 71; VM entry")
    );
    assert_eq!(heading_query("no headings\n"), None);
}

#[test]
fn heading_query_keeps_sixteen_terms_and_caps_characters() {
    let many: String = (0..20).fold(String::new(), |mut text, index| {
        text.push_str("# Section ");
        text.push(char::from(b'a' + index));
        text.push('\n');
        text
    });
    let query = heading_query(&many).unwrap();
    assert_eq!(query.split("; ").count(), MAX_TERMS);
    assert!(query.ends_with("Section p"));

    let long: String = (0..16).fold(String::new(), |mut text, index| {
        text.push_str("# ");
        text.push(char::from(b'a' + index));
        text.push_str(&"é".repeat(60));
        text.push('\n');
        text
    });
    let query = heading_query(&long).unwrap();
    assert_eq!(query.chars().count(), MAX_QUERY_CHARS);
}

#[test]
fn outline_names_drop_qualifiers_and_duplicates() {
    let outline: api::FileOutline = serde_json::from_value(json!({
        "path": "src/vmx.cpp",
        "entries": [
            {"chunkId":"1","lineStart":1,"lineEnd":2,"kind":"function","symbol":"hv::vmx::launch"},
            {"chunkId":"2","lineStart":3,"lineEnd":4,"kind":"block","symbol":null},
            {"chunkId":"3","lineStart":5,"lineEnd":6,"kind":"function","symbol":"launch"},
            {"chunkId":"4","lineStart":7,"lineEnd":8,"kind":"container","qualifiedSymbol":"hv::Vmcs"}
        ]
    }))
    .unwrap();
    assert_eq!(outline_query(&outline).as_deref(), Some("launch; Vmcs"));
    let empty = api::FileOutline {
        path: "x".into(),
        entries: Vec::new(),
    };
    assert_eq!(outline_query(&empty), None);
}

#[test]
fn name_query_humanizes_stem_and_parent() {
    assert_eq!(name_query("hv/src/vmx/VmcsFields.cpp"), "Vmcs Fields vmx");
    assert_eq!(
        name_query("docs/intel/extracts/sdm_vol3-ch24.md"),
        "sdm vol3 ch24 extracts"
    );
    assert_eq!(name_query("README"), "README");
    assert_eq!(name_query("x/.env"), "env x");
}

#[test]
fn markdown_extensions_are_case_insensitive() {
    assert!(is_markdown("a/B.MD"));
    assert!(is_markdown("a.markdown"));
    assert!(is_markdown("a.Mdx"));
    assert!(!is_markdown("a.rs"));
    assert!(!is_markdown("md"));
}

#[test]
fn paths_are_validated_and_deduplicated_in_first_order() {
    let paths = strings(&["b.md", "a.md", "b.md", "c/d.rs"]);
    assert_eq!(requested_paths(&paths).unwrap(), ["b.md", "a.md", "c/d.rs"]);
    for bad in ["", "a\\b", "c:/x", "a//b", "./a", "a/../b", "a/", "a\u{7}b"] {
        let error = requested_paths(&strings(&["ok.md", bad])).unwrap_err();
        assert!(
            error
                .to_string()
                .ends_with("next: correct the argument and call again."),
            "{bad:?}"
        );
    }
    let long = "a".repeat(MAX_PATH_BYTES + 1);
    assert!(requested_paths(&[long, "b".into()]).is_err());
    assert!(requested_paths(&strings(&["a.md"])).is_err());
    assert!(requested_paths(&strings(&["a.md", "a.md"])).is_err());
    let many: Vec<String> = (0..=MAX_FILES).map(|index| format!("{index}.md")).collect();
    assert!(requested_paths(&many).is_err());
    assert!(requested_paths(&many[..MAX_FILES]).is_ok());
}

#[test]
fn ranked_paths_are_distinct_in_rank_order_and_weighted_by_root_rank() {
    let ranked = strings(&["a", "b", "c", "d"]);
    let weights = profile(&ranked);
    assert!((weights["a"] - 1.0).abs() < 1e-12);
    assert!((weights["d"] - 0.5).abs() < 1e-12);
    assert!((weights["b"] - 1.0 / 2.0_f64.sqrt()).abs() < 1e-12);
}

#[test]
fn pairs_use_direct_hits_and_shared_profiles_without_the_pair() {
    // The pair's own paths dominate both profiles, but `shared` ignores them.
    let first = strings(&["b.md", "a.md", "x", "y"]);
    let second = strings(&["a.md", "b.md", "x", "y"]);
    let third = strings(&["z", "w"]);
    let files = ["b.md", "a.md", "c.md", "d.md"];
    let profiles = [
        Some(profile(&first)),
        Some(profile(&second)),
        Some(profile(&third)),
        None,
    ];
    let pairs = related_pairs(&files, &profiles);
    let expected_direct = round4(std::f64::consts::FRAC_1_SQRT_2);
    assert_eq!(
        pairs,
        [AffinityPair {
            a: "a.md",
            b: "b.md",
            direct: expected_direct,
            shared: 1.0,
        }]
    );
}

#[test]
fn pairs_below_the_shared_threshold_without_direct_hits_are_dropped() {
    // Only one deep-ranked path is common, so the cosine stays below the threshold.
    let mut deep: Vec<String> = (0..60).map(|index| format!("p{index}")).collect();
    deep.push("common".into());
    let mut other: Vec<String> = (0..60).map(|index| format!("q{index}")).collect();
    other.push("common".into());
    let profiles = [Some(profile(&deep)), Some(profile(&other))];
    let value = shared(
        profiles[0].as_ref().unwrap(),
        profiles[1].as_ref().unwrap(),
        ["a", "b"],
    );
    assert!(value > 0.0 && value < SHARED_THRESHOLD);
    assert_eq!(related_pairs(&["a", "b"], &profiles), []);
    assert!((round4(0.123_456) - 0.1235).abs() < 1e-12);
    assert!((round4(1.000_000_1) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn summary_distinguishes_search_misses_empty_results_and_loading() {
    let ranked = strings(&["a.md", "other.md"]);
    let entries = [
        AffinityFile::new("a.md", QuerySource::Content, Some(&ranked)),
        AffinityFile::new("b.md", QuerySource::Content, Some(&ranked)),
        AffinityFile::new("c.md", QuerySource::Content, Some(&[])),
        AffinityFile::new("d.md", QuerySource::Content, None),
    ];
    assert_eq!(
        summary(&entries, 1, 1),
        "Semantic affinity: 4 files, 1 related pair. Input files returned: 1 of 3 completed searches. 1 search returned no result files. 1 search was unavailable while the backend loads."
    );
}

fn search_reply(paths: &[&str]) -> Reply {
    let hits: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| json!({"domainId":"code","id":index.to_string(),"score":1.0,"path":path,"snippet":""}))
        .collect();
    Reply::ok(&serde_json::Value::Array(hits).to_string())
}

fn checkout() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("docs")).unwrap();
    std::fs::write(
        directory.path().join("docs/a.md"),
        "# VMCS layout\n## Guest state\n",
    )
    .unwrap();
    std::fs::write(directory.path().join("docs/b.md"), "# VM-entry checks\n").unwrap();
    directory
}

#[tokio::test]
async fn command_profiles_files_and_falls_back_from_a_missing_outline_to_the_name() {
    let directory = checkout();
    let stub = Stub::serve(vec![
        Reply::Answer {
            status: "404 Not Found",
            headers: Vec::new(),
            body: json!({"success":false,"errors":[{"code":"NotFound"}]}).to_string(),
        },
        search_reply(&[
            "docs/a.md",
            "docs/x.md",
            "src/c.rs",
            "docs/y.md",
            "docs/a.md",
        ]),
    ])
    .await;
    let client = Client::for_test_server(&stub.url)
        .with_codebase("A".into())
        .with_test_local_root(directory.path().to_path_buf());
    let output = affinity(
        &client,
        &strings(&["src/c.rs", "docs/b.md", "docs/a.md", "docs/b.md"]),
        QueryPresentation::Inspector,
    )
    .await
    .unwrap();
    assert_eq!(
        output.text,
        "Semantic affinity: 3 files, 3 related pairs. Input files returned: 2 of 3 completed searches."
    );
    let display = output.display.unwrap();
    assert_eq!(display["version"], 1);
    assert_eq!(display["kind"], "affinity");
    assert_eq!(display["identity"]["codebaseId"], "A");
    assert_eq!(
        display["files"],
        json!([
            {"path":"src/c.rs","query":"name","indexed":true,"results":4,"search":"self_match"},
            {"path":"docs/b.md","query":"content","indexed":false,"results":4,"search":"other_matches"},
            {"path":"docs/a.md","query":"content","indexed":true,"results":4,"search":"self_match"}
        ])
    );
    assert_eq!(
        display["pairs"],
        json!([
            {"a":"docs/a.md","b":"docs/b.md","direct":0.5,"shared":1.0},
            {"a":"docs/a.md","b":"src/c.rs","direct":0.7887,"shared":1.0},
            {"a":"docs/b.md","b":"src/c.rs","direct":0.2887,"shared":1.0}
        ])
    );
    assert_eq!(display["caveats"].as_array().unwrap().len(), 1);
    let heads = stub.request_lines();
    assert_eq!(heads.len(), 4);
    assert!(
        heads[0]
            .starts_with("GET /v1/codebases/A/graph/outline?path=src%2Fc.rs&includeBody=false ")
    );
    assert!(
        heads[1..]
            .iter()
            .all(|head| head.starts_with("POST /v1/search "))
    );
}

#[tokio::test]
async fn loading_searches_leave_files_without_profiles_and_add_caveats() {
    let directory = checkout();
    let stub = Stub::serve(vec![Reply::loading("5")]).await;
    let client = Client::for_test_server(&stub.url)
        .with_codebase("A".into())
        .with_test_local_root(directory.path().to_path_buf());
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        affinity(
            &client,
            &strings(&["docs/a.md", "docs/b.md"]),
            QueryPresentation::Inspector,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        output.text,
        "Semantic affinity: 2 files, 0 related pairs. 2 searches were unavailable while the backend loads."
    );
    let display = output.display.unwrap();
    assert_eq!(display["pairs"], json!([]));
    assert_eq!(display["files"][0]["results"], 0);
    assert_eq!(display["files"][0]["indexed"], false);
    assert_eq!(display["files"][0]["search"], "loading");
    assert_eq!(display["caveats"].as_array().unwrap().len(), 3);
    assert_eq!(stub.requests(), 2);
}

#[tokio::test]
async fn a_completed_empty_search_is_distinct_from_a_loading_search() {
    let directory = checkout();
    let stub = Stub::serve(vec![search_reply(&[]), Reply::loading("5")]).await;
    let client = Client::for_test_server(&stub.url)
        .with_codebase("A".into())
        .with_test_local_root(directory.path().to_path_buf());
    let output = affinity(
        &client,
        &strings(&["docs/a.md", "docs/b.md"]),
        QueryPresentation::Inspector,
    )
    .await
    .unwrap();
    assert_eq!(
        output.text,
        "Semantic affinity: 2 files, 0 related pairs. Input files returned: 0 of 1 completed search. 1 search returned no result files. 1 search was unavailable while the backend loads."
    );
    let display = output.display.unwrap();
    let mut searches: Vec<_> = display["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            assert_eq!(file["results"], 0);
            assert_eq!(file["indexed"], false);
            file["search"].as_str().unwrap()
        })
        .collect();
    searches.sort_unstable();
    assert_eq!(searches, ["empty", "loading"]);
    assert_eq!(display["pairs"], json!([]));
    assert_eq!(stub.requests(), 2);
}

#[tokio::test]
async fn other_search_failures_fail_the_command() {
    let directory = checkout();
    let stub = Stub::serve(vec![Reply::gateway("502 Bad Gateway")]).await;
    let client = Client::for_test_server(&stub.url)
        .with_codebase("A".into())
        .with_test_local_root(directory.path().to_path_buf());
    let error = affinity(
        &client,
        &strings(&["docs/a.md", "docs/b.md"]),
        QueryPresentation::Plain,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().starts_with("affinity failed:"));
}
