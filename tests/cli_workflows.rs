use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::Command;

const PREIMAGE: &str = "fn old_name() {}\n";
const POSTIMAGE: &str = "fn new_name() {}\n";
const PLAN_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[derive(Clone, Debug)]
struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

struct Server {
    address: SocketAddr,
    stopped: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Request>>>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn start(response: impl Fn(&Request) -> Value + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind isolated HTTP server");
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let worker = thread::spawn(move || {
            for connection in listener.incoming() {
                let mut stream = connection.expect("accept CLI request");
                if stop.load(Ordering::Acquire) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_request(&stream);
                let body = serde_json::to_vec(&response(&request)).unwrap();
                recorded.lock().unwrap().push(request);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            address,
            stopped,
            requests,
            worker: Some(worker),
        }
    }

    fn request_count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.path == path)
            .count()
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.expect("HTTP fixture worker completed");
            }
        }
    }
}

fn read_request(stream: &TcpStream) -> Request {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap().to_string();
    let path = parts.next().unwrap().to_string();
    let mut headers = HashMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.split_once(':').expect("HTTP header field");
        headers.insert(key.to_ascii_lowercase(), value.trim().to_string());
    }
    let length = headers
        .get("content-length")
        .map_or(0, |value| value.parse().unwrap());
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    Request {
        method,
        path,
        headers,
        body: if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        },
    }
}

struct Checkout {
    _directory: tempfile::TempDir,
    config: PathBuf,
    root: PathBuf,
}

impl Checkout {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("checkout-a");
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::write(root.join("src/lib.rs"), PREIMAGE).unwrap();
        let config = directory.path().join("config");
        fs::create_dir_all(config.join("semctl")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let cache = HashMap::from([(root.to_string_lossy().into_owned(), "A")]);
        let config_value = json!({"codebase_cache": cache});
        fs::write(
            config.join("semctl/config.toml"),
            toml::to_string(&config_value).unwrap(),
        )
        .unwrap();
        fs::write(config.join("semctl/installation-id"), "a".repeat(64)).unwrap();
        Self {
            _directory: directory,
            config,
            root,
        }
    }

    fn command(&self, server: &Server, cwd: &Path, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_semctl"));
        command
            .args(["--server", &format!("http://{}", server.address)])
            .args(["--tenant", "test-tenant"])
            .args(arguments)
            .current_dir(cwd)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("SEMCTX_TOKEN", "offline-test-token")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for variable in [
            "SEMCTX_CODEBASE",
            "SEMCTX_SERVER",
            "SEMCTX_TENANT",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_CEILING_DIRECTORIES",
        ] {
            command.env_remove(variable);
        }
        command
    }

    async fn run(&self, server: &Server, cwd: &Path, arguments: &[&str]) -> Output {
        let mut command = self.command(server, cwd, arguments);
        let child = command.spawn().expect("start CLI process");
        tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
            .await
            .expect("CLI workflow timed out")
            .expect("collect CLI output")
    }
}

fn successful_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI emits JSON")
}

fn summary(codebase: &str) -> Value {
    json!({"id": codebase, "slug": codebase, "graphGeneration": 7, "graphFresh": true})
}

fn plan(codebase: &str, source: &str) -> Value {
    json!({
        "schemaVersion": 1,
        "planId": PLAN_ID,
        "operation": "rename",
        "codebaseId": codebase,
        "graphGeneration": 7,
        "sourceIdentity": source,
        "graphComplete": true,
        "providerGenerationsCurrent": true,
        "dependentCodebases": [],
        "applicable": true,
        "confidence": "high",
        "files": [{
            "path": "src/lib.rs",
            "preimageHash": blake3::hash(PREIMAGE.as_bytes()).to_hex().to_string(),
            "edits": [{"start": 3, "end": 11, "replacement": "new_name"}],
            "expectedPostimageHash": blake3::hash(POSTIMAGE.as_bytes()).to_hex().to_string()
        }],
        "warnings": [],
        "refusalReasons": [],
        "unresolvedSites": [],
        "uncertainSites": [],
        "formatter": null,
        "renderedDiff": ""
    })
}

fn edit_response(request: &Request) -> Value {
    let parts: Vec<_> = request.path.split('/').collect();
    let codebase = parts.get(3).expect("codebase request path");
    let data = match (request.method.as_str(), parts.as_slice()) {
        ("POST", ["", "v1", "codebases", _, "edits", "rename"]) => plan(
            codebase,
            request
                .headers
                .get("x-semctx-source-id")
                .map_or("remote-source", String::as_str),
        ),
        ("GET", ["", "v1", "codebases", _]) => summary(codebase),
        _ => panic!("unexpected edit request: {request:?}"),
    };
    json!({"success": true, "data": data})
}

async fn assert_edit_roundtrip(nested: bool, explicit: bool) {
    let checkout = Checkout::new();
    let server = Server::start(edit_response);
    let cwd = if nested {
        let output = std::process::Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "--quiet"])
            .arg(&checkout.root)
            .output()
            .expect("initialize isolated Git repository");
        assert!(output.status.success());
        checkout.root.join("src/nested")
    } else {
        checkout.root.clone()
    };
    let prefix = if explicit {
        vec!["--codebase", "A"]
    } else {
        Vec::new()
    };
    let mut arguments = prefix.clone();
    arguments.extend([
        "edit",
        "rename",
        "--path",
        "src/lib.rs",
        "--line",
        "1",
        "--column",
        "3",
        "new_name",
    ]);
    let planned = successful_json(&checkout.run(&server, &cwd, &arguments).await);
    assert_eq!(planned["codebaseId"], "A");
    assert_ne!(planned["sourceIdentity"], "remote-source");
    assert_eq!(
        fs::read_to_string(checkout.root.join("src/lib.rs")).unwrap(),
        PREIMAGE
    );
    let plan_path = checkout.root.join("plan.json");
    fs::write(&plan_path, serde_json::to_vec(&planned).unwrap()).unwrap();
    let mut arguments = prefix.clone();
    arguments.extend(["edit", "apply", plan_path.to_str().unwrap()]);
    for duplicate in [false, true] {
        let applied = successful_json(&checkout.run(&server, &cwd, &arguments).await);
        assert_eq!(applied["alreadyApplied"], duplicate);
        assert_eq!(applied["alreadyUndone"], false);
        assert_eq!(applied["changedFiles"][0]["path"], "src/lib.rs");
        assert_eq!(
            fs::read_to_string(checkout.root.join("src/lib.rs")).unwrap(),
            POSTIMAGE
        );
    }
    let mut arguments = prefix;
    arguments.extend(["edit", "undo", PLAN_ID]);
    for duplicate in [false, true] {
        let undone = successful_json(&checkout.run(&server, &cwd, &arguments).await);
        assert_eq!(undone["alreadyUndone"], duplicate);
        assert_eq!(undone["alreadyApplied"], false);
        assert_eq!(
            fs::read_to_string(checkout.root.join("src/lib.rs")).unwrap(),
            PREIMAGE
        );
    }
    let requests = server.requests();
    let rename = requests
        .iter()
        .find(|request| request.method == "POST")
        .unwrap();
    assert_eq!(rename.body["target"]["path"], "src/lib.rs");
    assert_eq!(rename.body["newName"], "new_name");
    assert_eq!(rename.headers["authorization"], "Bearer offline-test-token");
    assert!(
        requests
            .iter()
            .filter_map(|request| request.headers.get("x-semctx-source-id"))
            .all(|source| source == planned["sourceIdentity"].as_str().unwrap())
    );
}

#[tokio::test]
async fn explicitly_selected_checkout_supports_apply_and_undo_replay() {
    assert_edit_roundtrip(false, true).await;
}

#[tokio::test]
async fn cached_working_directory_supports_apply_and_undo_replay() {
    assert_edit_roundtrip(false, false).await;
}

#[tokio::test]
async fn nested_git_directory_edits_the_recorded_checkout_root() {
    assert_edit_roundtrip(true, false).await;
}

#[tokio::test]
async fn unrelated_pinned_codebase_cannot_apply_to_the_working_directory() {
    let checkout = Checkout::new();
    let server = Server::start(edit_response);
    let planned = successful_json(
        &checkout
            .run(
                &server,
                &checkout.root,
                &[
                    "--codebase",
                    "B",
                    "edit",
                    "rename",
                    "--symbol",
                    "old_name",
                    "new_name",
                ],
            )
            .await,
    );
    assert_eq!(planned["sourceIdentity"], "remote-source");
    let plan_path = checkout.root.join("plan.json");
    fs::write(&plan_path, serde_json::to_vec(&planned).unwrap()).unwrap();
    let output = checkout
        .run(
            &server,
            &checkout.root,
            &[
                "--codebase",
                "B",
                "edit",
                "apply",
                plan_path.to_str().unwrap(),
            ],
        )
        .await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no bound local checkout root"));
    assert_eq!(
        fs::read_to_string(checkout.root.join("src/lib.rs")).unwrap(),
        PREIMAGE
    );
    assert_eq!(server.requests().len(), 1);
}

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
        assert!(text.contains("[codebase B] src/lib.rs:1"), "{text}");
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
    let local = text
        .lines()
        .find(|line| line.contains("[codebase A]"))
        .unwrap();
    let remote = text
        .lines()
        .find(|line| line.contains("[codebase B]"))
        .unwrap();
    let unidentified = text
        .lines()
        .find(|line| line.starts_with("[code rust] src/lib.rs"))
        .unwrap();
    assert!(local.contains("checkout-a"), "{text}");
    assert!(local.contains("stale"), "{text}");
    assert!(remote.contains("[codebase B] src/lib.rs:1"), "{text}");
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("scope must be"));
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
    assert!(text.contains("[codebase A] src/lib.rs:1"), "{text}");
    assert!(!text.contains("checkout-a"), "{text}");
    assert!(!text.contains("stale"), "{text}");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("x-semctx-source-id"));
}

#[tokio::test]
async fn mcp_scoped_search_works_from_an_unindexed_directory() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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
            assert!(response.to_string().contains("mutually exclusive"));
        } else {
            let text = response["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("[codebase B] src/lib.rs:1"), "{text}");
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

fn readiness_server(
    failed: bool,
    job_complete: Arc<AtomicBool>,
    job_polled: Arc<tokio::sync::Notify>,
) -> Server {
    Server::start(move |request| {
        let completed = job_complete.load(Ordering::Acquire);
        let data = match request.path.as_str() {
            "/v1/whoami" => json!({"capabilities": []}),
            path if path.starts_with("/v1/codebases?") => json!([]),
            "/v1/codebases" | "/v1/codebases/A" => json!({
                "id": "A", "slug": "a", "sourceKind": "Local"
            }),
            "/v1/codebases/A/sync" => json!({
                "jobId": "first-index", "needContent": [], "toDelete": []
            }),
            "/v1/jobs/first-index" => {
                job_polled.notify_one();
                json!({
                    "filesToEmbed": 1, "filesToDelete": 0,
                    "filesEmbedded": u32::from(completed && !failed),
                    "filesDeleted": 0, "filesFailed": u32::from(completed && failed),
                    "completedAt": completed.then_some("2026-09-10T00:00:00Z"),
                    "error": (completed && failed).then_some("embedding failed")
                })
            }
            "/v1/search" => {
                if request.body["codebaseIds"] != json!(["B"]) {
                    assert!(
                        completed && !failed,
                        "search exposed an incomplete first index"
                    );
                }
                json!([])
            }
            path => panic!("unexpected readiness request: {path}"),
        };
        json!({"success": true, "data": data, "page": 0, "pageSize": 500, "total": 0})
    })
}

#[tokio::test]
async fn scoped_mcp_search_waits_for_initial_embedding_and_propagates_failure() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    for failed in [false, true] {
        let checkout = Checkout::new();
        fs::write(checkout.config.join("semctl/config.toml"), "").unwrap();
        let polled = Arc::new(tokio::sync::Notify::new());
        let complete = Arc::new(AtomicBool::new(false));
        let server = readiness_server(failed, complete.clone(), polled.clone());
        let mut child = checkout
            .command(&server, &checkout.root, &["mcp"])
            .env("SEMCTX_MCP_UPDATE_CHECK", "0")
            .env("SEMCTX_MCP_RESYNC_SECS", "0")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let initialize = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": rmcp::model::ProtocolVersion::default(),
                "capabilities": {}, "clientInfo": {"name": "readiness-test", "version": "1"}
            }
        });
        stdin
            .write_all(format!("{initialize}\n").as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        let index = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "index_codebase", "arguments": {}
        }});
        stdin
            .write_all(format!("{index}\n").as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), polled.notified())
            .await
            .expect("initial job must start");
        for (id, arguments) in [
            (3, json!({"query": "main", "codebase_ids": ["A"]})),
            (4, json!({"query": "main", "scope": "personal"})),
            (5, json!({"query": "main", "codebase_ids": ["B"]})),
        ] {
            let search = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
                "name": "search_codebase", "arguments": arguments
            }});
            stdin
                .write_all(format!("{search}\n").as_bytes())
                .await
                .unwrap();
        }
        let unrelated = tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
            .await
            .expect("unrelated search must remain available")
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&unrelated).unwrap()["id"],
            5,
            "{unrelated}"
        );
        assert_eq!(server.request_count("/v1/search"), 1);
        complete.store(true, Ordering::Release);
        let mut ids = Vec::new();
        for _ in 0..3 {
            let line = tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
                .await
                .expect("initial index must release waiting tools")
                .unwrap()
                .unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            ids.push(response["id"].as_u64().unwrap());
            assert_eq!(line.contains("embedding failed"), failed, "{line}");
        }
        ids.sort_unstable();
        assert_eq!(ids, [2, 3, 4]);
        drop(stdin);
        let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .expect("MCP shutdown timed out")
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(
            server.request_count("/v1/search"),
            if failed { 1 } else { 3 }
        );
    }
}

#[tokio::test]
async fn older_versioned_servers_keep_unrelated_remote_basenames_separate() {
    for project_keys in [false, true] {
        let checkout = Checkout::new();
        fs::write(checkout.config.join("semctl/config.toml"), "").unwrap();
        for arguments in [
            vec!["init", "--quiet"],
            vec![
                "remote",
                "add",
                "origin",
                "https://example.test/our-org/repo.git",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(arguments)
                    .current_dir(&checkout.root)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let server = Server::start(move |request| {
            let data = match (request.method.as_str(), request.path.as_str()) {
                (_, "/v1/whoami") => json!({"capabilities": if project_keys {
                    vec!["codebase-versions", "project-keys"]
                } else { vec!["codebase-versions"] }}),
                (_, path) if path.starts_with("/v1/codebases?sourceId=") => json!([]),
                (_, path) if path.starts_with("/v1/codebases?") => json!([{
                    "id": "UNRELATED", "slug": "repo", "sourceKind": "Git",
                    "remoteUrl": "https://example.test/other-org/repo.git"
                }]),
                ("POST", "/v1/codebases") => {
                    if project_keys {
                        assert_eq!(request.body["slug"], "checkout-a");
                    } else {
                        assert!(
                            request.body["slug"]
                                .as_str()
                                .unwrap()
                                .starts_with("checkout-a-")
                        );
                    }
                    json!({"id": "NEW", "slug": request.body["slug"], "sourceKind": "Local"})
                }
                ("POST", "/v1/codebases/NEW/sync") => {
                    json!({"jobId": "job", "needContent": [], "toDelete": []})
                }
                _ => panic!("unexpected project request: {request:?}"),
            };
            let total = data.as_array().map_or(0, Vec::len);
            json!({"success": true, "data": data, "page": 0, "pageSize": 500, "total": total})
        });
        let output = checkout
            .run(&server, &checkout.root, &["index", "--no-wait"])
            .await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            server
                .requests()
                .iter()
                .any(|r| r.path == "/v1/codebases/NEW/sync")
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn native_checkout_paths_submit_distinct_stable_source_identities() {
    use std::os::unix::ffi::OsStringExt;

    let checkout = Checkout::new();
    let names = [
        b"same\\checkout".to_vec(),
        b"same/checkout".to_vec(),
        b"native-\xff".to_vec(),
        b"native-\xfe".to_vec(),
    ];
    let server = Server::start(|request| {
        let data = match request.path.as_str() {
            "/v1/whoami" => json!({"capabilities": []}),
            "/v1/codebases/A/sync" => json!({"jobId": "job", "needContent": [], "toDelete": []}),
            path => panic!("unexpected source identity request: {path}"),
        };
        json!({"success": true, "data": data})
    });
    for name in names {
        let root = checkout.root.join(std::ffi::OsString::from_vec(name));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        for _ in 0..2 {
            let output = checkout
                .run(&server, &root, &["--codebase", "A", "index", "--no-wait"])
                .await;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    let requests = server.requests();
    let sources: Vec<_> = requests
        .iter()
        .filter(|r| r.path.ends_with("/sync"))
        .map(|r| r.body["sourceId"].as_str().unwrap())
        .collect();
    assert_eq!(sources.len(), 8);
    for pair in sources.as_chunks::<2>().0 {
        assert_eq!(pair[0], pair[1]);
    }
    let unique: std::collections::HashSet<_> = sources.into_iter().collect();
    assert_eq!(
        unique.len(),
        4,
        "each distinct checkout needs a distinct manifest identity"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ambiguous_legacy_cache_cannot_reuse_a_shared_single_manifest_codebase() {
    let checkout = Checkout::new();
    let slash = checkout.root.join("same/checkout");
    let backslash = checkout.root.join("same\\checkout");
    for root in [&slash, &backslash] {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
    }
    let cache = HashMap::from([
        (slash.to_str().unwrap(), "LEGACY"),
        (backslash.to_str().unwrap(), "LEGACY"),
    ]);
    fs::write(
        checkout.config.join("semctl/config.toml"),
        toml::to_string(&json!({"codebase_cache": cache})).unwrap(),
    )
    .unwrap();
    let server = Server::start(|request| {
        let data = match (request.method.as_str(), request.path.as_str()) {
            (_, "/v1/whoami") => json!({"capabilities": []}),
            (_, "/v1/codebases/LEGACY") => {
                json!({"id": "LEGACY", "slug": "legacy", "sourceKind": "Local"})
            }
            (_, path) if path.starts_with("/v1/codebases?") => json!([]),
            ("POST", "/v1/codebases") => {
                json!({"id": "NEW", "slug": request.body["slug"], "sourceKind": "Local"})
            }
            ("POST", "/v1/codebases/LEGACY/sync" | "/v1/codebases/NEW/sync") => {
                json!({"jobId": "job", "needContent": [], "toDelete": []})
            }
            _ => panic!("unexpected cache migration request: {request:?}"),
        };
        json!({"success": true, "data": data, "page": 0, "pageSize": 500, "total": 0})
    });
    for root in [&backslash, &slash] {
        let output = checkout.run(&server, root, &["index", "--no-wait"]).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let requests = server.requests();
    let manifests: Vec<_> = requests
        .iter()
        .filter(|request| request.path.ends_with("/sync"))
        .collect();
    assert_eq!(manifests[0].path, "/v1/codebases/NEW/sync");
    assert_eq!(manifests[1].path, "/v1/codebases/LEGACY/sync");
    assert_ne!(manifests[0].body["sourceId"], manifests[1].body["sourceId"]);
}
