//! Messages that a host sends before `initialize`.
//!
//! `rmcp` accepts only `ping` before `initialize`. Any other message ends the
//! handshake with an error, and the session ends. oh-my-pi 19.1 and later send
//! a `server/discover` request before `initialize`, so `semctl mcp` never
//! attached to them. The [`PreInitialize`] transport keeps such a session open
//! until `initialize` arrives:
//!
//! - A request other than `initialize` or `ping` gets a JSON-RPC "method not
//!   found" error with the same `id`.
//! - A notification is dropped. JSON-RPC forbids an answer to a notification.
//! - Every other message, and every message from `initialize` on, passes
//!   through unchanged.
//!
//! The `rmcp` stream transport answers a line that is not JSON-RPC with a parse
//! error and continues. The end of the stream before `initialize` still ends
//! the session.

use rmcp::RoleServer;
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, ErrorCode, ErrorData, ServerJsonRpcMessage,
};
use rmcp::service::{RunningService, ServerInitializeError, ServiceExt};
use rmcp::transport::{IntoTransport, Transport};
use tracing::{debug, warn};

use super::McpServer;

impl McpServer {
    /// Serve one MCP session on `transport`. The standalone role and the
    /// daemon role both start their sessions here. The error is boxed because
    /// `rmcp`'s initialize error is large and both callers only display it.
    pub(crate) async fn serve_session<E, A>(
        self,
        transport: impl IntoTransport<RoleServer, E, A>,
    ) -> Result<RunningService<RoleServer, Self>, Box<ServerInitializeError>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        self.serve(PreInitialize::new(transport.into_transport()))
            .await
            .map_err(Box::new)
    }
}

/// A server transport that answers or drops the messages that a host sends
/// before `initialize`.
struct PreInitialize<T> {
    inner: T,
    /// Set when the `initialize` request has passed through.
    initialize_seen: bool,
}

impl<T> PreInitialize<T> {
    fn new(inner: T) -> Self {
        Self {
            inner,
            initialize_seen: false,
        }
    }
}

impl<T: Transport<RoleServer>> Transport<RoleServer> for PreInitialize<T> {
    type Error = T::Error;

    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        loop {
            let message = self.inner.receive().await?;
            if self.initialize_seen {
                return Some(message);
            }
            let request = match &message {
                ClientJsonRpcMessage::Request(request) => request,
                ClientJsonRpcMessage::Notification(_) => {
                    debug!("dropped a notification sent before initialize");
                    continue;
                }
                _ => return Some(message),
            };
            match &request.request {
                ClientRequest::InitializeRequest(_) => {
                    self.initialize_seen = true;
                    return Some(message);
                }
                // `rmcp` answers a `ping` before `initialize` itself.
                ClientRequest::PingRequest(_) => return Some(message),
                other => {
                    let method = other.method();
                    debug!(method, "answered a request sent before initialize");
                    let error = ErrorData::new(
                        ErrorCode::METHOD_NOT_FOUND,
                        format!("method not supported before initialize: {method}"),
                        None,
                    );
                    let answer = ServerJsonRpcMessage::error(error, Some(request.id.clone()));
                    if let Err(error) = self.inner.send(answer).await {
                        // The host cannot read answers any more, so the
                        // session is over. Report it as a closed connection.
                        warn!(%error, "could not answer a request sent before initialize");
                        return None;
                    }
                }
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests;
