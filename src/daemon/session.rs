//! One attach request, served as one MCP session.
//!
//! The session path is short on purpose. It validates the attach body, builds
//! the same [`McpServer`] the standalone role builds, answers `attached`, and
//! then hands the rest of the connection to `rmcp`. Nothing about the tool
//! surface differs between the two roles: the only difference is where the
//! session's invocation context came from and which stream carries its
//! JSON-RPC traffic.
//!
//! Nothing past the handshake newline was buffered — the handshake reader
//! stops at that byte — so the remaining bytes of the connection are exactly
//! the MCP stream.

use tracing::{debug, info, warn};

use rmcp::ServiceExt;

use super::VERSION;
use super::serve::{ConnectionGuard, Daemon, Sessions};
use crate::ipc::Stream;
use crate::ipc::handshake::{self, Response, SessionRequest};
use crate::mcp::McpServer;
use crate::session::SessionContext;

/// Serve one MCP session over `stream` until the client goes away.
///
/// The connection guard travels with the session, so the daemon counts this
/// connection until the task ends, whether it ended by itself or was aborted
/// by the drain.
pub(super) async fn serve(
    daemon: &Daemon,
    mut stream: Stream,
    request: SessionRequest,
    _connection: ConnectionGuard,
) {
    // The attach body is external data. A context that cannot be built is the
    // client's problem, and it must be told rather than left waiting.
    let context = match SessionContext::from_handshake(request) {
        Ok(context) => context,
        Err(error) => {
            refuse(&mut stream, &format!("{error:#}")).await;
            return;
        }
    };
    let cwd = context.cwd.clone();
    let server = match McpServer::new(context, daemon.engine().clone()).await {
        Ok(server) => server,
        Err(error) => {
            refuse(&mut stream, &format!("cannot build the session: {error:#}")).await;
            return;
        }
    };

    // Counted from here: the session exists, and the answer below promises it.
    let session = Sessions::attach(daemon.sessions().clone(), daemon.pid());
    if let Err(error) =
        handshake::write_line_async(&mut stream, &Response::attached(VERSION, session.id())).await
    {
        debug!(%error, session = session.id(), "the client went away during the attach answer");
        return;
    }
    info!(
        session = session.id(),
        cwd = %cwd.display(),
        sessions = daemon.sessions().session_count(),
        "session attached"
    );

    // Exactly what the standalone role does for its one session: ask the
    // engine for the process-wide update check on this session's behalf, then
    // resolve this session's codebase before the first tool call.
    server.start_update_check();
    // Detached, so `initialize` is answered while the bind runs. This session
    // owns the task and aborts it below, so no bind outlives its session.
    let binding = tokio::spawn({
        let server = server.clone();
        async move { server.bind_at_startup().await }
    });

    match server.serve(stream).await {
        Ok(service) => {
            // The client's end of file, a transport failure, or the drain's
            // abort ends the wait. None of them is a daemon failure: the
            // session is over either way.
            if let Err(error) = service.waiting().await {
                debug!(%error, session = session.id(), "session ended with a transport error");
            }
        }
        Err(error) => warn!(%error, session = session.id(), "could not serve the session"),
    }
    binding.abort();
    info!(session = session.id(), "session ended");
    // Dropping `session` here decrements the session count and wakes the idle
    // timer. The connection guard goes with the task.
}

/// Tell the client why it has no session, then close.
///
/// Best effort on the write: a client that already went away cannot be told
/// anything, and the connection closes either way.
async fn refuse(stream: &mut Stream, reason: &str) {
    debug!(reason, "refusing an attach request");
    if let Err(error) =
        handshake::write_line_async(stream, &Response::rejected(VERSION, reason)).await
    {
        debug!(%error, "could not deliver the refusal");
    }
}
