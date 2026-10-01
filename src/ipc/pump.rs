//! Asynchronous byte pump used by the client role.
//!
//! The client copies bytes between the process standard streams and one
//! daemon connection. It never parses the stream. Two tasks do the work, so
//! neither direction can block the other.
//!
//! The pump is asynchronous on every platform. A Windows named pipe must be
//! opened for overlapped input and output: two blocking calls on one
//! synchronous file object are serialized, so a blocking read of the answer
//! would block the write of the request. The client therefore builds a small
//! current-thread runtime and uses the same code path as Unix.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// Copy buffer size for each direction.
const BUFFER_BYTES: usize = 16 * 1024;

/// Process exit status of the client role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    /// The daemon closed the connection, or standard output went away.
    Clean,
    /// The connection failed.
    TransportFailed,
}

impl Exit {
    /// The process exit code this outcome reports.
    pub(crate) fn code(self) -> i32 {
        match self {
            Self::Clean => 0,
            Self::TransportFailed => 1,
        }
    }
}

/// Whether the transport can end its outbound direction on its own.
///
/// The value belongs to the transport, so the pump itself stays free of
/// platform knowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HalfClose {
    /// A shutdown ends only the write direction. A Unix domain socket does
    /// this. The pump keeps draining the connection until the daemon closes
    /// it.
    // Reported by the Unix transport only. The pump must stay free of
    // platform knowledge, so both states exist on every target.
    #[cfg_attr(
        windows,
        allow(
            dead_code,
            reason = "reported by the Unix transport and by this module's tests"
        )
    )]
    Supported,
    /// The transport has no half-close. A Windows named pipe is in this
    /// group. The pump ends the connection and finishes when input ends.
    // Reported by the Windows transport only, for the same reason.
    #[cfg_attr(
        not(windows),
        allow(
            dead_code,
            reason = "reported by the Windows transport and by this module's tests"
        )
    )]
    Unsupported,
}

/// What remains readable after the outbound direction ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutboundEnd {
    /// The inbound direction stays open.
    HalfClosed,
    /// The whole connection is closed. Nothing more will arrive.
    Closed,
}

/// What one pump task reports when it finishes.
enum Report {
    /// The inbound task finished with this outcome.
    Inbound(Exit),
    /// The outbound task finished and the outbound direction ended.
    Outbound(OutboundEnd),
    /// The outbound task failed. The connection is broken.
    OutboundFailed,
}

/// Copy bytes between the process standard streams and one connection.
///
/// `input` is standard input and `output` is standard output in the client
/// role. Both are parameters so a test can drive the pump with in-memory
/// duplex pairs.
///
/// The call returns as soon as the connection ends. It does not wait for the
/// outbound task, which is usually still blocked reading standard input: the
/// caller ([`crate::ipc::run_client`]) shuts its runtime down in the
/// background, which abandons that task without blocking, so lingering here
/// would only delay the host's end of file after the daemon goes away.
pub(crate) async fn run<C, I, O>(connection: C, half_close: HalfClose, input: I, output: O) -> Exit
where
    C: AsyncRead + AsyncWrite + Send + 'static,
    I: AsyncRead + Unpin + Send + 'static,
    O: AsyncWrite + Unpin + Send + 'static,
{
    let (reader, writer) = tokio::io::split(connection);
    let (reports, mut inbox) = mpsc::channel(2);
    let outbound_reports = reports.clone();

    // A send failure means the receiver is gone, which happens only after the
    // pump already returned. Nothing is left to report, so it is safe to drop.
    tokio::spawn(async move {
        let report = match copy_outbound(input, writer, half_close).await {
            Ok(end) => Report::Outbound(end),
            Err(_) => Report::OutboundFailed,
        };
        let _ = outbound_reports.send(report).await;
    });
    tokio::spawn(async move {
        let _ = reports
            .send(Report::Inbound(copy_inbound(reader, output).await))
            .await;
    });

    collect(&mut inbox).await
}

/// Wait for the task reports and decide the exit status.
async fn collect(inbox: &mut mpsc::Receiver<Report>) -> Exit {
    loop {
        match inbox.recv().await {
            // The daemon closed the connection, or it broke. Either way the
            // session is over, so the pump returns at once. The outbound task
            // may still be blocked reading standard input; the caller's
            // background runtime shutdown abandons it without waiting.
            // Waiting for it here would only delay the host's end of file —
            // and when the daemon died, that is exactly the signal the host
            // needs promptly to reconnect. On a named pipe a peer that went
            // away surfaces as end of file, so this arm is the crash path too.
            Some(Report::Inbound(exit)) => return exit,
            // A transport with no half-close ended the whole connection when
            // standard input reached end of file. Nothing more will arrive.
            Some(Report::Outbound(OutboundEnd::Closed)) => return Exit::Clean,
            // The outbound direction ended but the connection stays readable
            // (a half-close), or the outbound write failed. Keep draining the
            // inbound direction until the daemon closes it.
            Some(Report::Outbound(OutboundEnd::HalfClosed) | Report::OutboundFailed) => {}
            // Both tasks ended without a report. Treat that as a failure,
            // because no task observed a clean close.
            None => return Exit::TransportFailed,
        }
    }
}

/// Copy standard input into the connection, then end the outbound direction.
async fn copy_outbound<I, W>(
    mut input: I,
    mut writer: W,
    half_close: HalfClose,
) -> io::Result<OutboundEnd>
where
    I: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; BUFFER_BYTES];
    loop {
        let read = match input.read(&mut buffer).await {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        writer.write_all(&buffer[..read]).await?;
        writer.flush().await?;
    }
    match half_close {
        HalfClose::Supported => {
            writer.shutdown().await?;
            Ok(OutboundEnd::HalfClosed)
        }
        HalfClose::Unsupported => {
            // Dropping the write half releases this end of the connection.
            // The pump then stops, and process exit closes the rest.
            drop(writer);
            Ok(OutboundEnd::Closed)
        }
    }
}

/// Copy the connection into standard output.
async fn copy_inbound<R, O>(mut reader: R, mut output: O) -> Exit
where
    R: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; BUFFER_BYTES];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => return Exit::Clean,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Exit::TransportFailed,
        };
        // A closed standard output ends the process cleanly: the host went
        // away, and there is nothing left to deliver to it.
        if output.write_all(&buffer[..read]).await.is_err() || output.flush().await.is_err() {
            return Exit::Clean;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Exit, HalfClose, run};
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};

    /// Capacity of every in-memory pair in this module's tests.
    const PAIR_BYTES: usize = 1024;

    /// An input that never yields a byte and never ends, standing in for a
    /// client whose host is holding standard input open with nothing to send.
    struct StuckInput;

    impl AsyncRead for StuckInput {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    /// A connection that always fails to read and accepts every write.
    struct BrokenConnection;

    impl AsyncRead for BrokenConnection {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::from(io::ErrorKind::ConnectionReset)))
        }
    }

    impl AsyncWrite for BrokenConnection {
        fn poll_write(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Read exactly `count` bytes, so a test never depends on chunk sizes.
    async fn read_exactly(stream: &mut DuplexStream, count: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; count];
        stream
            .read_exact(&mut buffer)
            .await
            .expect("read the expected bytes");
        buffer
    }

    #[tokio::test]
    async fn bytes_flow_in_both_directions() {
        let (client_side, mut daemon_side) = tokio::io::duplex(PAIR_BYTES);
        let (mut host_input, pump_input) = tokio::io::duplex(PAIR_BYTES);
        let (pump_output, mut host_output) = tokio::io::duplex(PAIR_BYTES);

        let pump = tokio::spawn(run(
            client_side,
            HalfClose::Supported,
            pump_input,
            pump_output,
        ));

        host_input
            .write_all(b"ping\n")
            .await
            .expect("write to the pump");
        assert_eq!(read_exactly(&mut daemon_side, 5).await, b"ping\n");

        daemon_side
            .write_all(b"pong\n")
            .await
            .expect("write to the client");
        assert_eq!(read_exactly(&mut host_output, 5).await, b"pong\n");

        // End of file on standard input half-closes the outbound direction.
        drop(host_input);
        let mut drained = Vec::new();
        daemon_side
            .read_to_end(&mut drained)
            .await
            .expect("the outbound direction ends");
        assert!(drained.is_empty(), "{drained:?}");

        // The daemon closes its end, which ends the pump cleanly.
        drop(daemon_side);
        assert_eq!(pump.await.expect("the pump task finished"), Exit::Clean);
    }

    #[tokio::test]
    async fn input_end_of_file_half_closes_and_waits_for_the_daemon() {
        let (client_side, mut daemon_side) = tokio::io::duplex(PAIR_BYTES);
        let (host_input, pump_input) = tokio::io::duplex(PAIR_BYTES);
        let (pump_output, _host_output) = tokio::io::duplex(PAIR_BYTES);

        let pump = tokio::spawn(run(
            client_side,
            HalfClose::Supported,
            pump_input,
            pump_output,
        ));

        // End of file on standard input ends only the outbound direction.
        drop(host_input);
        let mut drained = Vec::new();
        daemon_side
            .read_to_end(&mut drained)
            .await
            .expect("the outbound direction ends");
        assert!(drained.is_empty(), "{drained:?}");
        assert!(!pump.is_finished(), "the pump must still drain");

        // The pump ends when the daemon closes its end.
        drop(daemon_side);
        assert_eq!(pump.await.expect("the pump task finished"), Exit::Clean);
    }

    #[tokio::test]
    async fn input_end_of_file_ends_the_pump_when_the_transport_cannot_half_close() {
        let (client_side, _daemon_side) = tokio::io::duplex(PAIR_BYTES);
        let (host_input, pump_input) = tokio::io::duplex(PAIR_BYTES);
        let (pump_output, _host_output) = tokio::io::duplex(PAIR_BYTES);

        let pump = tokio::spawn(run(
            client_side,
            HalfClose::Unsupported,
            pump_input,
            pump_output,
        ));

        // The inbound direction stays open, so only the closed transport can
        // end the pump.
        drop(host_input);
        assert_eq!(pump.await.expect("the pump task finished"), Exit::Clean);
    }

    #[tokio::test]
    async fn a_closed_output_ends_the_pump_cleanly() {
        let (client_side, mut daemon_side) = tokio::io::duplex(PAIR_BYTES);
        let (pump_output, host_output) = tokio::io::duplex(PAIR_BYTES);

        // Dropping the reading end makes every write to the pump output fail.
        drop(host_output);
        let pump = tokio::spawn(run(
            client_side,
            HalfClose::Supported,
            tokio::io::empty(),
            pump_output,
        ));

        daemon_side
            .write_all(b"pong\n")
            .await
            .expect("write to the client");
        assert_eq!(pump.await.expect("the pump task finished"), Exit::Clean);
    }

    #[tokio::test]
    async fn a_broken_connection_ends_the_pump_with_the_failure_status() {
        let (pump_output, _host_output) = tokio::io::duplex(PAIR_BYTES);
        let exit = run(
            BrokenConnection,
            HalfClose::Supported,
            tokio::io::empty(),
            pump_output,
        )
        .await;
        assert_eq!(exit, Exit::TransportFailed);
    }

    /// When the connection ends — the daemon was killed, say — the pump must
    /// not wait for the outbound task still blocked reading standard input.
    /// Waiting would delay the host's end of file, which is how the host
    /// learns the daemon is gone and reconnects. Paused time makes it
    /// deterministic: had the pump kept the old two-second courtesy wait, the
    /// clock would jump well past this bound.
    #[tokio::test(start_paused = true)]
    async fn a_connection_end_does_not_wait_for_a_blocked_outbound_task() {
        let (pump_output, _host_output) = tokio::io::duplex(PAIR_BYTES);
        let start = tokio::time::Instant::now();
        let exit = run(
            BrokenConnection,
            HalfClose::Unsupported,
            StuckInput,
            pump_output,
        )
        .await;
        assert_eq!(exit, Exit::TransportFailed);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a connection that ended must not linger for a stuck outbound task"
        );
    }

    #[test]
    fn the_exit_codes_match_the_documented_statuses() {
        assert_eq!(Exit::Clean.code(), 0);
        assert_eq!(Exit::TransportFailed.code(), 1);
    }
}
