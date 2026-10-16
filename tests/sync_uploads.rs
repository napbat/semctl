//! Process-isolated regressions for failed upload requests during `semctl index`.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

/// Enough files for two upload batches: the first batch holds 256 files.
const FILES: usize = 300;
/// A path in the second upload batch.
const SECOND_BATCH: &str = "src/f299.rs";
/// A path in the first upload batch.
const FIRST_BATCH: &str = "src/f000.rs";

#[derive(Clone)]
struct Request {
    method: String,
    body: Value,
}

impl Request {
    fn uploads(&self, path: &str) -> bool {
        self.method == "PUT"
            && self.body["files"]
                .as_array()
                .is_some_and(|files| files.iter().any(|file| file["path"] == path))
    }

    fn completes_the_sync(&self) -> bool {
        self.method == "PUT"
            && self.body["final"] == true
            && self.body["files"].as_array().is_some_and(Vec::is_empty)
    }
}

/// How the fixture answers one request.
enum Reply {
    /// A success envelope.
    Success,
    /// A failure status with a plain-text body, as a gateway sends it.
    Failure {
        status: u16,
        retry_after: Option<u64>,
    },
    /// Close the connection without an answer.
    Close,
}

struct Server {
    address: SocketAddr,
    stopped: Arc<AtomicBool>,
    /// The worker returns every request it received, in order.
    worker: Option<JoinHandle<Vec<Request>>>,
}

impl Server {
    /// Answer each upload with `reply`, which also sees every earlier request.
    /// A manifest is answered with a plan that requests every listed file.
    fn start(reply: impl Fn(&Request, &[Request]) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = std::thread::spawn(move || {
            let mut recorded = Vec::new();
            for connection in listener.incoming() {
                let mut stream = connection.unwrap();
                if stop.load(Ordering::Acquire) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let request = read_request(&stream);
                let answer = if request.method == "PUT" {
                    reply(&request, &recorded)
                } else {
                    Reply::Success
                };
                respond(&mut stream, &request, &answer);
                recorded.push(request);
            }
            recorded
        });
        Self {
            address,
            stopped,
            worker: Some(worker),
        }
    }

    /// Stop the fixture and return every request it received, in order.
    fn stop(mut self) -> Vec<Request> {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        self.worker
            .take()
            .expect("a running fixture has a worker")
            .join()
            .unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn uploads_of(requests: &[Request], path: &str) -> usize {
    requests
        .iter()
        .filter(|request| request.uploads(path))
        .count()
}

fn completed_the_sync(requests: &[Request]) -> bool {
    requests.iter().any(Request::completes_the_sync)
}

fn read_request(stream: &TcpStream) -> Request {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let method = line.split_whitespace().next().unwrap().to_string();
    let mut length = 0;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).unwrap();
    Request {
        method,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn respond(stream: &mut TcpStream, request: &Request, reply: &Reply) {
    match *reply {
        Reply::Success => {
            let data = if request.method == "POST" {
                let paths: Vec<_> = request.body["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| entry["path"].clone())
                    .collect();
                json!({"jobId": "job", "needContent": paths, "toDelete": []})
            } else {
                json!({})
            };
            let body = serde_json::to_vec(&json!({"success": true, "data": data})).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
        Reply::Failure {
            status,
            retry_after,
        } => {
            let body = "Client Closed Request";
            let retry = retry_after
                .map(|seconds| format!("Retry-After: {seconds}\r\n"))
                .unwrap_or_default();
            write!(
                stream,
                "HTTP/1.1 {status} Fixture\r\nContent-Type: text/plain\r\n{retry}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        Reply::Close => {}
    }
}

struct Checkout {
    _directory: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
}

impl Checkout {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        for n in 0..FILES {
            fs::write(
                root.join(format!("src/f{n:03}.rs")),
                format!("fn f{n:03}() {{}}\n"),
            )
            .unwrap();
        }
        let config = directory.path().join("xdg");
        fs::create_dir_all(config.join("semctl")).unwrap();
        fs::write(config.join("semctl/installation-id"), "a".repeat(64)).unwrap();
        Self {
            _directory: directory,
            root,
            config,
        }
    }

    fn index(&self, server: &Server) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_semctl"));
        command
            .args(["--server", &format!("http://{}", server.address)])
            .args(["--tenant", "test", "--codebase", "A", "index"])
            .arg(&self.root)
            .arg("--no-wait")
            .current_dir(&self.root)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("SEMCTX_TOKEN", "offline-fixture-token")
            .env("NO_PROXY", "*");
        for name in [
            "SEMCTX_CODEBASE",
            "SEMCTX_SERVER",
            "SEMCTX_TENANT",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
        ] {
            command.env_remove(name);
        }
        command.output().unwrap()
    }
}

/// The reported defect: one batch that a gateway cut off stopped every other
/// batch, and the sync was never completed.
#[test]
fn a_failed_batch_does_not_stop_the_other_batches() {
    let checkout = Checkout::new();
    let server = Server::start(|request, _| {
        if request.uploads(SECOND_BATCH) {
            Reply::Failure {
                status: 499,
                retry_after: Some(0),
            }
        } else {
            Reply::Success
        }
    });

    let output = checkout.index(&server);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let requests = server.stop();

    assert!(
        !output.status.success(),
        "an incomplete upload must be reported: {stderr}"
    );
    assert!(
        stderr.contains("44 of 300 requested file(s) were not uploaded"),
        "{stderr}"
    );
    assert!(stderr.contains("src/f256.rs"), "{stderr}");
    assert_eq!(uploads_of(&requests, FIRST_BATCH), 1);
    assert_eq!(
        uploads_of(&requests, SECOND_BATCH),
        2,
        "a retryable failure gets exactly one more attempt"
    );
    assert!(
        completed_the_sync(&requests),
        "the delivered batches must still be completed"
    );
}

#[test]
fn a_dropped_connection_is_retried_and_the_sync_completes() {
    let checkout = Checkout::new();
    let server = Server::start(|request, earlier| {
        let attempted = earlier.iter().any(|before| before.uploads(SECOND_BATCH));
        if request.uploads(SECOND_BATCH) && !attempted {
            Reply::Close
        } else {
            Reply::Success
        }
    });

    let output = checkout.index(&server);
    let requests = server.stop();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(uploads_of(&requests, SECOND_BATCH), 2);
    assert!(completed_the_sync(&requests));
}

#[test]
fn a_refused_sync_job_stops_the_upload_without_completing_it() {
    let checkout = Checkout::new();
    let server = Server::start(|request, _| {
        if request.uploads(SECOND_BATCH) {
            Reply::Failure {
                status: 409,
                retry_after: None,
            }
        } else {
            Reply::Success
        }
    });

    let output = checkout.index(&server);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let requests = server.stop();

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("409"), "{stderr}");
    assert_eq!(
        uploads_of(&requests, SECOND_BATCH),
        1,
        "a refusal of the sync job is not retried"
    );
    assert!(!completed_the_sync(&requests));
}
