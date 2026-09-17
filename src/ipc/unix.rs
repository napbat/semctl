//! Unix domain socket endpoint for the shared local daemon.
//!
//! The endpoint lives in a private per-user runtime directory. The daemon
//! elects itself with a lock file next to the socket, so two daemons can never
//! serve one endpoint. Every accepted peer must run under the same user id.

use std::fs::{self, DirBuilder, File, Permissions, TryLockError};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use super::{Election, Endpoint, Listener, Stream};

/// Largest socket path this module will use.
///
/// The `sun_path` field of a Unix socket address holds 108 bytes on Linux and
/// 104 bytes on macOS. The design keeps a margin and stops at 100 bytes.
const MAX_SOCKET_PATH_BYTES: usize = 100;

/// First wait between connection attempts.
const FIRST_BACKOFF: Duration = Duration::from_millis(10);

/// Longest wait between connection attempts.
const LAST_BACKOFF: Duration = Duration::from_millis(200);

/// The effective user id of this process.
///
/// The crate has no `libc` dependency, so the symbol is declared here.
/// `geteuid` is the right identity to compare against: a file this process
/// creates is owned by the effective user id, and both `SO_PEERCRED` on Linux
/// and `getpeereid` on macOS report a peer's effective user id.
pub(super) fn current_uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: `geteuid` takes no argument and cannot fail. Every Unix target
    // this crate supports defines `uid_t` as a 32-bit unsigned integer.
    unsafe { geteuid() }
}

/// The socket file for one endpoint identity.
pub(super) fn socket_path(runtime_dir: &Path, id: &str) -> PathBuf {
    runtime_dir.join(format!("{id}.sock"))
}

/// The election lock file for one endpoint identity.
pub(super) fn lock_path(runtime_dir: &Path, id: &str) -> PathBuf {
    runtime_dir.join(format!("{id}.lock"))
}

/// The daemon log file for one endpoint identity.
pub(super) fn log_path(runtime_dir: &Path, id: &str) -> PathBuf {
    runtime_dir.join(format!("{id}.log"))
}

/// The runtime directory this process uses for one endpoint identity.
pub(super) fn runtime_dir(id: &str, uid: u32) -> PathBuf {
    select_runtime_dir(preferred_runtime_dir(), id, uid)
}

/// Choose between the preferred directory and the `/tmp` fallback.
///
/// The fallback also applies when the preferred directory would make the
/// socket path longer than [`MAX_SOCKET_PATH_BYTES`].
fn select_runtime_dir(preferred: Option<PathBuf>, id: &str, uid: u32) -> PathBuf {
    match preferred {
        Some(dir)
            if socket_path(&dir, id).as_os_str().as_bytes().len() <= MAX_SOCKET_PATH_BYTES =>
        {
            dir
        }
        _ => PathBuf::from(format!("/tmp/semctl-{uid}")),
    }
}

/// The platform's preferred runtime directory, when the platform names one.
#[cfg(target_os = "linux")]
fn preferred_runtime_dir() -> Option<PathBuf> {
    non_empty_var("XDG_RUNTIME_DIR").map(|base| PathBuf::from(base).join("semctl"))
}

/// The platform's preferred runtime directory, when the platform names one.
#[cfg(target_os = "macos")]
fn preferred_runtime_dir() -> Option<PathBuf> {
    non_empty_var("TMPDIR").map(|base| PathBuf::from(base).join("semctl"))
}

/// The platform's preferred runtime directory, when the platform names one.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn preferred_runtime_dir() -> Option<PathBuf> {
    None
}

/// Read an environment variable, treating an empty value as unset.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn non_empty_var(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// Create the runtime directory when it is missing, then verify it.
pub(super) fn ensure_runtime_dir(dir: &Path, uid: u32) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_private_dir(dir)?,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect runtime directory {}", dir.display()));
        }
    }
    verify_runtime_dir(dir, uid)
}

/// Create an owner-only directory, including any missing parent.
fn create_private_dir(dir: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("create runtime directory {}", dir.display()))?;
    // `mkdir` masks the requested mode with the process umask, so a strict
    // umask would drop the owner execute bit. Set the mode again.
    fs::set_permissions(dir, Permissions::from_mode(0o700))
        .with_context(|| format!("set mode 0700 on {}", dir.display()))
}

/// Fail unless the directory is a real directory owned by `uid` with no group
/// or other permission bits.
///
/// The check uses `symlink_metadata`, so a symbolic link in place of the
/// directory is refused rather than followed.
fn verify_runtime_dir(dir: &Path, uid: u32) -> Result<()> {
    let metadata = fs::symlink_metadata(dir)
        .with_context(|| format!("inspect runtime directory {}", dir.display()))?;
    ensure!(
        metadata.is_dir(),
        "runtime path {} is not a directory",
        dir.display()
    );
    ensure!(
        metadata.uid() == uid,
        "runtime directory {} is owned by uid {}, not uid {uid}",
        dir.display(),
        metadata.uid()
    );
    let mode = metadata.mode() & 0o777;
    let shared_bits = mode & 0o077;
    ensure!(
        shared_bits == 0,
        "runtime directory {} allows group or other access (mode {mode:04o})",
        dir.display()
    );
    Ok(())
}

/// A bound Unix domain socket and the election lock that protects it.
pub(crate) struct SocketListener {
    listener: tokio::net::UnixListener,
    socket_path: PathBuf,
    uid: u32,
    /// The election lock. It is held for the lifetime of this listener.
    _lock: File,
}

impl SocketListener {
    /// Accept the next connection from a peer with the expected user id.
    ///
    /// A peer with another user id is closed before any byte is read.
    pub(super) async fn accept(&self) -> Result<tokio::net::UnixStream> {
        loop {
            let (stream, _address) = self
                .listener
                .accept()
                .await
                .with_context(|| format!("accept on {}", self.socket_path.display()))?;
            match stream.peer_cred() {
                Ok(peer) if peer.uid() == self.uid => return Ok(stream),
                Ok(peer) => {
                    tracing::warn!(
                        peer_uid = peer.uid(),
                        "refused a local connection from another user"
                    );
                    drop(stream);
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "refused a local connection with unreadable peer credentials"
                    );
                    drop(stream);
                }
            }
        }
    }
}

impl Drop for SocketListener {
    fn drop(&mut self) {
        // Best effort. The election lock is still held here, so no other
        // daemon can own the path. A file left behind is removed as a stale
        // socket by the next daemon that wins the election.
        let _ = fs::remove_file(&self.socket_path);
    }
}

/// Elect this process and bind the endpoint socket.
///
/// Call this inside a Tokio runtime: the listener registers with the reactor.
pub(super) fn bind(endpoint: &Endpoint) -> Result<Election> {
    ensure_runtime_dir(endpoint.runtime_dir(), endpoint.uid())?;
    let lock = crate::config::open_lock(endpoint.lock_path())?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(Election::Lost),
        Err(TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("lock {}", endpoint.lock_path().display()));
        }
    }
    // The lock is held from here, so no other daemon owns this endpoint. A
    // socket file at the path is left over from a dead daemon and must go
    // before the bind. Removing it before the lock would break a live daemon.
    remove_stale_socket(endpoint.socket_path())?;
    let listener = tokio::net::UnixListener::bind(endpoint.socket_path())
        .with_context(|| format!("bind {}", endpoint.socket_path().display()))?;
    // The runtime directory already denies other users. An owner-only socket
    // keeps the endpoint private even if that directory mode later changes.
    fs::set_permissions(endpoint.socket_path(), Permissions::from_mode(0o600))
        .with_context(|| format!("set mode 0600 on {}", endpoint.socket_path().display()))?;
    Ok(Election::Won(Listener::Socket(SocketListener {
        listener,
        socket_path: endpoint.socket_path().to_path_buf(),
        uid: endpoint.uid(),
        _lock: lock,
    })))
}

/// Remove a socket file left by a dead daemon.
fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove stale socket {}", path.display())),
    }
}

/// Connect to the endpoint.
///
/// The call retries a missing or refused socket until `deadline`, because a
/// daemon this client just spawned needs a moment to bind.
pub(super) async fn connect(endpoint: &Endpoint, deadline: Instant) -> Result<Stream> {
    let path = endpoint.socket_path();
    let mut backoff = FIRST_BACKOFF;
    loop {
        match tokio::net::UnixStream::connect(path).await {
            Ok(stream) => return Ok(Stream::Socket(stream)),
            Err(error) if is_absent(&error) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(error).with_context(|| {
                        format!("connect {} before the deadline", path.display())
                    });
                }
                tokio::time::sleep(backoff.min(remaining)).await;
                backoff = (backoff * 2).min(LAST_BACKOFF);
            }
            Err(error) => {
                return Err(error).with_context(|| format!("connect {}", path.display()));
            }
        }
    }
}

/// Whether the failure means no daemon is listening yet.
fn is_absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Election, Endpoint, MAX_SOCKET_PATH_BYTES, bind, current_uid, ensure_runtime_dir,
        select_runtime_dir, socket_path, verify_runtime_dir,
    };
    use crate::ipc::handshake::{
        self, Request, Response, SessionRequest, Token, decode_request, decode_response,
    };
    use crate::ipc::pump;
    use std::fs::{self, Permissions};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TEST_ID: &str = "0123456789abcdef";

    fn endpoint_in(dir: &Path) -> Endpoint {
        // A temporary directory honors the process umask, which can leave
        // group bits. A runtime directory must be owner-only.
        fs::set_permissions(dir, Permissions::from_mode(0o700)).expect("set mode 0700");
        Endpoint::in_runtime_dir(dir.to_path_buf(), TEST_ID.to_string(), current_uid())
    }

    fn attach_request() -> Request {
        Request::attach(
            "0.2.0",
            SessionRequest {
                cwd: PathBuf::from("/abs/path"),
                server: None,
                tenant: None,
                codebase: None,
                token: Some(Token::new("super-secret-value")),
                resync_secs: None,
                update_check: true,
            },
        )
    }

    #[test]
    fn a_private_directory_passes_validation() {
        let dir = tempfile::tempdir().expect("temporary directory");
        fs::set_permissions(dir.path(), Permissions::from_mode(0o700)).expect("set mode 0700");
        verify_runtime_dir(dir.path(), current_uid()).expect("a private directory is accepted");
    }

    #[test]
    fn a_directory_with_group_bits_is_refused() {
        let dir = tempfile::tempdir().expect("temporary directory");
        fs::set_permissions(dir.path(), Permissions::from_mode(0o750)).expect("set mode 0750");
        let error = verify_runtime_dir(dir.path(), current_uid()).expect_err("group access");
        assert!(
            error.to_string().contains("group or other access"),
            "{error}"
        );
    }

    #[test]
    fn a_directory_with_other_bits_is_refused() {
        let dir = tempfile::tempdir().expect("temporary directory");
        fs::set_permissions(dir.path(), Permissions::from_mode(0o701)).expect("set mode 0701");
        let error = verify_runtime_dir(dir.path(), current_uid()).expect_err("other access");
        assert!(
            error.to_string().contains("group or other access"),
            "{error}"
        );
    }

    /// The test cannot change the owner of a directory without privilege, so
    /// it varies the expected user id instead. That exercises the same
    /// comparison the daemon makes.
    #[test]
    fn a_directory_owned_by_another_user_is_refused() {
        let dir = tempfile::tempdir().expect("temporary directory");
        fs::set_permissions(dir.path(), Permissions::from_mode(0o700)).expect("set mode 0700");
        let other = current_uid().wrapping_add(1);
        let error = verify_runtime_dir(dir.path(), other).expect_err("wrong owner");
        assert!(error.to_string().contains("is owned by uid"), "{error}");
    }

    #[test]
    fn a_symbolic_link_in_place_of_the_directory_is_refused() {
        let parent = tempfile::tempdir().expect("temporary directory");
        let target = parent.path().join("target");
        fs::create_dir(&target).expect("create the link target");
        fs::set_permissions(&target, Permissions::from_mode(0o700)).expect("set mode 0700");
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("create a symbolic link");
        let error = verify_runtime_dir(&link, current_uid()).expect_err("symbolic link");
        assert!(error.to_string().contains("is not a directory"), "{error}");
    }

    #[test]
    fn a_missing_runtime_directory_is_created_owner_only() {
        let parent = tempfile::tempdir().expect("temporary directory");
        let dir = parent.path().join("nested").join("semctl");
        ensure_runtime_dir(&dir, current_uid()).expect("create the runtime directory");
        let mode = fs::symlink_metadata(&dir)
            .expect("inspect the runtime directory")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "mode {mode:04o}");
    }

    #[test]
    fn a_short_preferred_directory_is_used() {
        let preferred = PathBuf::from("/run/user/1000/semctl");
        let chosen = select_runtime_dir(Some(preferred.clone()), TEST_ID, 1000);
        assert_eq!(chosen, preferred);
    }

    #[test]
    fn a_long_preferred_directory_falls_back_to_tmp() {
        let preferred = PathBuf::from(format!("/run/user/1000/{}/semctl", "d".repeat(120)));
        assert!(socket_path(&preferred, TEST_ID).as_os_str().len() > MAX_SOCKET_PATH_BYTES);
        let chosen = select_runtime_dir(Some(preferred), TEST_ID, 1000);
        assert_eq!(chosen, PathBuf::from("/tmp/semctl-1000"));
    }

    #[test]
    fn no_preferred_directory_falls_back_to_tmp() {
        let chosen = select_runtime_dir(None, TEST_ID, 1000);
        assert_eq!(chosen, PathBuf::from("/tmp/semctl-1000"));
    }

    #[tokio::test]
    async fn a_second_bind_reports_that_another_daemon_owns_the_endpoint() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let endpoint = endpoint_in(dir.path());
        let first = bind(&endpoint).expect("first bind");
        assert!(matches!(first, Election::Won(_)), "first bind must win");
        let second = bind(&endpoint).expect("second bind");
        assert!(matches!(second, Election::Lost), "second bind must lose");
        drop(first);
    }

    #[tokio::test]
    async fn a_released_endpoint_can_be_bound_again() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let endpoint = endpoint_in(dir.path());
        drop(bind(&endpoint).expect("first bind"));
        let again = bind(&endpoint).expect("second bind");
        assert!(matches!(again, Election::Won(_)), "the endpoint is free");
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_removed_before_the_bind() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let endpoint = endpoint_in(dir.path());
        fs::write(endpoint.socket_path(), b"left by a dead daemon").expect("write a stale file");
        let election = bind(&endpoint).expect("bind over a stale socket");
        assert!(matches!(election, Election::Won(_)), "the bind must win");
        let kind = fs::symlink_metadata(endpoint.socket_path())
            .expect("inspect the socket")
            .file_type();
        assert!(kind.is_socket(), "the path must now be a socket");
    }

    #[tokio::test]
    async fn the_socket_is_owner_only() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let endpoint = endpoint_in(dir.path());
        let election = bind(&endpoint).expect("bind");
        assert!(matches!(election, Election::Won(_)));
        let mode = fs::symlink_metadata(endpoint.socket_path())
            .expect("inspect the socket")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "mode {mode:04o}");
    }

    #[tokio::test]
    async fn a_client_and_a_daemon_exchange_the_handshake() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let endpoint = endpoint_in(dir.path());
        let Election::Won(listener) = bind(&endpoint).expect("bind") else {
            panic!("the first bind must win the election");
        };

        let daemon = tokio::spawn(async move {
            let mut stream = listener.accept().await.expect("accept a connection");
            let line = handshake::read_line_async(&mut stream)
                .await
                .expect("read the attach line");
            let request = decode_request(&line).expect("decode the attach line");
            assert!(matches!(request, Request::Attach { .. }), "{request:?}");
            handshake::write_line_async(&mut stream, &Response::attached("0.2.0", "session-1"))
                .await
                .expect("write the attached line");
            // Hold both ends until the client has read its answer.
            (listener, stream)
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = super::connect(&endpoint, deadline)
            .await
            .expect("connect to the daemon");
        handshake::write_line_async(&mut stream, &attach_request())
            .await
            .expect("write the attach line");
        let line = handshake::read_line_async(&mut stream)
            .await
            .expect("read the answer");
        let response = decode_response(&line).expect("decode the answer");
        assert!(
            matches!(response, Response::Attached { .. }),
            "{response:?}"
        );
        drop(daemon.await.expect("the daemon task finished"));
    }

    /// One connection carries both directions at the same time. A transport
    /// that serializes its operations would deadlock here.
    #[tokio::test]
    async fn the_client_pump_carries_bytes_in_both_directions() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let endpoint = endpoint_in(dir.path());
        let Election::Won(listener) = bind(&endpoint).expect("bind") else {
            panic!("the first bind must win the election");
        };

        let daemon = tokio::spawn(async move {
            let mut stream = listener.accept().await.expect("accept a connection");
            let mut request = [0u8; 5];
            stream
                .read_exact(&mut request)
                .await
                .expect("read the request");
            assert_eq!(&request, b"ping\n");
            stream.write_all(b"pong\n").await.expect("write the answer");
            stream.flush().await.expect("flush the answer");
            (listener, stream)
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = super::connect(&endpoint, deadline)
            .await
            .expect("connect to the daemon");
        let half_close = stream.half_close();
        let (mut host_input, pump_input) = tokio::io::duplex(256);
        let (pump_output, mut host_output) = tokio::io::duplex(256);
        let pump = tokio::spawn(pump::run(stream, half_close, pump_input, pump_output));

        host_input
            .write_all(b"ping\n")
            .await
            .expect("write to the pump");
        let mut answer = [0u8; 5];
        host_output
            .read_exact(&mut answer)
            .await
            .expect("read from the pump");
        assert_eq!(&answer, b"pong\n");

        // End of file on the input half-closes; the daemon then closes its
        // end, which ends the pump cleanly.
        drop(host_input);
        let (listener, daemon_stream) = daemon.await.expect("the daemon task finished");
        drop(daemon_stream);
        assert_eq!(
            pump.await.expect("the pump task finished"),
            pump::Exit::Clean
        );
        drop(listener);
    }

    #[tokio::test]
    async fn a_client_without_a_daemon_fails_at_the_deadline() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let endpoint = endpoint_in(dir.path());
        let deadline = Instant::now() + Duration::from_millis(30);
        let error = super::connect(&endpoint, deadline)
            .await
            .expect_err("no daemon is listening");
        assert!(error.to_string().contains("before the deadline"), "{error}");
    }
}
