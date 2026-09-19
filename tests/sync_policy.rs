//! Process-isolated source-policy and public sync-manifest regressions.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{Value, json};

struct Server {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn start(before_plan: impl Fn() + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = std::thread::spawn(move || {
            for connection in listener.incoming() {
                let mut stream = connection.unwrap();
                if stop.load(Ordering::Acquire) {
                    break;
                }
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
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
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                recorded
                    .lock()
                    .unwrap()
                    .push((method.clone(), request.clone()));
                let data = if method == "POST" {
                    before_plan();
                    let paths: Vec<_> = request["files"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|entry| entry["path"].clone())
                        .collect();
                    json!({"jobId": "fixture-job", "needContent": paths, "toDelete": []})
                } else {
                    json!({})
                };
                let bytes = serde_json::to_vec(&json!({"success": true, "data": data})).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        Self {
            address,
            requests,
            stopped,
            worker: Some(worker),
        }
    }

    fn bodies(&self, method: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(verb, _)| verb == method)
            .map(|(_, body)| body.clone())
            .collect()
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

struct Checkout {
    directory: tempfile::TempDir,
    root: PathBuf,
    global: PathBuf,
    system: PathBuf,
    config: PathBuf,
}

impl Checkout {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir(&root).unwrap();
        let config = directory.path().join("xdg");
        fs::create_dir_all(config.join("semctl")).unwrap();
        fs::write(config.join("semctl/installation-id"), "a".repeat(64)).unwrap();
        let global = directory.path().join("global.config");
        let system = directory.path().join("system.config");
        fs::write(&global, "").unwrap();
        fs::write(&system, "").unwrap();
        Self {
            directory,
            root,
            global,
            system,
            config,
        }
    }

    fn command(&self, executable: &str) -> Command {
        let mut command = Command::new(executable);
        command
            .current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", &self.global)
            .env("GIT_CONFIG_SYSTEM", &self.system)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("SEMCTX_TOKEN", "offline-fixture-token")
            .env("NO_PROXY", "*");
        for name in [
            "GIT_CONFIG",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_CEILING_DIRECTORIES",
            "SEMCTX_CODEBASE",
            "SEMCTX_SERVER",
            "SEMCTX_TENANT",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn git(&self, arguments: &[&str]) {
        let result = self.command("git").args(arguments).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    fn run(&self, server: &Server, root: &Path) -> Output {
        self.command(env!("CARGO_BIN_EXE_semctl"))
            .args([
                "--server",
                &format!("http://{}", server.address),
                "--tenant",
                "test",
                "--codebase",
                "A",
                "index",
            ])
            .arg(root)
            .arg("--no-wait")
            .output()
            .unwrap()
    }

    fn success(&self, server: &Server) {
        let result = self.run(server, &self.root);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

fn restore_mtime(path: &Path, modified: std::time::SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
}

#[test]
fn manifests_hash_current_bytes_after_timestamp_preserving_edits() {
    for replace in [false, true] {
        let checkout = Checkout::new();
        let file = checkout.root.join("main.rs");
        fs::write(&file, "fn original() {}\n").unwrap();
        let server = Server::start(|| {});
        checkout.success(&server);
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        if replace {
            let replacement = checkout.root.join("replacement");
            fs::write(&replacement, "fn modified() {}\n").unwrap();
            restore_mtime(&replacement, modified);
            fs::remove_file(&file).unwrap();
            fs::rename(replacement, &file).unwrap();
        } else {
            fs::write(&file, "fn modified() {}\n").unwrap();
            restore_mtime(&file, modified);
        }
        checkout.success(&server);
        let manifests = server.bodies("POST");
        assert_ne!(
            manifests[0]["files"][0]["hash"],
            manifests[1]["files"][0]["hash"]
        );
        assert_eq!(
            server.bodies("PUT")[1]["files"][0]["content"],
            "fn modified() {}\n"
        );
    }
}

#[test]
fn cached_exclusions_require_the_current_content_hash() {
    for (before, after) in [
        (
            b"// @generated\nfn old() {}\n".as_slice(),
            b"// maintained\nfn new() {}\n".as_slice(),
        ),
        (
            b"\xffn main() {}\n".as_slice(),
            b"fn main() {}\n".as_slice(),
        ),
    ] {
        assert_eq!(before.len(), after.len());
        let checkout = Checkout::new();
        let file = checkout.root.join("main.rs");
        fs::write(&file, before).unwrap();
        let server = Server::start(|| {});
        checkout.success(&server);
        assert!(
            server.bodies("POST")[0]["files"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        fs::write(&file, after).unwrap();
        restore_mtime(&file, modified);
        checkout.success(&server);
        assert_eq!(server.bodies("POST")[1]["files"][0]["path"], "main.rs");
    }
}

#[test]
fn manifests_apply_inherited_global_included_and_repository_rules() {
    let checkout = Checkout::new();
    checkout.git(&["init", "--quiet"]);
    let excluded = checkout.directory.path().join("global excludes");
    fs::write(&excluded, "global-private.txt\nallowed.txt\n").unwrap();
    let included = checkout.directory.path().join("included.config");
    fs::write(
        &included,
        format!(
            "[core]\nexcludesFile = \"{}\"\n",
            excluded.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    fs::write(&checkout.global, "[include]\npath = included.config\n").unwrap();
    fs::write(
        checkout.directory.path().join(".ignore"),
        "inherited-private.txt\n",
    )
    .unwrap();
    fs::write(
        checkout.root.join(".git/info/exclude"),
        "repository-private.txt\n",
    )
    .unwrap();
    fs::write(checkout.root.join(".semctlignore"), "!allowed.txt\n").unwrap();
    for name in [
        "global-private.txt",
        "inherited-private.txt",
        "repository-private.txt",
        "allowed.txt",
    ] {
        fs::write(checkout.root.join(name), "fixture content\n").unwrap();
    }
    let server = Server::start(|| {});
    checkout.success(&server);
    let manifests = server.bodies("POST");
    let files = manifests[0]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["path"], "allowed.txt");
}

#[test]
fn malformed_config_or_ignore_rules_abort_before_manifest_submission() {
    for config in [false, true] {
        let checkout = Checkout::new();
        let path = if config {
            checkout.global.clone()
        } else {
            checkout.root.join(".semctlignore")
        };
        fs::write(
            path,
            if config {
                "[unterminated\n"
            } else {
                "{unterminated\n"
            },
        )
        .unwrap();
        fs::write(checkout.root.join("private.txt"), "fixture content\n").unwrap();
        let server = Server::start(|| {});
        assert!(!checkout.run(&server, &checkout.root).status.success());
        assert!(server.bodies("POST").is_empty());
    }
}

#[cfg(unix)]
#[test]
fn unreadable_policy_sources_abort_before_manifest_submission() {
    use std::os::unix::fs::PermissionsExt;
    for source in [
        "global",
        "system",
        "include",
        "excludes",
        "inherited",
        "repository",
        "local-config",
    ] {
        let checkout = Checkout::new();
        checkout.git(&["init", "--quiet"]);
        let included = checkout.directory.path().join("included.config");
        let excludes = checkout.directory.path().join("global.ignore");
        fs::write(
            &included,
            format!("[core]\nexcludesFile = {}\n", excludes.display()),
        )
        .unwrap();
        fs::write(&checkout.global, "[include]\npath = included.config\n").unwrap();
        fs::write(&excludes, "private.txt\n").unwrap();
        let inherited = checkout.directory.path().join(".ignore");
        fs::write(&inherited, "private.txt\n").unwrap();
        let repository = checkout.root.join(".git/info/exclude");
        fs::write(&repository, "private.txt\n").unwrap();
        let local = checkout.root.join(".git/config");
        let denied = match source {
            "global" => &checkout.global,
            "system" => &checkout.system,
            "include" => &included,
            "excludes" => &excludes,
            "inherited" => &inherited,
            "repository" => &repository,
            _ => &local,
        };
        fs::write(checkout.root.join("private.txt"), "fixture content\n").unwrap();
        fs::set_permissions(denied, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(denied).is_ok() {
            fs::set_permissions(denied, fs::Permissions::from_mode(0o600)).unwrap();
            eprintln!("permission assertion skipped: this account can read mode-000 files");
            continue;
        }
        let server = Server::start(|| {});
        let result = checkout.run(&server, &checkout.root);
        fs::set_permissions(denied, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!result.status.success(), "accepted unreadable {source}");
        assert!(
            server.bodies("POST").is_empty(),
            "submitted manifest with unreadable {source}"
        );
    }
}

#[test]
fn linked_worktrees_use_the_common_repository_excludes() {
    let checkout = Checkout::new();
    checkout.git(&["init", "--quiet"]);
    fs::write(checkout.root.join("main.rs"), "fn main() {}\n").unwrap();
    checkout.git(&["add", "main.rs"]);
    checkout.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "fixture",
    ]);
    let linked = checkout.directory.path().join("linked");
    checkout.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        linked.to_str().unwrap(),
    ]);
    fs::write(checkout.root.join(".git/info/exclude"), "private.txt\n").unwrap();
    fs::write(linked.join("private.txt"), "fixture content\n").unwrap();
    let server = Server::start(|| {});
    let result = checkout.run(&server, &linked);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let manifests = server.bodies("POST");
    assert_eq!(manifests[0]["files"].as_array().unwrap().len(), 1);
    assert_eq!(manifests[0]["files"][0]["path"], "main.rs");
}

#[test]
fn policy_changes_during_server_planning_abort_content_upload() {
    let checkout = Checkout::new();
    fs::write(checkout.root.join("private.txt"), "fixture content\n").unwrap();
    let rule = checkout.root.join(".semctlignore");
    let server = Server::start(move || fs::write(&rule, "private.txt\n").unwrap());
    let result = checkout.run(&server, &checkout.root);
    assert!(!result.status.success());
    assert_eq!(server.bodies("POST").len(), 1);
    assert!(server.bodies("PUT").is_empty());
}

#[cfg(unix)]
#[test]
fn linked_worktree_policy_preserves_native_git_directory_paths() {
    use std::os::unix::ffi::OsStringExt;
    let mut checkout = Checkout::new();
    let native = checkout
        .directory
        .path()
        .join(std::ffi::OsString::from_vec(b"repo\xff ".to_vec()));
    fs::rename(&checkout.root, &native).unwrap();
    checkout.root = native;
    checkout.git(&["init", "--quiet"]);
    fs::write(checkout.root.join("main.rs"), "fn main() {}\n").unwrap();
    checkout.git(&["add", "main.rs"]);
    checkout.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "fixture",
    ]);
    let linked = checkout.directory.path().join("linked ");
    let output = checkout
        .command("git")
        .args(["worktree", "add", "--quiet", "--detach"])
        .arg(&linked)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(checkout.root.join(".git/info/exclude"), "private.txt\n").unwrap();
    fs::write(linked.join("private.txt"), "fixture content\n").unwrap();
    let server = Server::start(|| {});
    let result = checkout.run(&server, &linked);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let manifests = server.bodies("POST");
    assert_eq!(manifests[0]["files"].as_array().unwrap().len(), 1);
    assert_eq!(manifests[0]["files"][0]["path"], "main.rs");
}

#[test]
fn an_explicitly_empty_excludes_setting_disables_the_default_file() {
    let checkout = Checkout::new();
    fs::create_dir_all(checkout.config.join("git")).unwrap();
    fs::write(checkout.config.join("git/ignore"), "main.rs\n").unwrap();
    fs::write(&checkout.global, "[core]\nexcludesFile = \n").unwrap();
    fs::write(checkout.root.join("main.rs"), "fn main() {}\n").unwrap();
    let server = Server::start(|| {});
    checkout.success(&server);
    assert_eq!(server.bodies("POST")[0]["files"][0]["path"], "main.rs");
}

#[cfg(unix)]
#[test]
fn null_device_configuration_remains_supported() {
    let mut checkout = Checkout::new();
    checkout.global = PathBuf::from("/dev/null");
    checkout.system = PathBuf::from("/dev/null");
    fs::write(checkout.root.join("main.rs"), "fn main() {}\n").unwrap();
    let server = Server::start(|| {});
    checkout.success(&server);
    assert_eq!(server.bodies("POST")[0]["files"][0]["path"], "main.rs");
}

#[test]
fn policy_scratch_files_inside_the_checkout_never_enter_a_whitelisted_manifest() {
    let checkout = Checkout::new();
    fs::write(checkout.root.join(".semctlignore"), "!*\n").unwrap();
    fs::write(checkout.root.join("main.rs"), "fn main() {}\n").unwrap();
    // Another process can leave private output visible while this scan runs.
    fs::write(
        checkout.root.join(".semctl-000000123abc-42.tmp"),
        "private config fixture\n",
    )
    .unwrap();
    let server = Server::start(|| {});
    let result = checkout
        .command(env!("CARGO_BIN_EXE_semctl"))
        .env("TMPDIR", &checkout.root)
        .env("TMP", &checkout.root)
        .env("TEMP", &checkout.root)
        .args([
            "--server",
            &format!("http://{}", server.address),
            "--tenant",
            "test",
            "--codebase",
            "A",
            "index",
        ])
        .arg(&checkout.root)
        .arg("--no-wait")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for manifest in server.bodies("POST") {
        let paths: Vec<_> = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, [".semctlignore", "main.rs"]);
    }
    for upload in server.bodies("PUT") {
        assert!(upload["files"].as_array().unwrap().iter().all(|entry| {
            !entry["content"]
                .as_str()
                .unwrap()
                .contains("private config fixture")
        }));
    }
}
