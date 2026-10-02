//! The `semctl daemon status` and `semctl daemon stop` client.
//!
//! Both commands are one connection, one request line, one answer line. They
//! never elect and never spawn: asking a daemon that is not there is an
//! answer, not a reason to start one.

use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use tracing::debug;

use super::status::{DaemonStatus, StopAck};
use crate::ipc::handshake::{self, Request};
use crate::ipc::{self, Endpoint, Stream};

/// What both commands report when the endpoint has no daemon.
///
/// The endpoint is derived from the configuration directory, the build
/// version, and the user, so "this configuration" is the accurate scope: a
/// daemon may well be running for another one.
const NO_DAEMON: &str = "no daemon is running for this configuration";

/// Print the running daemon's status.
pub(crate) async fn status(json: bool) -> Result<()> {
    let mut stream = open().await?;
    let line = exchange(&mut stream, &Request::status(), "status").await?;
    let report: DaemonStatus =
        serde_json::from_slice(&line).context("decode the daemon status answer")?;
    if json {
        let rendered =
            serde_json::to_string_pretty(&report).context("render the daemon status as JSON")?;
        println!("{rendered}");
    } else {
        print!("{}", report.render());
    }
    Ok(())
}

/// Ask the running daemon to drain and exit.
pub(crate) async fn stop() -> Result<()> {
    let mut stream = open().await?;
    let line = exchange(&mut stream, &Request::stop(), "stop").await?;
    let ack: StopAck = serde_json::from_slice(&line).context("decode the daemon stop answer")?;
    println!(
        "semctl daemon {} (version {}) is stopping",
        ack.pid, ack.version
    );
    Ok(())
}

/// Connect to this configuration's endpoint, without starting a daemon.
///
/// The deadline is now, so the connection is not retried: a control command
/// reports what is running, and waiting for a daemon that may never bind would
/// only delay that answer. The transport error is logged rather than reported,
/// because "the socket is missing", "the socket is stale", and "the pipe is
/// gone" are the same fact for a person: no daemon is there.
async fn open() -> Result<Stream> {
    let endpoint = Endpoint::current().context("locate the local daemon endpoint")?;
    ipc::connect(&endpoint, Instant::now())
        .await
        .map_err(|error| {
            debug!(
                error = format!("{error:#}"),
                "no daemon answered the local endpoint"
            );
            anyhow!(NO_DAEMON)
        })
}

/// Send one control request and read its one answer line.
///
/// A connection that closes before the answer means the daemon was exiting,
/// which is the same fact as no daemon at all.
async fn exchange(stream: &mut Stream, request: &Request, name: &str) -> Result<Vec<u8>> {
    let no_daemon = |error: handshake::HandshakeError, context: String| {
        if ipc::connection_lost(&error) {
            debug!(%error, "the daemon closed the connection before it answered");
            anyhow!(NO_DAEMON)
        } else {
            anyhow::Error::new(error).context(context)
        }
    };
    handshake::write_line_async(stream, request)
        .await
        .map_err(|error| no_daemon(error, format!("send the {name} request")))?;
    handshake::read_answer_async(stream)
        .await
        .map_err(|error| no_daemon(error, format!("read the {name} answer")))
}

#[cfg(all(test, unix))]
mod tests {
    use super::{NO_DAEMON, exchange};
    use crate::ipc::Stream;
    use crate::ipc::handshake::Request;

    /// A daemon that is exiting can accept from its listen queue and then
    /// close. The request write then fails with a broken pipe or the answer
    /// read sees end of file, depending on the platform. Both mean no daemon.
    #[tokio::test]
    async fn a_daemon_that_closes_before_it_answers_is_no_daemon() {
        let (client, daemon) = tokio::net::UnixStream::pair().expect("a socket pair");
        drop(daemon);
        let mut client = Stream::Socket(client);
        let error = exchange(&mut client, &Request::status(), "status")
            .await
            .expect_err("a closed daemon answers nothing");
        assert_eq!(format!("{error:#}"), NO_DAEMON);
    }
}
