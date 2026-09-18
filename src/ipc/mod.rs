//! The local endpoint a semctl client uses to reach the shared daemon.
//!
//! One daemon serves one operating-system user and one configuration
//! directory. The endpoint name carries a hash of both, plus the build
//! version, so a mixed-version fleet stays safe by construction: an old client
//! keeps attaching to the old daemon, and a new client starts a new one.
//!
//! The module holds three parts. [`Endpoint`] names the endpoint and owns the
//! platform paths. [`Listener`] is the daemon side. [`run_client`] is the
//! client side: it builds its own current-thread Tokio runtime, attaches, and
//! pumps bytes.
//!
//! Both sides are asynchronous. A Windows named pipe must be opened for
//! overlapped input and output, because Windows serializes the operations on
//! one file object opened for synchronous input and output. Two blocking
//! threads on one pipe would deadlock: the read of the answer would hold back
//! the write of the request. Tokio's named pipe types already open the pipe
//! for overlapped operation, so the client uses them instead of hand-written
//! overlapped code.
//!
//! Platform code stays in the `unix` and `windows` child modules. No
//! platform type is visible above this module.

pub(crate) mod handshake;
pub(crate) mod pump;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use self::handshake::{Request, Response, SessionRequest};

/// The build version that takes part in the endpoint identity.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How many hexadecimal characters of the digest name the endpoint.
const IDENTITY_HEX_CHARS: usize = 16;

/// Stable endpoint identity for one configuration directory, build version,
/// and operating-system user.
///
/// The three inputs are joined with a zero byte, so no two different input
/// triples can produce the same hashed bytes. A path cannot contain a zero
/// byte on any supported platform.
///
/// The function is pure. It performs no input or output and reads no
/// environment value.
pub(crate) fn identity(config_dir: &Path, version: &str, platform_user: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(config_dir.as_os_str().as_encoded_bytes());
    hasher.update(&[0]);
    hasher.update(version.as_bytes());
    hasher.update(&[0]);
    hasher.update(platform_user.as_bytes());
    let mut hex = hasher.finalize().to_hex().to_string();
    hex.truncate(IDENTITY_HEX_CHARS);
    hex
}

/// The paths and names of one local daemon endpoint.
#[derive(Debug, Clone)]
pub(crate) struct Endpoint {
    /// The hashed identity that names every file and object of this endpoint.
    id: String,
    /// The daemon log file.
    log_path: PathBuf,
    /// The private per-user directory that holds the socket and the lock.
    #[cfg(unix)]
    runtime_dir: PathBuf,
    /// The Unix domain socket the daemon binds.
    #[cfg(unix)]
    socket_path: PathBuf,
    /// The election lock file.
    #[cfg(unix)]
    lock_path: PathBuf,
    /// The effective user id that owns the endpoint.
    #[cfg(unix)]
    uid: u32,
    /// The named pipe the daemon creates.
    #[cfg(windows)]
    pipe_name: String,
    /// The security identifier string of the user that owns the endpoint.
    #[cfg(windows)]
    user_sid: String,
}

impl Endpoint {
    /// The endpoint for this process: its configuration directory, its build
    /// version, and the user it runs as.
    #[cfg(unix)]
    pub(crate) fn current() -> Result<Self> {
        let config_dir = config_directory()?;
        let uid = unix::current_uid();
        let id = identity(&config_dir, VERSION, &uid.to_string());
        let runtime_dir = unix::runtime_dir(&id, uid);
        Ok(Self::in_runtime_dir(runtime_dir, id, uid))
    }

    /// The endpoint for this process: its configuration directory, its build
    /// version, and the user it runs as.
    #[cfg(windows)]
    pub(crate) fn current() -> Result<Self> {
        let config_dir = config_directory()?;
        let user_sid = windows::current_user_sid()?;
        let id = identity(&config_dir, VERSION, &user_sid);
        let log_path = windows::log_path(&id)?;
        Ok(Self {
            pipe_name: windows::pipe_name(&id),
            id,
            log_path,
            user_sid,
        })
    }

    /// Build a Unix endpoint under an explicit runtime directory.
    ///
    /// [`Endpoint::current`] uses this after it selects the directory. A test
    /// uses it to place an endpoint in a temporary directory.
    #[cfg(unix)]
    pub(crate) fn in_runtime_dir(runtime_dir: PathBuf, id: String, uid: u32) -> Self {
        Self {
            socket_path: unix::socket_path(&runtime_dir, &id),
            lock_path: unix::lock_path(&runtime_dir, &id),
            log_path: unix::log_path(&runtime_dir, &id),
            runtime_dir,
            id,
            uid,
        }
    }

    /// The hashed identity that names this endpoint.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// The daemon log file.
    pub(crate) fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Open the daemon log file for appending, creating what is missing.
    ///
    /// A client calls this before it starts a daemon: the daemon's standard
    /// error is that log. Unix creates the file owner-only, inside the
    /// verified private runtime directory. Windows creates the parent
    /// directory under the local application data directory.
    pub(crate) fn open_log(&self) -> Result<std::fs::File> {
        #[cfg(unix)]
        let file = unix::open_log(self)?;
        #[cfg(windows)]
        let file = windows::open_log(self)?;
        Ok(file)
    }

    /// The working directory for a daemon started for this endpoint.
    ///
    /// It is a directory of the endpoint itself, never a checkout: a daemon
    /// holds its working directory open for its whole life and serves sessions
    /// invoked from many directories. [`Self::open_log`] creates it, so a
    /// caller that opened the log may use this path.
    ///
    /// Unix uses the runtime directory, which holds the socket, the lock, and
    /// the log. Windows uses the log's parent directory; `None` means the path
    /// names no parent, and the daemon then keeps this client's directory.
    pub(crate) fn daemon_dir(&self) -> Option<&Path> {
        #[cfg(unix)]
        let dir = Some(self.runtime_dir());
        #[cfg(windows)]
        let dir = self.log_path.parent();
        dir
    }

    /// The private directory that holds the socket and the lock.
    #[cfg(unix)]
    pub(crate) fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    /// The Unix domain socket path.
    #[cfg(unix)]
    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The election lock path.
    #[cfg(unix)]
    pub(crate) fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// The effective user id that owns this endpoint.
    #[cfg(unix)]
    pub(crate) fn uid(&self) -> u32 {
        self.uid
    }

    /// The named pipe of this endpoint.
    #[cfg(windows)]
    pub(crate) fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// The security identifier string of the user that owns this endpoint.
    #[cfg(windows)]
    pub(crate) fn user_sid(&self) -> &str {
        &self.user_sid
    }
}

/// The configuration directory, exactly as it is configured.
///
/// The path is not resolved. Resolving it would make the identity depend on
/// whether the directory already exists: a directory under a symbolic link
/// hashes one way before it is created and another way afterwards, and the
/// client that created it would then start a second daemon for the same
/// configuration. Every process of one user reads the same configured path,
/// which is what the identity needs.
fn config_directory() -> Result<PathBuf> {
    crate::config::config_dir().context("locate the configuration directory")
}

/// The result of the daemon election for one endpoint.
pub(crate) enum Election {
    /// This process owns the endpoint and must serve it.
    Won(Listener),
    /// Another daemon owns the endpoint. This process must exit with status 0.
    Lost,
}

/// The daemon side of the endpoint.
pub(crate) enum Listener {
    /// A bound Unix domain socket.
    #[cfg(unix)]
    Socket(unix::SocketListener),
    /// A pool of listening named pipe instances.
    #[cfg(windows)]
    Pipe(windows::PipeListener),
}

impl Listener {
    /// Run the election and, when this process wins, bind the endpoint.
    ///
    /// A lost election is a normal outcome, not an error: another daemon
    /// already serves this endpoint. Call this inside a Tokio runtime.
    pub(crate) fn bind(endpoint: &Endpoint) -> Result<Election> {
        #[cfg(unix)]
        let election = unix::bind(endpoint)?;
        #[cfg(windows)]
        let election = windows::bind(endpoint)?;
        Ok(election)
    }

    /// Accept the next connection from a peer that runs as the endpoint owner.
    pub(crate) async fn accept(&self) -> Result<Stream> {
        match self {
            #[cfg(unix)]
            Self::Socket(listener) => Ok(Stream::Socket(listener.accept().await?)),
            #[cfg(windows)]
            Self::Pipe(listener) => Ok(Stream::PipeServer(listener.accept().await?)),
        }
    }
}

/// One connection over the local endpoint.
///
/// A Unix domain socket has one type for both ends. A named pipe has one type
/// for the daemon side and another for the client side, so this enum carries
/// both.
#[derive(Debug)]
pub(crate) enum Stream {
    /// A Unix domain socket connection, on either side.
    #[cfg(unix)]
    Socket(tokio::net::UnixStream),
    /// A connected named pipe instance, on the daemon side.
    #[cfg(windows)]
    PipeServer(tokio::net::windows::named_pipe::NamedPipeServer),
    /// An open named pipe, on the client side.
    #[cfg(windows)]
    PipeClient(tokio::net::windows::named_pipe::NamedPipeClient),
}

impl Stream {
    /// Whether this transport can end its outbound direction on its own.
    ///
    /// A Unix domain socket shuts down its write direction and stays
    /// readable. A named pipe has no half-close: `poll_shutdown` only flushes.
    pub(crate) fn half_close(&self) -> pump::HalfClose {
        match self {
            #[cfg(unix)]
            Self::Socket(_) => pump::HalfClose::Supported,
            #[cfg(windows)]
            Self::PipeServer(_) | Self::PipeClient(_) => pump::HalfClose::Unsupported,
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => Pin::new(stream).poll_read(context, buffer),
            #[cfg(windows)]
            Self::PipeServer(stream) => Pin::new(stream).poll_read(context, buffer),
            #[cfg(windows)]
            Self::PipeClient(stream) => Pin::new(stream).poll_read(context, buffer),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => Pin::new(stream).poll_write(context, bytes),
            #[cfg(windows)]
            Self::PipeServer(stream) => Pin::new(stream).poll_write(context, bytes),
            #[cfg(windows)]
            Self::PipeClient(stream) => Pin::new(stream).poll_write(context, bytes),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => Pin::new(stream).poll_flush(context),
            #[cfg(windows)]
            Self::PipeServer(stream) => Pin::new(stream).poll_flush(context),
            #[cfg(windows)]
            Self::PipeClient(stream) => Pin::new(stream).poll_flush(context),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => Pin::new(stream).poll_shutdown(context),
            #[cfg(windows)]
            Self::PipeServer(stream) => Pin::new(stream).poll_shutdown(context),
            #[cfg(windows)]
            Self::PipeClient(stream) => Pin::new(stream).poll_shutdown(context),
        }
    }
}

/// Connect to the endpoint.
///
/// The call retries while no daemon listens yet, until `deadline`. A client
/// that spawned a daemon uses the deadline to wait for that daemon to bind.
pub(crate) async fn connect(endpoint: &Endpoint, deadline: Instant) -> Result<Stream> {
    #[cfg(unix)]
    let stream = unix::connect(endpoint, deadline).await?;
    #[cfg(windows)]
    let stream = windows::connect(endpoint, deadline).await?;
    Ok(stream)
}

/// Attach to the daemon and copy bytes until the connection ends.
///
/// This is the transport half of the client role. It builds its own
/// current-thread Tokio runtime, because the role decision happens before any
/// runtime exists, and one connection with two directions needs no worker
/// pool.
///
/// `connect` is the caller's connection strategy, run inside that runtime. The
/// client role uses it to try a daemon that is already listening, start one
/// when none is, and try again. Keeping it here rather than in this module
/// leaves the endpoint free of any knowledge about starting processes.
///
/// An `Err` means no session was served: the connection, the handshake, or the
/// daemon's answer failed, and the caller may still serve the session another
/// way. An `Ok` means the pump ran, and the exit belongs to the host.
pub(crate) fn run_client<C, F>(session: SessionRequest, connect: C) -> Result<pump::Exit>
where
    C: Fn() -> F,
    F: Future<Output = Result<Stream>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("build the client runtime")?;
    let outcome = runtime.block_on(attach_and_pump(session, connect));
    // The outbound task can still be waiting for standard input. Dropping the
    // runtime would wait for that read, so shut it down in the background and
    // let process exit release the handles.
    runtime.shutdown_background();
    outcome
}

/// Perform the attach handshake, then pump bytes between the standard streams
/// and the connection.
///
/// The handshake is retried once, and only when the connection closed before
/// any answer arrived. A daemon that drained while this connection waited in
/// its backlog produces exactly that, and the retry starts another daemon and
/// attaches to it. A refusal is an answer and is never retried, and nothing is
/// retried once the pump owns the standard streams.
async fn attach_and_pump<C, F>(session: SessionRequest, connect: C) -> Result<pump::Exit>
where
    C: Fn() -> F,
    F: Future<Output = Result<Stream>>,
{
    let stream = match attach(&session, &connect).await? {
        Some(stream) => stream,
        None => attach(&session, &connect).await?.ok_or_else(|| {
            anyhow!("the daemon closed the connection before it answered the attach request")
        })?,
    };
    let half_close = stream.half_close();
    Ok(pump::run(stream, half_close, tokio::io::stdin(), tokio::io::stdout()).await)
}

/// One attach attempt.
///
/// `Ok(None)` means the daemon went away before it answered, which the caller
/// may retry. Every other failure is final.
async fn attach<C, F>(session: &SessionRequest, connect: &C) -> Result<Option<Stream>>
where
    C: Fn() -> F,
    F: Future<Output = Result<Stream>>,
{
    let mut stream = connect().await?;
    let request = Request::attach(VERSION, session.clone());
    if let Err(error) = handshake::write_line_async(&mut stream, &request).await {
        if connection_lost(&error) {
            return Ok(None);
        }
        return Err(error).context("send the attach request");
    }
    let line = match handshake::read_line_async(&mut stream).await {
        Ok(line) => line,
        Err(error) if connection_lost(&error) => return Ok(None),
        Err(error) => return Err(error).context("read the attach answer"),
    };
    accept_attach(handshake::decode_response(&line).context("decode the attach answer")?)?;
    Ok(Some(stream))
}

/// Whether the daemon went away before it answered.
fn connection_lost(error: &handshake::HandshakeError) -> bool {
    match error {
        handshake::HandshakeError::Closed => true,
        handshake::HandshakeError::Transport(error) => matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}

/// Decide whether the daemon's answer opens a session on this connection.
///
/// The version is checked as well as the answer kind. The endpoint identity
/// already carries the version, so a daemon of another build reached this
/// client another way, and one build's client cannot be served by another
/// build's tool surface.
fn accept_attach(response: Response) -> Result<()> {
    match response {
        Response::Attached { version, .. } if version != VERSION => Err(anyhow!(
            "the daemon is semctl {version}; this client is semctl {VERSION}"
        )),
        Response::Attached { .. } => Ok(()),
        Response::Rejected { reason, .. } => {
            Err(anyhow!("the daemon refused the session: {reason}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Endpoint, IDENTITY_HEX_CHARS, Response, Stream, VERSION, accept_attach, config_directory,
        identity,
    };
    use std::path::Path;
    use tokio::io::{AsyncRead, AsyncWrite};

    /// The daemon serves each session on its own task, and the client splits
    /// one connection into two pump tasks. Both need these bounds.
    #[test]
    fn the_stream_type_satisfies_the_transport_bounds() {
        fn accepts_transport<T: AsyncRead + AsyncWrite + Unpin + Send>() {}
        accepts_transport::<Stream>();
    }

    #[test]
    fn identity_is_stable_for_equal_inputs() {
        let first = identity(Path::new("/home/a/.config/semctl"), "0.2.0", "1000");
        let second = identity(Path::new("/home/a/.config/semctl"), "0.2.0", "1000");
        assert_eq!(first, second);
        assert_eq!(first.len(), IDENTITY_HEX_CHARS);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()), "{first}");
    }

    #[test]
    fn identity_changes_with_the_configuration_directory() {
        let first = identity(Path::new("/home/a/.config/semctl"), "0.2.0", "1000");
        let second = identity(Path::new("/home/b/.config/semctl"), "0.2.0", "1000");
        assert_ne!(first, second);
    }

    #[test]
    fn identity_changes_with_the_version() {
        let first = identity(Path::new("/home/a/.config/semctl"), "0.2.0", "1000");
        let second = identity(Path::new("/home/a/.config/semctl"), "0.2.1", "1000");
        assert_ne!(first, second);
    }

    #[test]
    fn identity_changes_with_the_platform_user() {
        let first = identity(Path::new("/home/a/.config/semctl"), "0.2.0", "1000");
        let second = identity(Path::new("/home/a/.config/semctl"), "0.2.0", "1001");
        assert_ne!(first, second);
    }

    /// The zero byte between the inputs keeps two different triples apart even
    /// when their concatenations are equal.
    #[test]
    fn identity_separates_the_three_inputs() {
        let first = identity(Path::new("/a"), "b", "c");
        let second = identity(Path::new("/a"), "bc", "");
        assert_ne!(first, second);
    }

    #[test]
    fn the_endpoint_identity_uses_this_build_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[cfg(unix)]
    #[test]
    fn a_unix_endpoint_names_the_socket_lock_and_log_in_one_directory() {
        let endpoint = Endpoint::in_runtime_dir(
            std::path::PathBuf::from("/run/user/1000/semctl"),
            "0123456789abcdef".to_string(),
            1000,
        );
        assert_eq!(endpoint.id(), "0123456789abcdef");
        assert_eq!(
            endpoint.socket_path(),
            Path::new("/run/user/1000/semctl/0123456789abcdef.sock")
        );
        assert_eq!(
            endpoint.lock_path(),
            Path::new("/run/user/1000/semctl/0123456789abcdef.lock")
        );
        assert_eq!(
            endpoint.log_path(),
            Path::new("/run/user/1000/semctl/0123456789abcdef.log")
        );
        assert_eq!(endpoint.runtime_dir(), Path::new("/run/user/1000/semctl"));
        assert_eq!(endpoint.uid(), 1000);
    }

    #[test]
    fn the_current_endpoint_is_built_from_this_process() {
        let endpoint = Endpoint::current().expect("build the endpoint for this process");
        assert_eq!(endpoint.id().len(), IDENTITY_HEX_CHARS);
    }

    /// The configuration directory is hashed as it is configured. Resolving it
    /// would give one identity before the directory exists and another
    /// afterwards, so one user would end up with two daemons.
    #[test]
    fn the_endpoint_identity_uses_the_configured_path_unresolved() {
        let configured = config_directory().expect("locate the configuration directory");
        assert_eq!(
            configured,
            crate::config::config_dir().expect("the configured path")
        );
    }

    #[test]
    fn a_session_opens_only_on_an_answer_from_this_build() {
        accept_attach(Response::attached(VERSION, "1-1")).expect("this build's daemon serves");
    }

    /// The endpoint identity carries the version, so an answer from another
    /// build means this client reached that daemon another way.
    #[test]
    fn an_attached_answer_from_another_build_is_refused() {
        let error = accept_attach(Response::attached("0.0.1", "1-1"))
            .expect_err("another build cannot serve this client");
        assert!(error.to_string().contains("0.0.1"), "{error}");
        assert!(error.to_string().contains(VERSION), "{error}");
    }

    /// A daemon that drained while this connection waited in its backlog
    /// closes it with no answer. That is the one failure the client retries.
    #[test]
    fn only_a_connection_that_closed_before_an_answer_is_retried() {
        use super::connection_lost;
        use crate::ipc::handshake::HandshakeError;
        use std::io;

        assert!(connection_lost(&HandshakeError::Closed));
        for kind in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::UnexpectedEof,
        ] {
            assert!(
                connection_lost(&HandshakeError::Transport(io::Error::from(kind))),
                "{kind:?}"
            );
        }
        assert!(!connection_lost(&HandshakeError::TimedOut));
        assert!(!connection_lost(&HandshakeError::LineTooLong));
        assert!(!connection_lost(&HandshakeError::Malformed("no".into())));
        assert!(!connection_lost(&HandshakeError::Transport(
            io::Error::from(io::ErrorKind::PermissionDenied)
        )));
    }

    #[test]
    fn a_refusal_reports_the_reason_the_daemon_gave() {
        let error = accept_attach(Response::rejected(VERSION, "no session for you"))
            .expect_err("a refusal opens no session");
        assert!(error.to_string().contains("no session for you"), "{error}");
    }
}
