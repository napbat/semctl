use super::*;

fn hit(codebase: Option<&str>, snippet: &str) -> api::SearchHit {
    serde_json::from_value(serde_json::json!({
        "domainId": "code", "id": "hit", "score": 2.0,
        "path": "src/lib.rs", "codebaseId": codebase,
        "lineStart": 8, "lineEnd": 9, "byteStart": 40, "byteEnd": 60,
        "snippet": snippet, "focusLine": 9, "focusByte": 44,
        "snippetLineStart": 8, "snippetLineEnd": 9,
        "snippetByteStart": 40, "snippetByteEnd": 60
    }))
    .unwrap()
}

fn content() -> api::FileContent {
    serde_json::from_value(serde_json::json!({
        "path": "src/lib.rs", "contentHash": "pinned", "content": "",
        "encoding": "utf-8", "isBinary": false, "size": 60,
        "byteStart": 40, "byteEnd": 60, "lineStart": 8,
        "lineEnd": 9, "truncated": true
    }))
    .unwrap()
}

fn symbol_hit() -> api::SymbolSearchHit {
    serde_json::from_value(serde_json::json!({
        "codebaseId": "A", "path": "src/lib.rs",
        "lineStart": 8, "lineEnd": 9, "byteStart": 40, "byteEnd": 60,
        "name": "run", "qualifiedName": "crate::run",
        "kind": "function", "project": "crates/app/Cargo.toml",
        "language": "rust", "score": 4_294_967_295_u32
    }))
    .unwrap()
}

#[test]
fn graph_lookup_preserves_current_and_legacy_provenance_without_relevance_scores() {
    let client = Client::for_test("A", None);
    let query = "  crate::run \"exact\"\n雪  ";
    let snippet = "\r\n    target = target + 1;\t\n";
    for access in [Some(true), Some(false), None] {
        let mut graph = hit(Some("A"), snippet);
        graph.symbol = Some("target".into());
        graph.qualified_symbol = Some("crate::target".into());
        graph.enclosing_symbol = Some("crate::run".into());
        graph.is_write = access;
        graph.reference_namespace = Some("value".into());
        graph.reference_kind = Some("assignment".into());
        graph.external_target = Some("rust:external::target".into());
        let hits = [graph];
        let lookup = lookup(&client, query, &hits).unwrap();
        let batch = batch(
            &client,
            &[api::SymbolHits {
                symbol: query.into(),
                hits: hits.into(),
            }],
        )
        .unwrap();
        assert_eq!(lookup, batch);
        let wire = serde_json::to_vec(&lookup).unwrap();
        let display: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(display["version"], 1);
        assert_eq!(display["kind"], "batch");
        assert_eq!(display["symbols"][0]["symbol"], query);
        let row = &display["symbols"][0]["hits"][0];
        assert_eq!(row["qualifiedName"], "crate::target");
        assert_eq!(row["enclosingSymbol"], "crate::run");
        assert_eq!(row["isWrite"], serde_json::json!(access));
        assert_eq!(row["referenceNamespace"], "value");
        assert_eq!(row["referenceKind"], "assignment");
        assert_eq!(row["externalTarget"], "rust:external::target");
        assert_eq!(row["snippet"], snippet);
        assert_eq!(row["startLine"], 8);
        assert_eq!(row["endByte"], 60);
        assert_eq!(row["focusByte"], 44);
        assert_eq!(row["snippetStartByte"], 40);
        assert!(row["score"].is_null());
        assert_eq!(row["binding"]["state"], "not-probed");
    }
    let legacy: api::SearchHit = serde_json::from_value(serde_json::json!({
        "domainId": "code", "id": "legacy", "score": 0.0, "snippet": ""
    }))
    .unwrap();
    let display = lookup(&client, query, &[legacy]).unwrap();
    let row = &display["symbols"][0]["hits"][0];
    for field in [
        "qualifiedName",
        "enclosingSymbol",
        "isWrite",
        "referenceNamespace",
        "referenceKind",
        "externalTarget",
        "project",
        "score",
        "startLine",
    ] {
        assert!(row[field].is_null(), "{field}");
    }
}

#[test]
fn symbol_search_retains_real_score_project_and_coordinates_without_source_claims() {
    let client = Client::for_test("A", None);
    let query = "run";
    let display = symbol_search(&client, query, &[symbol_hit()]).unwrap();
    assert_eq!(display["kind"], "batch");
    assert_eq!(display["symbols"][0]["symbol"], query);
    let row = &display["symbols"][0]["hits"][0];
    assert_eq!(row["symbol"], "run");
    assert_eq!(row["qualifiedName"], "crate::run");
    assert_eq!(row["project"], "crates/app/Cargo.toml");
    assert_eq!(row["score"], f64::from(u32::MAX));
    assert_eq!(row["startLine"], 8);
    assert_eq!(row["endLine"], 9);
    assert_eq!(row["startByte"], 40);
    assert_eq!(row["endByte"], 60);
    assert!(row["snippet"].is_null());
    assert_eq!(row["binding"]["state"], "not-probed");
    let mut zero = symbol_hit();
    zero.score = 0;
    zero.project = None;
    let display = symbol_search(&client, query, &[zero]).unwrap();
    assert_eq!(display["symbols"][0]["hits"][0]["score"], 0.0);
    assert!(display["symbols"][0]["hits"][0]["project"].is_null());
    let hits = [hit(Some("A"), "")];
    let scope = HitContext::search(None, Some("A"), true);
    assert_eq!(
        search(&client, query, &hits, &[], scope).unwrap()["hits"][0]["score"],
        2.0
    );
    assert!(lookup(&client, query, &hits).unwrap()["symbols"][0]["hits"][0]["score"].is_null());
}

#[test]
fn graph_projection_never_upgrades_unsafe_or_cross_copy_locations_to_bound_source() {
    let root = tempfile::tempdir().unwrap();
    let client = Client::for_test("A", Some(root.path().join("not-probed")));
    for codebase in ["A", "B"] {
        for path in [
            "../outside.rs",
            "/absolute.rs",
            r"C:\outside.rs",
            "src/lib.rs",
        ] {
            let mut graph = hit(Some(codebase), "");
            graph.path = Some(path.into());
            let display = lookup(&client, "run", &[graph]).unwrap();
            if codebase == "B" {
                assert!(display["identity"]["localRoot"].is_null());
            }
            let row = &display["symbols"][0]["hits"][0];
            assert_eq!(row["path"], path);
            assert_eq!(row["codebaseId"], codebase);
            assert_eq!(row["binding"]["state"], "not-probed");
            let mut symbol = symbol_hit();
            symbol.codebase_id = codebase.into();
            symbol.path = path.into();
            let display = symbol_search(&client, "run", &[symbol]).unwrap();
            if codebase == "B" {
                assert!(display["identity"]["localRoot"].is_null());
            }
            assert_eq!(display["symbols"][0]["hits"][0]["path"], path);
            assert_eq!(
                display["symbols"][0]["hits"][0]["binding"]["state"],
                "not-probed"
            );
        }
    }
    let display = batch(
        &client,
        &[api::SymbolHits {
            symbol: "run".into(),
            hits: vec![hit(Some("A"), ""), hit(Some("B"), "")],
        }],
    )
    .unwrap();
    assert!(display["identity"]["localRoot"].is_null());
    let canonical = lookup(&client.for_canonical(), "run", &[hit(Some("A"), "")]).unwrap();
    assert_eq!(canonical["identity"]["copy"], "canonical");
    assert!(canonical["identity"]["localRoot"].is_null());
}

#[test]
fn lookup_and_symbol_search_omit_empty_or_oversized_metadata_without_changing_payload() {
    let client = Client::for_test("A", None);
    assert!(lookup(&client, "missing", &[]).is_none());
    assert!(symbol_search(&client, "missing", &[]).is_none());
    let hits: Vec<_> = (0..MAX_DISPLAY_HITS).map(|_| hit(Some("A"), "")).collect();
    assert!(lookup(&client, "run", &hits).is_some());
    let too_many: Vec<_> = (0..=MAX_DISPLAY_HITS).map(|_| hit(Some("A"), "")).collect();
    assert!(lookup(&client, "run", &too_many).is_none());
    let symbols: Vec<_> = (0..MAX_DISPLAY_HITS).map(|_| symbol_hit()).collect();
    assert!(symbol_search(&client, "run", &symbols).is_some());
    let too_many: Vec<_> = (0..=MAX_DISPLAY_HITS).map(|_| symbol_hit()).collect();
    assert!(symbol_search(&client, "run", &too_many).is_none());
    let graph = [hit(Some("A"), "")];
    let empty = lookup(&client, "", &graph).unwrap();
    let overhead = serde_json::to_vec(&empty).unwrap().len();
    let exact = "x".repeat(MAX_DISPLAY_BYTES - overhead);
    let display = lookup(&client, &exact, &graph).unwrap();
    assert_eq!(
        serde_json::to_vec(&display).unwrap().len(),
        MAX_DISPLAY_BYTES
    );
    assert!(lookup(&client, &(exact + "x"), &graph).is_none());
    let mut huge = hit(Some("A"), "complete source");
    huge.enclosing_symbol = Some("x".repeat(MAX_DISPLAY_BYTES + 1));
    let output = QueryOutput::new(huge.snippet.clone(), lookup(&client, "run", &[huge]));
    assert!(output.display.is_none());
    assert_eq!(output.text, "complete source");
    let mut huge = symbol_hit();
    huge.project = Some("x".repeat(MAX_DISPLAY_BYTES + 1));
    assert!(symbol_search(&client, "run", &[huge]).is_none());
    assert!(symbol_search(&client, &"\n".repeat(MAX_DISPLAY_BYTES / 2), &symbols[..1]).is_none());
}

#[tokio::test]
async fn search_query_round_trips_exactly_without_extra_reads_or_plain_metadata() {
    use crate::client::stub::{Reply, Stub};
    use crate::query::{QueryPresentation, SearchOpts};

    let query = "  exact \"query\" \\\\ <tag>&[]\r\n\t\x1b[31m cafÃ© é›ª  ";
    for response in [
        "[]",
        r#"[{"domainId":"code","id":"hit","score":1.0,"path":"src/lib.rs","snippet":"fn sample() {}"}]"#,
    ] {
        for presentation in [QueryPresentation::Plain, QueryPresentation::Inspector] {
            let stub = Stub::serve(vec![Reply::ok(response)]).await;
            let client = Client::for_test_server(&stub.url).with_codebase("A".into());
            let output =
                crate::query::search(&client, query, 8, &[], &SearchOpts::default(), presentation)
                    .await
                    .unwrap();
            match presentation {
                QueryPresentation::Inspector => {
                    let wire = serde_json::to_vec(&output.display.unwrap()).unwrap();
                    let display: Value = serde_json::from_slice(&wire).unwrap();
                    assert_eq!(display["query"].as_str(), Some(query));
                    assert_eq!(display["kind"], "search");
                }
                QueryPresentation::Plain => assert!(output.display.is_none()),
            }
            assert_eq!(stub.requests(), 1);
        }
    }
}

#[test]
fn search_query_counts_toward_the_whole_serialized_display_cap() {
    let client = Client::for_test("A", None);
    let scope = HitContext::search(None, Some("A"), true);
    let hits = [hit(Some("A"), "source")];
    let empty = search(&client, "", &hits, &[], scope).unwrap();
    let overhead = serde_json::to_vec(&empty).unwrap().len();
    let exact = "x".repeat(MAX_DISPLAY_BYTES - overhead);
    let display = search(&client, &exact, &hits, &[], scope).unwrap();
    assert_eq!(display["query"].as_str(), Some(exact.as_str()));
    assert_eq!(
        serde_json::to_vec(&display).unwrap().len(),
        MAX_DISPLAY_BYTES
    );
    assert!(search(&client, &(exact + "x"), &hits, &[], scope).is_none());
    assert!(search(&client, &"x".repeat(MAX_DISPLAY_BYTES + 1), &[], &[], scope).is_none());
    assert!(
        search(
            &client,
            &"\n".repeat(MAX_DISPLAY_BYTES / 2),
            &hits,
            &[],
            scope
        )
        .is_none()
    );
}

#[test]
fn late_index_warning_obeys_the_encoded_cap_without_losing_native_text() {
    let client = Client::for_test("A", None);
    let scope = HitContext::search(None, Some("A"), true);
    let warning = "stale \"snapshot\"\né›ª";
    let mut empty = QueryOutput::new(String::new(), search(&client, "", &[], &[], scope));
    empty.set_index_warning(Some(warning.into()));
    let overhead = serde_json::to_vec(empty.display.as_ref().unwrap())
        .unwrap()
        .len();
    for extra in [0, 1] {
        let query = "x".repeat(MAX_DISPLAY_BYTES - overhead + extra);
        let mut output = QueryOutput::new(
            "complete native result".into(),
            search(&client, &query, &[], &[], scope),
        );
        assert!(output.display.is_some());
        output.set_index_warning(Some(warning.into()));
        if extra == 0 {
            let display = output.display.as_ref().unwrap();
            assert_eq!(display["indexWarning"], warning);
            assert_eq!(
                serde_json::to_vec(display).unwrap().len(),
                MAX_DISPLAY_BYTES
            );
        } else {
            assert!(output.display.is_none());
        }
        assert_eq!(output.text, "complete native result");
    }
}

#[test]
fn byte_and_hit_caps_are_independent_and_all_or_nothing() {
    let client = Client::for_test("A", None);
    let scope = HitContext::search(None, Some("A"), true);
    let hits: Vec<_> = (0..MAX_DISPLAY_HITS).map(|_| hit(Some("A"), "")).collect();
    assert_eq!(
        search(&client, "query", &hits, &[], scope).unwrap()["hits"]
            .as_array()
            .unwrap()
            .len(),
        32
    );
    let too_many: Vec<_> = (0..=MAX_DISPLAY_HITS).map(|_| hit(Some("A"), "")).collect();
    assert!(search(&client, "query", &too_many, &[], scope).is_none());
    assert!(
        search(
            &client,
            "query",
            &[hit(Some("A"), &"x".repeat(MAX_DISPLAY_BYTES))],
            &[],
            scope
        )
        .is_none()
    );

    let content = content();
    let empty = source(&client, &content, "", Some("rs"), "content-plane").unwrap();
    let overhead = serde_json::to_vec(&empty).unwrap().len();
    let exact = "x".repeat(MAX_DISPLAY_BYTES - overhead);
    let display = source(&client, &content, &exact, Some("rs"), "content-plane").unwrap();
    assert_eq!(
        serde_json::to_vec(&display).unwrap().len(),
        MAX_DISPLAY_BYTES
    );
    assert!(
        source(
            &client,
            &content,
            &(exact + "x"),
            Some("rs"),
            "content-plane"
        )
        .is_none()
    );
    // Escaped and multibyte text count as serialized bytes, not characters.
    assert!(
        source(
            &client,
            &content,
            &"\n".repeat(MAX_DISPLAY_BYTES / 2),
            None,
            "content-plane"
        )
        .is_none()
    );
}

#[test]
fn search_preserves_coordinates_and_binding_without_cross_copy_roots() {
    let root = tempfile::tempdir().unwrap();
    let client = Client::for_test("A", Some(root.path().to_path_buf()));
    let scope = HitContext::search(client.local_root(), Some("A"), true);
    let hits = [hit(Some("A"), "first\nsecond")];
    let bound = search(&client, "query", &hits, &[HitSource::Bound], scope).unwrap();
    assert!(bound["identity"]["localRoot"].is_string());
    assert_eq!(bound["hits"][0]["binding"]["state"], "bound");
    assert_eq!(bound["hits"][0]["startLine"], 8);
    assert_eq!(bound["hits"][0]["focusLine"], 9);
    assert_eq!(bound["hits"][0]["snippetStartByte"], 40);
    let states = [
        (HitSource::ChangedLocal, "changed-local"),
        (HitSource::IndexedRangeMismatch, "range-mismatch"),
        (
            HitSource::Unverified("probe budget exhausted".into()),
            "unverified",
        ),
    ];
    for (state, expected) in states {
        let display = search(&client, "query", &hits, &[state], scope).unwrap();
        assert_eq!(display["hits"][0]["binding"]["state"], expected);
        assert!(display["hits"][0]["binding"]["reason"].is_string());
    }
    let mixed = [hit(Some("A"), ""), hit(Some("B"), "")];
    assert!(
        search(&client, "query", &mixed, &[], scope).unwrap()["identity"]["localRoot"].is_null()
    );
    let legacy = [hit(None, "")];
    let broad = HitContext::search(client.local_root(), Some("A"), false);
    assert!(
        search(&client, "query", &legacy, &[], broad).unwrap()["identity"]["localRoot"].is_null()
    );
    let canonical = client.for_canonical();
    let display = search(
        &canonical,
        "query",
        &hits,
        &[],
        HitContext::search(None, Some("A"), true),
    )
    .unwrap();
    assert_eq!(display["identity"]["copy"], "canonical");
    assert!(display["identity"]["localRoot"].is_null());
    let content = content();
    assert!(source(&client, &content, "canonical bytes", None, "content-plane").unwrap()["identity"]["localRoot"].is_null());
    let local = source(
        &client,
        &content,
        "verified bytes\r\n",
        Some("rs"),
        "verified-local",
    )
    .unwrap();
    assert_eq!(local["text"], "verified bytes\r\n");
    assert_eq!(local["contentHash"], "pinned");
    assert_eq!(local["byteStart"], 40);
    assert!(local["identity"]["localRoot"].is_string());
    let mut incomplete = hit(Some("A"), "");
    incomplete.line_start = None;
    incomplete.snippet_line_start = None;
    incomplete.score = f64::NAN;
    let display = search(&client, "query", &[incomplete], &[], scope).unwrap();
    assert!(display["hits"][0]["startLine"].is_null());
    assert!(display["hits"][0]["snippetStartLine"].is_null());
    assert!(display["hits"][0]["score"].is_null());
}

#[test]
fn batch_retains_misses_order_and_aggregate_bound_without_probing() {
    let client = Client::for_test("A", None);
    let mut symbols = vec![
        api::SymbolHits {
            symbol: "missing".into(),
            hits: vec![],
        },
        api::SymbolHits {
            symbol: "found".into(),
            hits: (0..32).map(|_| hit(Some("A"), "")).collect(),
        },
    ];
    let display = batch(&client, &symbols).unwrap();
    assert_eq!(display["symbols"][0]["symbol"], "missing");
    assert_eq!(display["symbols"][0]["hits"], serde_json::json!([]));
    assert_eq!(
        display["symbols"][1]["hits"][0]["binding"]["state"],
        "not-probed"
    );
    symbols.push(api::SymbolHits {
        symbol: "overflow".into(),
        hits: vec![hit(Some("A"), "")],
    });
    assert!(batch(&client, &symbols).is_none());
}

#[test]
fn status_preserves_unknown_counters_failures_and_snapshot_copy_identity() {
    let root = tempfile::tempdir().unwrap();
    let client = Client::for_test("A", Some(root.path().to_path_buf()));
    let coordinator: CoordinatorStatus = serde_json::from_value(serde_json::json!({
        "root": root.path(), "codebase_id": "A", "leases": 1, "watcher": "active",
        "last_job_id": "job", "running": false, "pending_triggers": 0,
        "trigger_overflow": false, "last_outcome": null, "last_error": "upload failed",
        "first_index": null, "sync_progress": null
    }))
    .unwrap();
    let job = Ok(serde_json::from_value::<api::JobStatus>(serde_json::json!({
        "filesEmbedded": 7, "filesFailed": 2, "error": "embedding failed"
    }))
    .unwrap());
    let totals = Ok((100, 4096));
    let display = status(&client, Some(&coordinator), &totals, Some(("job", &job))).unwrap();
    assert_eq!(display["coordinator"]["last_error"], "upload failed");
    assert_eq!(display["catalog"]["files"], 100);
    assert_eq!(display["job"]["filesEmbedded"], 7);
    assert_eq!(display["job"]["filesFailed"], 2);
    assert!(display["job"].get("filesToEmbed").is_none());
    assert!(display["job"].get("filesDeleted").is_none());
    assert!(display["job"].get("chunkCount").is_none());
    let canonical = status(&client.for_canonical(), Some(&coordinator), &totals, None).unwrap();
    assert!(canonical["coordinator"].is_null());
    assert!(canonical["identity"]["localRoot"].is_null());
    let failed_totals = Err(anyhow::anyhow!("catalog unavailable"));
    let failed_job = Err(anyhow::anyhow!("job unavailable"));
    let display = status(&client, None, &failed_totals, Some(("job", &failed_job))).unwrap();
    assert!(display["catalog"].is_null());
    assert!(display["job"].is_null());
    assert_eq!(display["catalogError"], "catalog unavailable");
    assert_eq!(display["jobError"], "job unavailable");
}
