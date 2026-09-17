//! Blocking byte pump used by the client role.
//!
//! The client copies bytes between the process standard streams and one
//! daemon connection. It never parses the stream. Two threads do the work, so
//! neither direction can block the other. This file uses no asynchronous
//! runtime: the client role must decide its role before a runtime exists.

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Copy buffer size for each direction.
const BUFFER_BYTES: usize = 16 * 1024;

/// Longest wait for the outbound thread after the inbound thread ends.
const OUTBOUND_WAIT: Duration = Duration::from_secs(2);

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

/// What remains readable after the outbound direction ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundEnd {
    /// The inbound direction stays open. This is the Unix half-close.
    HalfClosed,
    /// The whole connection is closed. Nothing more will arrive.
    Closed,
}

/// One duplex connection two pump threads can drive at the same time.
pub(crate) trait Connection: Send {
    /// The handle the outbound thread writes to.
    type Outbound: Write + Send + 'static;
    /// The handle the inbound thread reads from.
    type Inbound: Read + Send + 'static;

    /// Split the connection into one write handle and one read handle.
    ///
    /// Both handles address the same connection.
    fn split(self) -> io::Result<(Self::Outbound, Self::Inbound)>;

    /// End the outbound direction after standard input reaches end of file.
    ///
    /// A transport that supports a half-close ends only the write direction
    /// and reports [`OutboundEnd::HalfClosed`]. A transport without one closes
    /// the connection and reports [`OutboundEnd::Closed`].
    fn end_outbound(outbound: Self::Outbound) -> io::Result<OutboundEnd>;
}

/// What one pump thread reports when it finishes.
enum Report {
    /// The inbound thread finished with this outcome.
    Inbound(Exit),
    /// The outbound thread finished and the outbound direction ended.
    Outbound(OutboundEnd),
    /// The outbound thread failed. The connection is broken.
    OutboundFailed,
}

/// Copy bytes between the process standard streams and one connection.
///
/// `input` is standard input and `output` is standard output in the client
/// role. Both are parameters so a test can drive the pump with pipes.
///
/// The call returns when the connection ends. It does not wait for a thread
/// that is still blocked on standard input for longer than [`OUTBOUND_WAIT`].
pub(crate) fn run<C, I, O>(connection: C, input: I, output: O) -> io::Result<Exit>
where
    C: Connection,
    I: Read + Send + 'static,
    O: Write + Send + 'static,
{
    let (outbound, inbound) = connection.split()?;
    let (reports, inbox) = mpsc::channel();
    let outbound_reports = reports.clone();

    // A send failure means the receiver is gone, which happens only after the
    // pump already returned. Nothing is left to report, so it is safe to drop.
    thread::Builder::new()
        .name("semctl-ipc-out".to_string())
        .spawn(move || {
            let report = match copy_outbound::<C, I>(input, outbound) {
                Ok(end) => Report::Outbound(end),
                Err(_) => Report::OutboundFailed,
            };
            let _ = outbound_reports.send(report);
        })?;
    thread::Builder::new()
        .name("semctl-ipc-in".to_string())
        .spawn(move || {
            let _ = reports.send(Report::Inbound(copy_inbound(inbound, output)));
        })?;

    Ok(collect(&inbox))
}

/// Wait for the thread reports and decide the exit status.
fn collect(inbox: &mpsc::Receiver<Report>) -> Exit {
    let mut outbound_pending = true;
    loop {
        match inbox.recv() {
            Ok(Report::Inbound(exit)) => {
                if outbound_pending {
                    // Best effort. The outbound thread is usually still
                    // blocked reading standard input, and process exit ends
                    // it. Waiting longer would delay the exit.
                    let _ = inbox.recv_timeout(OUTBOUND_WAIT);
                }
                return exit;
            }
            // A closed connection has nothing left to deliver, so the pump
            // ends without waiting for the inbound thread.
            Ok(Report::Outbound(OutboundEnd::Closed)) => return Exit::Clean,
            Ok(Report::Outbound(OutboundEnd::HalfClosed) | Report::OutboundFailed) => {
                outbound_pending = false;
            }
            // Both threads ended without a report. Treat that as a failure,
            // because no thread observed a clean close.
            Err(_) => return Exit::TransportFailed,
        }
    }
}

/// Copy standard input into the connection, then end the outbound direction.
fn copy_outbound<C, I>(mut input: I, mut outbound: C::Outbound) -> io::Result<OutboundEnd>
where
    C: Connection,
    I: Read,
{
    let mut buffer = vec![0u8; BUFFER_BYTES];
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        outbound.write_all(&buffer[..read])?;
        outbound.flush()?;
    }
    C::end_outbound(outbound)
}

/// Copy the connection into standard output.
fn copy_inbound<R, W>(mut inbound: R, mut output: W) -> Exit
where
    R: Read,
    W: Write,
{
    let mut buffer = vec![0u8; BUFFER_BYTES];
    loop {
        let read = match inbound.read(&mut buffer) {
            Ok(0) => return Exit::Clean,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Exit::TransportFailed,
        };
        // A closed standard output ends the process cleanly: the host went
        // away, and there is nothing left to deliver to it.
        if output
            .write_all(&buffer[..read])
            .and_then(|()| output.flush())
            .is_err()
        {
            return Exit::Clean;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Connection, Exit, OutboundEnd, run};
    use std::io::{self, PipeReader, PipeWriter, Read, Write};

    /// A connection built from two one-way pipes, one for each direction.
    struct TestConnection {
        outbound: PipeWriter,
        inbound: Box<dyn Read + Send>,
        end: OutboundEnd,
    }

    /// The write handle, which remembers what the transport does at the end.
    struct TestOutbound {
        writer: PipeWriter,
        end: OutboundEnd,
    }

    impl Write for TestOutbound {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.writer.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.writer.flush()
        }
    }

    impl Connection for TestConnection {
        type Outbound = TestOutbound;
        type Inbound = Box<dyn Read + Send>;

        fn split(self) -> io::Result<(Self::Outbound, Self::Inbound)> {
            Ok((
                TestOutbound {
                    writer: self.outbound,
                    end: self.end,
                },
                self.inbound,
            ))
        }

        fn end_outbound(outbound: Self::Outbound) -> io::Result<OutboundEnd> {
            let end = outbound.end;
            // Dropping the write handle makes the far end read end of file.
            drop(outbound);
            Ok(end)
        }
    }

    /// A standard output that always fails, as a closed pipe does.
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A connection that always fails to read.
    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::ConnectionReset))
        }
    }

    fn read_exactly(reader: &mut PipeReader, count: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; count];
        reader
            .read_exact(&mut buffer)
            .expect("read the expected bytes");
        buffer
    }

    #[test]
    fn bytes_flow_in_both_directions() {
        let (input_reader, mut input_writer) = io::pipe().expect("standard input pipe");
        let (mut output_reader, output_writer) = io::pipe().expect("standard output pipe");
        let (mut to_daemon, from_client) = io::pipe().expect("outbound pipe");
        let (to_client, mut from_daemon) = io::pipe().expect("inbound pipe");

        let connection = TestConnection {
            outbound: from_client,
            inbound: Box::new(to_client),
            end: OutboundEnd::HalfClosed,
        };
        let pump = std::thread::spawn(move || {
            run(connection, input_reader, output_writer).expect("start the pump")
        });

        input_writer
            .write_all(b"ping\n")
            .expect("write to the pump");
        assert_eq!(read_exactly(&mut to_daemon, 5), b"ping\n");

        from_daemon
            .write_all(b"pong\n")
            .expect("write to the client");
        assert_eq!(read_exactly(&mut output_reader, 5), b"pong\n");

        // End of file on standard input half-closes the outbound direction.
        drop(input_writer);
        let mut drained = Vec::new();
        to_daemon
            .read_to_end(&mut drained)
            .expect("outbound direction ends");
        assert!(drained.is_empty());

        // The daemon closes its end, which ends the pump cleanly.
        drop(from_daemon);
        assert_eq!(pump.join().expect("join the pump"), Exit::Clean);
    }

    #[test]
    fn standard_input_end_of_file_half_closes_and_waits_for_the_daemon() {
        let (input_reader, input_writer) = io::pipe().expect("standard input pipe");
        let (_output_reader, output_writer) = io::pipe().expect("standard output pipe");
        let (mut to_daemon, from_client) = io::pipe().expect("outbound pipe");
        let (to_client, from_daemon) = io::pipe().expect("inbound pipe");

        let connection = TestConnection {
            outbound: from_client,
            inbound: Box::new(to_client),
            end: OutboundEnd::HalfClosed,
        };
        let pump = std::thread::spawn(move || {
            run(connection, input_reader, output_writer).expect("start the pump")
        });

        // End of file on standard input ends only the outbound direction.
        drop(input_writer);
        let mut drained = Vec::new();
        to_daemon
            .read_to_end(&mut drained)
            .expect("outbound direction ends");
        assert!(drained.is_empty());

        // The pump is still running. It ends when the daemon closes its end.
        drop(from_daemon);
        assert_eq!(pump.join().expect("join the pump"), Exit::Clean);
    }

    #[test]
    fn standard_input_end_of_file_ends_the_pump_when_the_transport_closes() {
        let (input_reader, input_writer) = io::pipe().expect("standard input pipe");
        let (_output_reader, output_writer) = io::pipe().expect("standard output pipe");
        let (_to_daemon, from_client) = io::pipe().expect("outbound pipe");
        let (to_client, _from_daemon) = io::pipe().expect("inbound pipe");

        let connection = TestConnection {
            outbound: from_client,
            inbound: Box::new(to_client),
            end: OutboundEnd::Closed,
        };
        let pump = std::thread::spawn(move || {
            run(connection, input_reader, output_writer).expect("start the pump")
        });

        // The inbound direction stays open, so only the closed transport can
        // end the pump.
        drop(input_writer);
        assert_eq!(pump.join().expect("join the pump"), Exit::Clean);
    }

    #[test]
    fn a_closed_standard_output_ends_the_pump_cleanly() {
        let (_to_daemon, from_client) = io::pipe().expect("outbound pipe");
        let (to_client, mut from_daemon) = io::pipe().expect("inbound pipe");

        let connection = TestConnection {
            outbound: from_client,
            inbound: Box::new(to_client),
            end: OutboundEnd::HalfClosed,
        };
        let pump = std::thread::spawn(move || {
            run(connection, io::empty(), FailingWriter).expect("start the pump")
        });

        from_daemon
            .write_all(b"pong\n")
            .expect("write to the client");
        assert_eq!(pump.join().expect("join the pump"), Exit::Clean);
    }

    #[test]
    fn a_broken_connection_ends_the_pump_with_the_failure_status() {
        let (_output_reader, output_writer) = io::pipe().expect("standard output pipe");
        let (_to_daemon, from_client) = io::pipe().expect("outbound pipe");

        let connection = TestConnection {
            outbound: from_client,
            inbound: Box::new(FailingReader),
            end: OutboundEnd::HalfClosed,
        };
        let pump = std::thread::spawn(move || {
            run(connection, io::empty(), output_writer).expect("start the pump")
        });

        assert_eq!(pump.join().expect("join the pump"), Exit::TransportFailed);
    }

    #[test]
    fn the_exit_codes_match_the_documented_statuses() {
        assert_eq!(Exit::Clean.code(), 0);
        assert_eq!(Exit::TransportFailed.code(), 1);
    }
}
