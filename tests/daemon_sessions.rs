//! End-to-end behavior of the shared local daemon.
//!
//! Every test owns one endpoint: its own configuration directory and its own
//! runtime directory, so two tests never share a daemon. Each test stops its
//! daemon through the [`Endpoint`] guard, which runs even when an assertion
//! fails.
//!
//! The server URL is a port nothing listens on, so every remote request fails
//! at once. That is deliberate: these tests are about process lifecycle, and a
//! failing startup reconcile must not change any of it.
//!
//! Unix only for now. The Windows named pipe path needs a Windows runner.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// A port nothing listens on: every request fails without a network wait.
const DEAD_SERVER: &str = "http://127.0.0.1:9";

/// Longest wait for a state a test polls for. Generous: a loaded machine can
/// take seconds to start a process and bind an endpoint.
const POLL_TIMEOUT: Duration = Duration::from_secs(60);

/// Wait between polls.
const POLL_STEP: Duration = Duration::from_millis(100);

/// Longest wait for one process to exit.
const EXIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest wait for one MCP response.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// Idle delay for a test that must not see an idle exit.
const LONG_IDLE: &str = "600";

/// One isolated daemon endpoint.
///
/// The directories are canonical, and the configuration directory exists
/// before the first process starts, so every process computes the same
/// endpoint identity from the same path.
struct Endpoint {
    home: tempfile::TempDir,
    config_home: PathBuf,
    runtime_dir: PathBuf,
    /// Client processes started against this endpoint, for stderr file names.
    started: std::cell::Cell<usize>,
}

impl Endpoint {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create an isolated endpoint directory");
        let root = std::fs::canonicalize(home.path()).expect("canonicalize the endpoint directory");
        let config_home = root.join("config");
        // `semctl` reads `<XDG_CONFIG_HOME>/semctl`. Creating it up front keeps
        // the endpoint identity the same for every process in the test.
        std::fs::create_dir_all(config_home.join("semctl")).expect("create the config directory");
        let runtime_dir = root.join("run");
        std::fs::create_dir_all(&runtime_dir).expect("create the runtime directory");
        std::fs::set_permissions(
            &runtime_dir,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .expect("make the runtime directory private");
        Self {
            home,
            config_home,
            runtime_dir,
            started: std::cell::Cell::new(0),
        }
    }

    /// An endpoint whose runtime directory cannot hold a socket.
    fn with_unusable_runtime_dir() -> Self {
        let mut endpoint = Self::new();
        let file = endpoint.path().join("not-a-directory");
        std::fs::write(&file, b"this is a regular file\n").expect("create the blocking file");
        endpoint.runtime_dir = file;
        endpoint
    }

    fn path(&self) -> &Path {
        self.home.path()
    }

    /// A `semctl` command against this endpoint, with the per-session
    /// variables of the test process removed.
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_semctl"));
        command
            .env("XDG_CONFIG_HOME", &self.config_home)
            // Linux names the runtime directory; macOS uses the temporary
            // directory. Setting both keeps one fixture correct on each.
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("TMPDIR", &self.runtime_dir)
            .env("SEMCTX_MCP_UPDATE_CHECK", "0")
            .env_remove("SEMCTX_TOKEN")
            .env_remove("SEMCTX_SERVER")
            .env_remove("SEMCTX_TENANT")
            .env_remove("SEMCTX_CODEBASE")
            .env_remove("SEMCTX_MCP_RESYNC_SECS")
            .env_remove("SEMCTX_MCP_DAEMON")
            .kill_on_drop(true);
        command
    }

    /// Write the configuration file that records `root` as the cached
    /// checkout of `codebase`, exactly as `semctl index` would.
    fn cache_codebase(&self, root: &Path, codebase: &str) {
        let key = root.to_str().expect("a printable checkout path");
        assert!(!key.contains(['"', '\\']), "{key} needs TOML escaping");
        std::fs::write(
            self.config_home.join("semctl").join("config.toml"),
            format!("[codebase_cache]\n\"{key}\" = \"{codebase}\"\n"),
        )
        .expect("write the config file");
    }

    /// Start one `semctl mcp` client against this endpoint.
    fn start_client(&self, mode: &str, options: ClientOptions<'_>) -> McpClient {
        let ordinal = self.started.get() + 1;
        self.started.set(ordinal);
        let log = std::fs::File::create(self.path().join(format!("client-{ordinal}.err")))
            .expect("create the client log");
        let mut command = self.command();
        command
            .args(["--server", DEAD_SERVER])
            .env("SEMCTX_MCP_DAEMON", mode)
            .env("SEMCTX_DAEMON_IDLE_SECS", options.idle_secs)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        if let Some(codebase) = options.codebase {
            command.args(["--codebase", codebase]);
        }
        if let Some(token) = options.token {
            command.env("SEMCTX_TOKEN", token);
        }
        if let Some(cwd) = options.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.arg("mcp").spawn().expect("start the MCP client");
        let stdin = child.stdin.take().expect("piped client input");
        let stdout = BufReader::new(child.stdout.take().expect("piped client output")).lines();
        McpClient {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }

    /// Ask the daemon for its status. `None` means no daemon answered.
    async fn status(&self) -> Option<Value> {
        let output = self
            .command()
            .args(["daemon", "status", "--json"])
            .output()
            .await
            .expect("run `semctl daemon status`");
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("no daemon is running for this configuration"),
                "an unexpected status failure: {stderr}"
            );
            return None;
        }
        Some(serde_json::from_slice(&output.stdout).expect("parse the daemon status"))
    }

    /// Poll the status until `ready` accepts it.
    async fn await_status<F>(&self, label: &str, ready: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        let deadline = Instant::now() + POLL_TIMEOUT;
        let mut last = Value::Null;
        loop {
            if let Some(status) = self.status().await {
                if ready(&status) {
                    return status;
                }
                last = status;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {label}; the last status was {last}"
            );
            tokio::time::sleep(POLL_STEP).await;
        }
    }

    /// Poll until no daemon answers this endpoint.
    async fn await_no_daemon(&self, label: &str) {
        let deadline = Instant::now() + POLL_TIMEOUT;
        loop {
            match self.status().await {
                None => return,
                Some(status) => assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {label}; the daemon still answers: {status}"
                ),
            }
            tokio::time::sleep(POLL_STEP).await;
        }
    }

    /// Ask the daemon to stop and report what it said.
    async fn stop(&self) -> std::process::Output {
        self.command()
            .args(["daemon", "stop"])
            .output()
            .await
            .expect("run `semctl daemon stop`")
    }

    /// Everything the daemons of this endpoint logged.
    ///
    /// A daemon's standard error is the endpoint log, so every daemon of this
    /// endpoint — the one that won the election and any that lost — appends to
    /// the same file. The log lives in `<XDG_RUNTIME_DIR>/semctl`.
    fn daemon_log(&self) -> String {
        let mut text = String::new();
        let Ok(entries) = std::fs::read_dir(self.runtime_dir.join("semctl")) else {
            return text;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.extension().is_some_and(|extension| extension == "log") {
                text.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
        text
    }
}

impl Drop for Endpoint {
    /// Stop this endpoint's daemon, even when the test failed.
    ///
    /// A daemon outlives the client that started it, so a test that panicked
    /// must not leave one behind. This runs before the temporary directory is
    /// removed, and it is best effort: there may be no daemon to stop.
    fn drop(&mut self) {
        let stopped = std::process::Command::new(env!("CARGO_BIN_EXE_semctl"))
            .args(["daemon", "stop"])
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("TMPDIR", &self.runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !stopped.is_ok_and(|status| status.success()) {
            return;
        }
        // Wait for the daemon to go, so its socket and its watchers are
        // released before the directory is removed.
        let deadline = Instant::now() + POLL_TIMEOUT;
        while Instant::now() < deadline {
            let answered = std::process::Command::new(env!("CARGO_BIN_EXE_semctl"))
                .args(["daemon", "status"])
                .env("XDG_CONFIG_HOME", &self.config_home)
                .env("XDG_RUNTIME_DIR", &self.runtime_dir)
                .env("TMPDIR", &self.runtime_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if !answered.is_ok_and(|status| status.success()) {
                return;
            }
            std::thread::sleep(POLL_STEP);
        }
    }
}

/// What one client invocation differs by.
#[derive(Clone, Copy, Default)]
struct ClientOptions<'a> {
    /// `SEMCTX_DAEMON_IDLE_SECS` for the daemon this client may start.
    idle_secs: &'a str,
    /// `--codebase`, when the session is pinned.
    codebase: Option<&'a str>,
    /// `SEMCTX_TOKEN`, when the session carries its own credentials.
    token: Option<&'a str>,
    /// The working directory the session is invoked from.
    cwd: Option<&'a Path>,
}

impl ClientOptions<'_> {
    fn new() -> Self {
        Self {
            idle_secs: LONG_IDLE,
            ..Self::default()
        }
    }
}

/// One `semctl mcp` client process, driven over its standard streams.
struct McpClient {
    child: Child,
    /// Taken when the test closes the client's input.
    stdin: Option<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl McpClient {
    async fn send(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).expect("serialize an MCP message");
        bytes.push(b'\n');
        self.stdin
            .as_mut()
            .expect("the client input is still open")
            .write_all(&bytes)
            .await
            .expect("write an MCP message");
    }

    async fn receive(&mut self) -> Value {
        let line = tokio::time::timeout(RESPONSE_TIMEOUT, self.stdout.next_line())
            .await
            .expect("an MCP response timed out")
            .expect("read an MCP response")
            .expect("the client closed its output before it answered");
        serde_json::from_str(&line).expect("parse an MCP response")
    }

    /// Complete the MCP handshake and list the tools.
    async fn initialize(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": rmcp::model::ProtocolVersion::default(),
                "capabilities": {},
                "clientInfo": { "name": "daemon-sessions", "version": "1" }
            }
        }))
        .await;
        let initialized = self.receive().await;
        assert_eq!(initialized["id"], 1);
        assert!(
            initialized["result"]["capabilities"]["tools"].is_object(),
            "{initialized}"
        );
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;

        self.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
            .await;
        let listed = self.receive().await;
        assert_eq!(listed["id"], 2);
        let tools = listed["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("a tool catalog: {listed}"));
        assert!(!tools.is_empty(), "{listed}");
    }

    /// Close the client's input, which ends its session.
    fn close_input(&mut self) {
        drop(self.stdin.take());
    }

    /// Wait for the client to exit.
    async fn wait(&mut self) -> ExitStatus {
        tokio::time::timeout(EXIT_TIMEOUT, self.child.wait())
            .await
            .expect("the client did not exit in time")
            .expect("wait for the client")
    }
}

/// One daemon serves five sessions, counts them, and stops when asked.
#[tokio::test(flavor = "multi_thread")]
async fn one_daemon_serves_five_sessions_and_stops_on_request() {
    let endpoint = Endpoint::new();
    let mut clients = Vec::new();
    for _ in 0..5 {
        let mut client = endpoint.start_client("require", ClientOptions::new());
        client.initialize().await;
        clients.push(client);
    }

    let status = endpoint
        .await_status("five sessions", |status| status["sessions"] == 5)
        .await;
    let pid = status["pid"].as_u64().expect("the daemon pid");
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));

    for client in &mut clients {
        client.close_input();
    }
    for mut client in clients {
        let exit = client.wait().await;
        assert!(exit.success(), "a client exited with {exit}");
    }

    let idle = endpoint
        .await_status("no session", |status| status["sessions"] == 0)
        .await;
    assert_eq!(
        idle["pid"].as_u64(),
        Some(pid),
        "the same daemon must still serve the endpoint"
    );

    let stopped = endpoint.stop().await;
    assert!(stopped.status.success(), "{stopped:?}");
    assert!(
        String::from_utf8_lossy(&stopped.stdout).contains(&format!("semctl daemon {pid}")),
        "{stopped:?}"
    );
    endpoint.await_no_daemon("the stopped daemon to exit").await;
}

/// Two clients that start at the same time must produce one daemon, not two.
#[tokio::test(flavor = "multi_thread")]
async fn two_clients_starting_at_once_produce_one_daemon() {
    let endpoint = Endpoint::new();
    // Both processes exist before either one is driven, so both race for the
    // same endpoint with no daemon running.
    let mut first = endpoint.start_client("require", ClientOptions::new());
    let mut second = endpoint.start_client("require", ClientOptions::new());
    first.initialize().await;
    second.initialize().await;

    let status = endpoint
        .await_status("two sessions", |status| status["sessions"] == 2)
        .await;
    let pid = status["pid"].as_u64().expect("the daemon pid");

    // The election is what makes this one daemon: a daemon that lost never
    // serves, and every daemon of this endpoint logs to the same file.
    let log = endpoint.daemon_log();
    assert_eq!(
        log.matches("serves the local endpoint").count(),
        1,
        "exactly one daemon may serve this endpoint:\n{log}"
    );

    for client in [&mut first, &mut second] {
        client.close_input();
        let exit = client.wait().await;
        assert!(exit.success(), "a client exited with {exit}");
    }
    let idle = endpoint
        .await_status("no session", |status| status["sessions"] == 0)
        .await;
    assert_eq!(idle["pid"].as_u64(), Some(pid));
}

/// An endpoint that cannot be used is a fallback for `auto` and a failure for
/// `require`.
#[tokio::test(flavor = "multi_thread")]
async fn an_unusable_endpoint_falls_back_only_in_auto_mode() {
    let endpoint = Endpoint::with_unusable_runtime_dir();

    let mut automatic = endpoint.start_client("auto", ClientOptions::new());
    automatic.initialize().await;
    automatic.close_input();
    let exit = automatic.wait().await;
    assert!(
        exit.success(),
        "`auto` must serve the session in this process: {exit}"
    );

    let mut required = endpoint.start_client("require", ClientOptions::new());
    let exit = required.wait().await;
    assert!(
        !exit.success(),
        "`require` must fail when the daemon is unavailable"
    );
    let mut served = Vec::new();
    required
        .stdout
        .get_mut()
        .read_to_end(&mut served)
        .await
        .expect("read what the refused client wrote");
    assert!(
        served.is_empty(),
        "a refused client must serve nothing: {}",
        String::from_utf8_lossy(&served)
    );
}

/// Two sessions on one checkout share one coordinator, and it reports both
/// leases.
#[tokio::test(flavor = "multi_thread")]
async fn two_sessions_on_one_checkout_share_one_coordinator() {
    let endpoint = Endpoint::new();
    let checkout = endpoint.path().join("checkout");
    std::fs::create_dir_all(checkout.join("src")).expect("create the checkout");
    std::fs::write(checkout.join("src/main.rs"), b"fn main() {}\n").expect("write a source file");
    // A Git checkout is what a person indexes. A machine without `git` still
    // runs this test: the cached root below is what binds the codebase.
    let _ = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&checkout)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let checkout = std::fs::canonicalize(&checkout).expect("canonicalize the checkout");
    endpoint.cache_codebase(&checkout, "test-id");

    let options = ClientOptions {
        idle_secs: LONG_IDLE,
        codebase: Some("test-id"),
        token: Some("test-token"),
        cwd: Some(&checkout),
    };
    let mut first = endpoint.start_client("require", options);
    let mut second = endpoint.start_client("require", options);
    first.initialize().await;
    second.initialize().await;

    // The startup reconcile fails against the dead server. That must not stop
    // the coordinator from existing or from counting its leases.
    let status = endpoint
        .await_status("one coordinator with two leases", |status| {
            status["coordinators"]
                .as_array()
                .is_some_and(|checkouts| checkouts.len() == 1 && checkouts[0]["leases"] == 2)
        })
        .await;
    let coordinator = &status["coordinators"][0];
    assert_eq!(coordinator["root"].as_str(), checkout.to_str(), "{status}");
    assert_eq!(coordinator["codebase_id"], "test-id", "{status}");
    assert_eq!(status["sessions"], 2, "{status}");

    for client in [&mut first, &mut second] {
        client.close_input();
        let exit = client.wait().await;
        assert!(exit.success(), "a client exited with {exit}");
    }
}

/// A daemon that nothing uses exits by itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_exits_after_its_last_session_leaves() {
    let endpoint = Endpoint::new();
    let mut client = endpoint.start_client(
        "require",
        ClientOptions {
            idle_secs: "1",
            ..ClientOptions::new()
        },
    );
    client.initialize().await;
    endpoint
        .await_status("the session to be counted", |status| {
            status["sessions"] == 1
        })
        .await;

    client.close_input();
    let exit = client.wait().await;
    assert!(exit.success(), "the client exited with {exit}");

    endpoint.await_no_daemon("the idle daemon to exit").await;
}
