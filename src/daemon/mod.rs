//! The shared local daemon: one process that serves every MCP session of one
//! operating-system user and one configuration directory.
//!
//! Three roles share this binary. Each one lives in its own child module:
//!
//! - The daemon role ([`serve`]) owns the local endpoint, the shared
//!   [`crate::engine`], and one session per connection.
//! - The session path ([`session`]) turns one attach handshake into one
//!   [`crate::mcp::McpServer`] served over that connection.
//! - The control path ([`control`]) is what `semctl daemon status` and
//!   `semctl daemon stop` speak.
//!
//! The daemon never reads its own environment or working directory for a
//! per-session value. Everything a session needs arrives in its attach body
//! (see [`crate::session::SessionContext::from_handshake`]).

pub(crate) mod control;
pub(crate) mod serve;
mod session;
mod status;

/// The version this build speaks, in the handshake and in the status line.
///
/// The endpoint identity already includes it, so a client of another version
/// reaches another endpoint. The attach check is the second line of defense,
/// for a client that reaches this endpoint some other way.
const VERSION: &str = env!("CARGO_PKG_VERSION");
