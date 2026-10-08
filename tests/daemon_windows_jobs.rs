//! A daemon must not inherit an MCP host's nonbreakaway job lifetime.
//!
//! Each test assigns only its helper subprocess to its own kill-on-close job.
//! The helper waits for assignment before it starts any semctl process. Cargo
//! and the integration-test parent never enter that job.

#![cfg(windows)]

use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::ptr;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

const DEAD_SERVER: &str = "http://127.0.0.1:9";
const TIMEOUT: Duration = Duration::from_secs(30);
const POLL_STEP: Duration = Duration::from_millis(25);
const HELPER_TEST: &str = "job_client_helper";
const ROOT_VAR: &str = "SEMCTL_WINDOWS_JOB_TEST_ROOT";
const CASE_VAR: &str = "SEMCTL_WINDOWS_JOB_TEST_CASE";

/// This handle is not inheritable and has no name. Closing its sole handle
/// terminates all assigned processes, including a daemon started by old code.
struct Job(HANDLE);

impl Job {
    fn new(allow_breakaway: bool) -> Self {
        // SAFETY: null attributes select a noninheritable handle. The null
        // name creates a new job instead of opening another owner's job.
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        assert!(
            !handle.is_null(),
            "create job: {}",
            io::Error::last_os_error()
        );
        let job = Self(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if allow_breakaway {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        }
        let size = u32::try_from(std::mem::size_of_val(&limits)).expect("job limits fit a DWORD");
        // SAFETY: the handle is live and the buffer matches the selected class.
        let configured = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&limits).cast(),
                size,
            )
        };
        assert_ne!(
            configured,
            0,
            "configure job: {}",
            io::Error::last_os_error()
        );
        job
    }

    fn assign(&self, process: &Child) {
        // SAFETY: both handles remain live for the call. Only this test's
        // blocked helper enters the job; the Cargo/test parent does not.
        let assigned = unsafe { AssignProcessToJobObject(self.0, process.as_raw_handle()) };
        assert_ne!(
            assigned,
            0,
            "assign helper job: {}",
            io::Error::last_os_error()
        );
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: this guard owns the only job handle and closes it once.
        unsafe { CloseHandle(self.0) };
    }
}

/// All command configuration is local to a fresh directory. No test changes
/// the parent environment or uses a user's saved token or endpoint.
struct Endpoint {
    directory: tempfile::TempDir,
}

impl Endpoint {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("create an isolated job-test directory");
        let config = directory.path().join("config/semctl");
        std::fs::create_dir_all(&config).expect("create the isolated configuration directory");
        std::fs::write(config.join("config.toml"), "").expect("write an empty configuration");
        Self { directory }
    }

    fn command(root: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_semctl"));
        Self::configure(&mut command, root);
        command
    }

    fn configure(command: &mut Command, root: &Path) {
        command
            .current_dir(root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("SEMCTX_MCP_UPDATE_CHECK", "0")
            .env("RUST_LOG", "warn")
            .env("SEMCTX_DAEMON_IDLE_SECS", "600");
        for name in [
            "SEMCTX_TOKEN",
            "SEMCTX_SERVER",
            "SEMCTX_TENANT",
            "SEMCTX_CODEBASE",
            "SEMCTX_MCP_RESYNC_SECS",
            "SEMCTX_MCP_DAEMON",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(name);
        }
    }

    async fn status(root: &Path) -> Option<Value> {
        let mut command = tokio::process::Command::from(Self::command(root));
        let output = tokio::time::timeout(
            TIMEOUT,
            command
                .args(["daemon", "status", "--json"])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("daemon status has a bounded wait")
        .expect("run the isolated daemon status command");
        if output.status.success() {
            return Some(serde_json::from_slice(&output.stdout).expect("parse daemon status"));
        }
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("no daemon is running for this configuration"),
            "unexpected daemon status failure: {output:?}"
        );
        None
    }
}

/// Own one process handle. Cleanup never looks up or kills a process by PID.
struct OwnedProcess(Child);

impl OwnedProcess {
    async fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.0.try_wait().expect("poll the owned process") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the owned process did not exit in time"
            );
            tokio::time::sleep(POLL_STEP).await;
        }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        // Killing an already-exited process can fail. The retained process
        // handle identifies only this fixture's process, even if its PID is reused.
        if let Err(error) = self.0.kill() {
            eprintln!("owned job-test process cleanup: {error}");
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.0.try_wait().is_ok_and(|status| status.is_some()) {
                return;
            }
            std::thread::sleep(POLL_STEP);
        }
        eprintln!("owned job-test process did not exit after cleanup");
    }
}

async fn run_case(case: &str) {
    let endpoint = Endpoint::new();
    let root = endpoint.directory.path();
    let mut external = if case == "existing" {
        let mut command = Endpoint::command(root);
        command
            .args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                std::fs::File::create(root.join("daemon.err")).expect("create daemon log"),
            ));
        Some(OwnedProcess(
            command.spawn().expect("start an owned external daemon"),
        ))
    } else {
        None
    };
    if let Some(daemon) = &external {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = Endpoint::status(root).await {
                assert_eq!(status["pid"], daemon.0.id());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "external daemon did not become ready"
            );
            tokio::time::sleep(POLL_STEP).await;
        }
    }

    let job = Job::new(false);
    let inner = case.starts_with("nested-").then(|| Job::new(true));
    let mut command = Command::new(std::env::current_exe().expect("locate this test executable"));
    Endpoint::configure(&mut command, root);
    command
        .args(["--exact", HELPER_TEST, "--ignored", "--nocapture"])
        .env(ROOT_VAR, root)
        .env(CASE_VAR, case)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            std::fs::File::create(root.join("helper.out")).expect("create helper output"),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(root.join("helper.err")).expect("create helper error log"),
        ));
    let mut helper = OwnedProcess(command.spawn().expect("start the gated job-test helper"));
    job.assign(&helper.0);
    if let Some(inner) = &inner {
        inner.assign(&helper.0);
    }
    std::fs::write(root.join("assigned"), b"assigned\n").expect("release the assigned helper");
    let status = helper.wait().await;
    drop(inner);
    drop(job);
    assert!(
        status.success(),
        "job case {case} failed: {status}\n{}\n{}",
        std::fs::read_to_string(root.join("helper.out")).unwrap_or_default(),
        std::fs::read_to_string(root.join("helper.err")).unwrap_or_default()
    );
    if let Some(daemon) = external.as_mut() {
        let status = Endpoint::status(root)
            .await
            .expect("external daemon survives the client job");
        assert_eq!(status["pid"], daemon.0.id());
        assert_eq!(status["sessions"], 0);
    }
}

async fn initialize(client: &mut tokio::process::Child) {
    let mut stdin = client.stdin.take().expect("the MCP client has piped input");
    let mut stdout = BufReader::new(
        client
            .stdout
            .take()
            .expect("the MCP client has piped output"),
    )
    .lines();
    for message in [
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params": {
            "protocolVersion":rmcp::model::ProtocolVersion::default(), "capabilities":{},
            "clientInfo":{"name":"windows-job-test", "version":"1"}}}),
        json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
    ] {
        let mut bytes = serde_json::to_vec(&message).expect("serialize an MCP request");
        bytes.push(b'\n');
        tokio::time::timeout(TIMEOUT, stdin.write_all(&bytes))
            .await
            .expect("MCP input has a bounded wait")
            .expect("write an MCP request");
        if message.get("id").is_none() {
            continue;
        }
        let line = tokio::time::timeout(TIMEOUT, stdout.next_line())
            .await
            .expect("MCP response has a bounded wait")
            .expect("read an MCP response")
            .expect("the MCP client answers before it closes output");
        let response: Value = serde_json::from_str(&line).expect("parse an MCP response");
        assert_eq!(response["id"], message["id"]);
        if message["id"] == 1 {
            assert!(
                response["result"]["capabilities"]["tools"].is_object(),
                "{response}"
            );
        } else {
            assert!(
                response["result"]["tools"]
                    .as_array()
                    .is_some_and(|tools| !tools.is_empty()),
                "{response}"
            );
        }
    }
    // Keep the session open while the helper checks which process serves it.
    client.stdin = Some(stdin);
    client.stdout = Some(stdout.into_inner().into_inner());
}

#[tokio::test]
async fn auto_serves_mcp_without_publishing_a_daemon_when_breakaway_is_denied() {
    run_case("auto").await;
}

#[tokio::test]
async fn require_reports_spawn_failure_without_publishing_a_daemon_when_breakaway_is_denied() {
    run_case("require").await;
}

#[tokio::test]
async fn a_client_in_a_nonbreakaway_job_can_attach_to_an_existing_daemon() {
    run_case("existing").await;
}

#[tokio::test]
async fn auto_serves_standalone_when_breakaway_leaves_a_restrictive_ancestor_job() {
    run_case("nested-auto").await;
}

#[tokio::test]
async fn require_reports_startup_failure_when_breakaway_leaves_a_restrictive_ancestor_job() {
    run_case("nested-require").await;
}

#[tokio::test]
#[ignore = "run only as the gated subprocess of an owning Windows job test"]
async fn job_client_helper() {
    let root =
        PathBuf::from(std::env::var_os(ROOT_VAR).expect("the parent supplies an isolated root"));
    let case = std::env::var(CASE_VAR).expect("the parent supplies the job-test case");
    let deadline = Instant::now() + TIMEOUT;
    while !root.join("assigned").exists() {
        assert!(
            Instant::now() < deadline,
            "the parent did not assign the helper job"
        );
        tokio::time::sleep(POLL_STEP).await;
    }
    let mode = if case.ends_with("auto") {
        "auto"
    } else {
        "require"
    };
    let log_path = root.join("client.err");
    let mut command = tokio::process::Command::from(Endpoint::command(&root));
    let mut client = command
        .args([
            "--server",
            DEAD_SERVER,
            "--codebase",
            "windows-job-test",
            "mcp",
        ])
        .env("SEMCTX_MCP_DAEMON", mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(
            std::fs::File::create(&log_path).expect("create client log"),
        ))
        .kill_on_drop(true)
        .spawn()
        .expect("start the isolated MCP client");
    if case.ends_with("require") {
        drop(client.stdin.take());
        let status = tokio::time::timeout(TIMEOUT, client.wait())
            .await
            .expect("require exits within its bounded wait")
            .expect("wait for require exit");
        assert_eq!(status.code(), Some(1));
        let error = std::fs::read_to_string(log_path).expect("read the require error");
        assert!(
            error.contains("requires the shared daemon")
                && (error.contains("start a semctl daemon")
                    || error.contains("detached")
                    || error.contains("exited")),
            "{error}"
        );
        assert!(
            Endpoint::status(&root).await.is_none(),
            "require must not publish a job-owned daemon"
        );
    } else {
        initialize(&mut client).await;
        let status = Endpoint::status(&root).await;
        if case.ends_with("auto") {
            assert!(
                status.is_none(),
                "auto must serve standalone instead of publishing a job-owned daemon: {status:?}"
            );
            let log = std::fs::read_to_string(log_path).expect("read the standalone warning");
            assert!(
                log.contains("serving this session in this process"),
                "{log}"
            );
        } else {
            assert_eq!(
                status.expect("the existing daemon still answers")["sessions"],
                1
            );
        }
        drop(client.stdin.take());
        let status = tokio::time::timeout(TIMEOUT, client.wait())
            .await
            .expect("MCP shutdown has a bounded wait")
            .expect("wait for MCP shutdown");
        assert!(status.success(), "{status}");
    }
}
