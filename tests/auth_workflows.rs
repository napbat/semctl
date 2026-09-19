//! Login state across real CLI processes. All authorities and files are isolated.

use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, mpsc};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    binary: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let binary = root.join(if cfg!(windows) {
            "semctl.exe"
        } else {
            "semctl"
        });
        std::fs::copy(env!("CARGO_BIN_EXE_semctl"), &binary).unwrap();
        std::fs::create_dir(root.join("home")).unwrap();
        Self {
            _directory: directory,
            root,
            binary,
        }
    }

    fn command(&self, server: Option<&str>, args: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("USERPROFILE", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("CODEX_HOME", self.root.join("home/.codex"))
            .env("PATH", self.root.join("empty-path"))
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        if let Some(server) = server {
            command.args(["--server", server]);
        }
        command.args(args);
        command
    }

    fn credentials_path(&self) -> PathBuf {
        self.root.join("config/semctl/credentials.json")
    }
    fn credentials(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.credentials_path()).unwrap()).unwrap()
    }
    fn config(&self) -> toml::Value {
        toml::from_str(
            &std::fs::read_to_string(self.root.join("config/semctl/config.toml")).unwrap(),
        )
        .unwrap()
    }
}

async fn completed(child: Child) -> Output {
    tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("CLI completed before timeout")
        .unwrap()
}

fn succeeded(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Authority {
    url: String,
    events: mpsc::UnboundedReceiver<String>,
    release: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Authority {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Authority {
    async fn new(tenant: &str, paused: Option<&str>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (events, receiver) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let task_release = release.clone();
        let server_url = url.clone();
        let tenant = tenant.to_string();
        let paused = paused.map(str::to_string);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let events = events.clone();
                let release = task_release.clone();
                let url = server_url.clone();
                let tenant = tenant.clone();
                let paused = paused.clone();
                tokio::spawn(async move {
                    let request = read_request(&mut stream).await;
                    let path = request.split_whitespace().nth(1).unwrap();
                    let event = match path {
                        "/connect/token" if request.contains("grant_type=refresh_token") => {
                            "refresh"
                        }
                        "/connect/token" => "device-token",
                        "/v1/tenants" => "tenants",
                        other => other,
                    };
                    let ready = release.notified();
                    events.send(event.to_string()).unwrap();
                    if paused.as_deref() == Some(event) {
                        ready.await;
                    }
                    let body = match path {
                        "/.well-known/oauth-protected-resource" => {
                            json!({"authorization_servers":[url]})
                        }
                        "/connect/device" => {
                            json!({"device_code":"fake-device", "user_code":"FAKE", "verification_uri":format!("{url}/activate"), "expires_in":30, "interval":1})
                        }
                        "/connect/token" => {
                            use base64::Engine;
                            let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
                                .encode(json!({"sub":"fake-user", "iss":url}).to_string());
                            json!({"access_token":format!("header.{payload}.signature"), "refresh_token":"fake-refresh", "expires_in":3600})
                        }
                        "/v1/tenants" => {
                            json!({"success":true, "data":[{"id":tenant, "slug":tenant, "name":tenant}]})
                        }
                        "/v1/domains" => json!({"success":true, "data":[]}),
                        other => panic!("unexpected request path: {other}"),
                    };
                    let body = body.to_string();
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                });
            }
        });
        Self {
            url,
            events: receiver,
            release,
            task,
        }
    }

    async fn wait_for(&mut self, target: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.events.recv().await.unwrap() == target {
                    return;
                }
            }
        })
        .await
        .expect("authority received the expected request");
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut request = Vec::new();
    loop {
        let mut buffer = [0_u8; 1024];
        let length = stream.read(&mut buffer).await.unwrap();
        assert!(length > 0);
        request.extend_from_slice(&buffer[..length]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let body_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if request.len() >= end + 4 + body_length {
                return String::from_utf8(request).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn empty_server_environment_keeps_the_persisted_login_server() {
    let fixture = Fixture::new();
    let authority = Authority::new("tenant", None).await;
    succeeded(
        &completed(
            fixture
                .command(Some(&authority.url), &["auth", "login", "--no-open"])
                .spawn()
                .unwrap(),
        )
        .await,
    );
    let output = completed(
        fixture
            .command(None, &["auth", "whoami"])
            .env("SEMCTX_SERVER", "")
            .spawn()
            .unwrap(),
    )
    .await;
    succeeded(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("server: ok"));
}

#[tokio::test]
async fn delayed_login_tenant_result_cannot_replace_the_new_login() {
    let fixture = Fixture::new();
    let mut first = Authority::new("first-tenant", Some("tenants")).await;
    let second = Authority::new("second-tenant", None).await;
    let old_login = fixture
        .command(Some(&first.url), &["auth", "login", "--no-open"])
        .spawn()
        .unwrap();
    first.wait_for("tenants").await;
    succeeded(
        &completed(
            fixture
                .command(Some(&second.url), &["auth", "login", "--no-open"])
                .spawn()
                .unwrap(),
        )
        .await,
    );
    first.release.notify_waiters();
    succeeded(&completed(old_login).await);
    assert_eq!(
        fixture.config()["active_tenant"].as_str(),
        Some("second-tenant")
    );
    assert_eq!(fixture.credentials()["session"]["server_url"], second.url);
    assert_eq!(fixture.credentials()["session"]["generation"], 2);
    let old_server_request = completed(
        fixture
            .command(Some(&first.url), &["auth", "whoami"])
            .spawn()
            .unwrap(),
    )
    .await;
    assert!(!old_server_request.status.success());
    assert!(String::from_utf8_lossy(&old_server_request.stderr).contains("another server"));
    assert!(
        first.events.try_recv().is_err(),
        "old server received no bearer or refresh request"
    );
}

#[tokio::test]
async fn purge_waits_for_inflight_refresh_and_leaves_no_credentials() {
    let fixture = Fixture::new();
    let mut authority = Authority::new("tenant", Some("refresh")).await;
    succeeded(
        &completed(
            fixture
                .command(Some(&authority.url), &["auth", "login", "--no-open"])
                .spawn()
                .unwrap(),
        )
        .await,
    );
    let mut credentials = fixture.credentials();
    credentials["expires_at_unix"] = 0.into();
    std::fs::write(
        fixture.credentials_path(),
        serde_json::to_vec(&credentials).unwrap(),
    )
    .unwrap();
    let refreshing = fixture
        .command(Some(&authority.url), &["auth", "whoami"])
        .spawn()
        .unwrap();
    authority.wait_for("refresh").await;
    let mut purging = fixture
        .command(None, &["uninstall", "--purge"])
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        purging.try_wait().unwrap().is_none(),
        "purge waits for the credential writer"
    );
    authority.release.notify_waiters();
    // Purge can run before whoami's later liveness probe. Its token was already
    // acquired, so only credential removal determines the final state here.
    completed(refreshing).await;
    succeeded(&completed(purging).await);
    assert!(!fixture.credentials_path().exists());
    assert!(fixture.root.join("config/.semctl.state.lock").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("config/.semctl.state.generation")).unwrap(),
        "2"
    );
}

#[tokio::test]
async fn purge_invalidates_a_device_login_that_has_not_published() {
    let fixture = Fixture::new();
    let mut authority = Authority::new("tenant", Some("device-token")).await;
    let pending = fixture
        .command(Some(&authority.url), &["auth", "login", "--no-open"])
        .spawn()
        .unwrap();
    authority.wait_for("device-token").await;
    succeeded(
        &completed(
            fixture
                .command(None, &["uninstall", "--purge"])
                .spawn()
                .unwrap(),
        )
        .await,
    );
    authority.release.notify_waiters();
    let output = completed(pending).await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("login state changed"));
    assert!(!fixture.credentials_path().exists());
}
