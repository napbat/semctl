//! The local endpoint a semctl client uses to reach the shared daemon.
//!
//! One daemon serves one operating-system user and one configuration
//! directory. The endpoint name carries a hash of both, plus the build
//! version, so a mixed-version fleet stays safe by construction: an old client
//! keeps attaching to the old daemon, and a new client starts a new one.
//!
//! The module holds three parts. [`Endpoint`] names the endpoint and owns the
//! platform paths. [`Listener`] is the daemon side and runs on Tokio.
//! [`connect_blocking`] is the client side and uses blocking input and output,
//! because the client role must not build an asynchronous runtime.
//!
//! Platform code stays in the `unix` and `windows` child modules. No
//! platform type is visible above this module.

pub(crate) mod handshake;
pub(crate) mod pump;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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

/// The configuration directory, resolved when it already exists.
///
/// The identity must be the same for one directory reached by two different
/// paths. The directory is missing before the first run, and an unresolvable
/// path is then used as it is.
fn config_directory() -> Result<PathBuf> {
    let dir = crate::config::config_dir().context("locate the configuration directory")?;
    Ok(std::fs::canonicalize(&dir).unwrap_or(dir))
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
            Self::Pipe(listener) => Ok(Stream::Pipe(listener.accept().await?)),
        }
    }
}

/// One accepted connection, seen by the daemon.
pub(crate) enum Stream {
    /// A Unix domain socket connection.
    #[cfg(unix)]
    Socket(tokio::net::UnixStream),
    /// A connected named pipe instance.
    #[cfg(windows)]
    Pipe(tokio::net::windows::named_pipe::NamedPipeServer),
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
            Self::Pipe(stream) => Pin::new(stream).poll_read(context, buffer),
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
            Self::Pipe(stream) => Pin::new(stream).poll_write(context, bytes),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => Pin::new(stream).poll_flush(context),
            #[cfg(windows)]
            Self::Pipe(stream) => Pin::new(stream).poll_flush(context),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Socket(stream) => Pin::new(stream).poll_shutdown(context),
            #[cfg(windows)]
            Self::Pipe(stream) => Pin::new(stream).poll_shutdown(context),
        }
    }
}

/// One connection to the daemon, seen by the client.
///
/// The client role uses blocking input and output. It must not build a Tokio
/// runtime, because the daemon decision happens before any runtime exists.
#[derive(Debug)]
pub(crate) enum BlockingStream {
    /// A Unix domain socket connection.
    #[cfg(unix)]
    Socket(std::os::unix::net::UnixStream),
    /// An open named pipe.
    #[cfg(windows)]
    Pipe(std::fs::File),
}

impl BlockingStream {
    /// A second handle to the same connection, for the other pump thread.
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(unix)]
            Self::Socket(socket) => socket.try_clone().map(Self::Socket),
            #[cfg(windows)]
            Self::Pipe(file) => file.try_clone().map(Self::Pipe),
        }
    }

    /// Limit how long one blocking read or write may take.
    ///
    /// The client sets [`handshake::EXCHANGE_TIMEOUT`] for the handshake and
    /// clears the limit before the byte pump starts.
    #[cfg_attr(
        windows,
        allow(clippy::unnecessary_wraps, reason = "the Unix form can fail")
    )]
    pub(crate) fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Socket(socket) => {
                socket.set_read_timeout(timeout)?;
                socket.set_write_timeout(timeout)
            }
            #[cfg(windows)]
            Self::Pipe(_) => {
                // A named pipe opened for synchronous input and output carries
                // no timeout of its own. The connect deadline bounds the
                // client, and the daemon closes an idle connection.
                let _ = timeout;
                Ok(())
            }
        }
    }
}

impl Read for BlockingStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Socket(socket) => socket.read(buffer),
            #[cfg(windows)]
            Self::Pipe(file) => file.read(buffer),
        }
    }
}

impl Write for BlockingStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Socket(socket) => socket.write(buffer),
            #[cfg(windows)]
            Self::Pipe(file) => file.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Socket(socket) => socket.flush(),
            #[cfg(windows)]
            Self::Pipe(file) => file.flush(),
        }
    }
}

impl pump::Connection for BlockingStream {
    type Outbound = Self;
    type Inbound = Self;

    fn split(self) -> io::Result<(Self, Self)> {
        let outbound = self.try_clone()?;
        Ok((outbound, self))
    }

    fn end_outbound(mut outbound: Self) -> io::Result<pump::OutboundEnd> {
        outbound.flush()?;
        match outbound {
            #[cfg(unix)]
            Self::Socket(socket) => {
                socket.shutdown(std::net::Shutdown::Write)?;
                // The inbound handle keeps the socket open, so the daemon can
                // still answer after the client stops writing.
                drop(socket);
                Ok(pump::OutboundEnd::HalfClosed)
            }
            #[cfg(windows)]
            Self::Pipe(file) => {
                // A named pipe has no half-close. The client closes its write
                // handle and the pump ends, as the design states.
                drop(file);
                Ok(pump::OutboundEnd::Closed)
            }
        }
    }
}

/// Connect to the endpoint with blocking input and output.
///
/// The call retries while no daemon listens yet, until `deadline`. A client
/// that spawned a daemon uses the deadline to wait for it to bind.
pub(crate) fn connect_blocking(endpoint: &Endpoint, deadline: Instant) -> Result<BlockingStream> {
    #[cfg(unix)]
    let stream = unix::connect(endpoint, deadline)?;
    #[cfg(windows)]
    let stream = windows::connect(endpoint, deadline)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::{BlockingStream, Endpoint, IDENTITY_HEX_CHARS, Stream, VERSION, identity};
    use std::io::{Read, Write};
    use std::path::Path;
    use tokio::io::{AsyncRead, AsyncWrite};

    /// The daemon runs each session on its own task, and the client pumps the
    /// connection from two threads. Both need these bounds.
    #[test]
    fn the_stream_types_satisfy_the_transport_bounds() {
        fn accepts_asynchronous<T: AsyncRead + AsyncWrite + Unpin + Send>() {}
        fn accepts_blocking<T: Read + Write + Send>() {}
        accepts_asynchronous::<Stream>();
        accepts_blocking::<BlockingStream>();
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
}
