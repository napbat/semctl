//! Attach and control messages for one local daemon connection.
//!
//! The client and the daemon exchange one line of JSON in each direction
//! before the connection carries Model Context Protocol (MCP) traffic. Each
//! line is UTF-8 and ends with one newline byte.
//!
//! Both sides are asynchronous. The daemon serves its connections on Tokio,
//! and the client role builds a small current-thread runtime, because a
//! Windows named pipe must be opened for overlapped input and output.
//!
//! Every reader here reads one byte at a time and stops at the first newline
//! byte. It must not buffer past that byte: the bytes that follow the
//! handshake belong to the MCP stream, and a buffered remainder would be lost.
//! A handshake line is short, so the cost of the byte loop is not material.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Wire version of the attach and control messages.
pub(crate) const PROTOCOL: u32 = 1;

/// Largest handshake line, including the terminating newline byte.
///
/// A handshake line arrives from a peer, so the bound is tight: it decides how
/// much a connection can make this process allocate before it is trusted.
pub(crate) const MAX_LINE_BYTES: usize = 64 * 1024;

/// Largest control answer, including the terminating newline byte.
///
/// A control answer is the other direction: this daemon builds it for a client
/// that already reached this endpoint, and its size grows with the number of
/// checkouts the daemon keeps in sync. One checkout costs a few hundred bytes,
/// so the handshake bound would cut the answer off at a few hundred checkouts,
/// well inside the scale this daemon serves. The bound stays finite so a
/// client still refuses an answer that cannot be a status line.
pub(crate) const MAX_ANSWER_BYTES: usize = 8 * 1024 * 1024;

/// Longest time one handshake exchange may take.
pub(crate) const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest time a client waits for the daemon's attach answer.
///
/// Deliberately longer than [`EXCHANGE_TIMEOUT`]. The answer comes from a
/// daemon that may be absorbing a burst of simultaneous cold attaches, and a
/// client that gives up early either fails the session (`require`) or falls
/// back to a standalone server that forfeits the sharing the daemon exists
/// for. The five-second bound stays on every daemon-side read: that is the
/// protection against a silent connection, and a client writes its one
/// request line immediately.
pub(crate) const ATTACH_ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// Message kinds a daemon accepts from a client.
const REQUEST_KINDS: &[&str] = &["attach", "status", "stop"];

/// Message kinds a client accepts from a daemon.
const RESPONSE_KINDS: &[&str] = &["attached", "rejected"];

/// A secret carried in the attach body.
///
/// The `Debug` output is redacted. A session token must not reach a log, a
/// trace field, or an error message.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Token(String);

impl Token {
    /// Wrap a secret value.
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Read the secret value. Call this only where the secret is used.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Token(redacted)")
    }
}

/// Per-connection invocation context the client sends to the daemon.
///
/// The daemon builds its session state from this body only. It must not read
/// its own process environment for any of these values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionRequest {
    /// Absolute working directory of the client process.
    pub(crate) cwd: PathBuf,
    /// Server URL override, from `--server` or `SEMCTX_SERVER`.
    #[serde(default)]
    pub(crate) server: Option<String>,
    /// Tenant override, from `--tenant` or `SEMCTX_TENANT`.
    #[serde(default)]
    pub(crate) tenant: Option<String>,
    /// Codebase pin, from `--codebase` or `SEMCTX_CODEBASE`.
    #[serde(default)]
    pub(crate) codebase: Option<String>,
    /// Invocation token, from `SEMCTX_TOKEN`. Absent means the stored login.
    #[serde(default)]
    pub(crate) token: Option<Token>,
    /// Resync interval override, from `SEMCTX_MCP_RESYNC_SECS`.
    #[serde(default)]
    pub(crate) resync_secs: Option<u64>,
    /// Whether this session wants the update note.
    pub(crate) update_check: bool,
}

/// The first line a client sends on a new connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Request {
    /// Start one MCP session over this connection.
    Attach {
        /// Wire version. Must equal [`PROTOCOL`].
        protocol: u32,
        /// Client package version, for diagnostics.
        version: String,
        /// Invocation context for the new session.
        session: Box<SessionRequest>,
    },
    /// Ask for one status line, then close.
    Status {
        /// Wire version. Must equal [`PROTOCOL`].
        protocol: u32,
    },
    /// Ask the daemon to drain and exit.
    Stop {
        /// Wire version. Must equal [`PROTOCOL`].
        protocol: u32,
    },
}

impl Request {
    /// Build an attach request for this build's version.
    pub(crate) fn attach(version: impl Into<String>, session: SessionRequest) -> Self {
        Self::Attach {
            protocol: PROTOCOL,
            version: version.into(),
            session: Box::new(session),
        }
    }

    /// Build a status request.
    pub(crate) fn status() -> Self {
        Self::Status { protocol: PROTOCOL }
    }

    /// Build a stop request.
    pub(crate) fn stop() -> Self {
        Self::Stop { protocol: PROTOCOL }
    }
}

/// The first line a daemon sends in answer to a [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Response {
    /// The session is open. Every later byte is MCP traffic.
    Attached {
        /// Wire version. Must equal [`PROTOCOL`].
        protocol: u32,
        /// Daemon package version, for diagnostics.
        version: String,
        /// Opaque session identifier, for status output and logs.
        session_id: String,
    },
    /// The daemon refused the request and closes the connection.
    Rejected {
        /// Wire version. Must equal [`PROTOCOL`].
        protocol: u32,
        /// Daemon package version, for diagnostics.
        version: String,
        /// Reason text, safe to show to a person. It carries no secret.
        reason: String,
    },
}

impl Response {
    /// Build an accepted answer.
    pub(crate) fn attached(version: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self::Attached {
            protocol: PROTOCOL,
            version: version.into(),
            session_id: session_id.into(),
        }
    }

    /// Build a refused answer.
    pub(crate) fn rejected(version: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Rejected {
            protocol: PROTOCOL,
            version: version.into(),
            reason: reason.into(),
        }
    }
}

/// Why one handshake line could not be exchanged or understood.
#[derive(Debug)]
pub(crate) enum HandshakeError {
    /// The transport failed.
    Transport(io::Error),
    /// The peer closed the connection before it sent a complete line.
    Closed,
    /// The exchange did not finish inside [`EXCHANGE_TIMEOUT`].
    TimedOut,
    /// The line reached [`MAX_LINE_BYTES`] without a newline byte.
    LineTooLong,
    /// The line is not the JSON object this protocol defines.
    Malformed(String),
    /// The `kind` field names a message this build does not know.
    UnknownKind(String),
    /// The `protocol` field does not match [`PROTOCOL`].
    ProtocolMismatch {
        /// The version this build speaks.
        expected: u32,
        /// The version the peer sent.
        received: u32,
    },
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "handshake transport failed: {error}"),
            Self::Closed => formatter.write_str("peer closed the connection during the handshake"),
            Self::TimedOut => formatter.write_str("handshake did not finish in time"),
            Self::LineTooLong => write!(
                formatter,
                "handshake line is longer than {MAX_LINE_BYTES} bytes"
            ),
            Self::Malformed(detail) => write!(formatter, "handshake line is malformed: {detail}"),
            Self::UnknownKind(kind) => write!(formatter, "unknown handshake kind {kind:?}"),
            Self::ProtocolMismatch { expected, received } => write!(
                formatter,
                "handshake protocol {received} does not match {expected}"
            ),
        }
    }
}

impl std::error::Error for HandshakeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for HandshakeError {
    fn from(error: io::Error) -> Self {
        Self::Transport(error)
    }
}

/// Serialize one message as a complete line, including the newline byte.
#[cfg(test)]
fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, HandshakeError> {
    encode_bounded(message, MAX_LINE_BYTES)
}

/// Encode one message as a complete line, within `limit` bytes.
fn encode_bounded<T: Serialize>(message: &T, limit: usize) -> Result<Vec<u8>, HandshakeError> {
    let mut line = serde_json::to_vec(message)
        .map_err(|error| HandshakeError::Malformed(error.to_string()))?;
    if line.len() > limit - 1 {
        return Err(HandshakeError::LineTooLong);
    }
    line.push(b'\n');
    Ok(line)
}

/// Fields every handshake line carries, read before the body is trusted.
#[derive(Deserialize)]
struct Envelope {
    kind: String,
    protocol: u32,
}

/// Validate the envelope, then decode the body.
///
/// The envelope pass gives a typed error for an unknown `kind` and for a
/// protocol mismatch. Without it both cases would surface as one untyped
/// serde message.
fn decode<T: DeserializeOwned>(line: &[u8], known: &[&str]) -> Result<T, HandshakeError> {
    let envelope: Envelope = serde_json::from_slice(line)
        .map_err(|error| HandshakeError::Malformed(error.to_string()))?;
    if !known.contains(&envelope.kind.as_str()) {
        return Err(HandshakeError::UnknownKind(envelope.kind));
    }
    if envelope.protocol != PROTOCOL {
        return Err(HandshakeError::ProtocolMismatch {
            expected: PROTOCOL,
            received: envelope.protocol,
        });
    }
    serde_json::from_slice(line).map_err(|error| HandshakeError::Malformed(error.to_string()))
}

/// Decode one client-to-daemon line.
pub(crate) fn decode_request(line: &[u8]) -> Result<Request, HandshakeError> {
    decode(line, REQUEST_KINDS)
}

/// Decode one daemon-to-client line.
pub(crate) fn decode_response(line: &[u8]) -> Result<Response, HandshakeError> {
    decode(line, RESPONSE_KINDS)
}

/// Read one byte, or report the end of the stream.
async fn read_byte_async<R: AsyncRead + Unpin + ?Sized>(reader: &mut R) -> io::Result<Option<u8>> {
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte).await {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Read one line from an asynchronous stream, without the newline byte.
///
/// The whole read must finish inside [`EXCHANGE_TIMEOUT`].
pub(crate) async fn read_line_async<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
) -> Result<Vec<u8>, HandshakeError> {
    read_bounded(reader, MAX_LINE_BYTES).await
}

/// Read one control answer, which may be longer than a handshake line.
///
/// Nothing follows a control answer on that connection: the daemon closes it
/// after one answer. The byte loop is kept all the same, so one reader serves
/// both directions.
pub(crate) async fn read_answer_async<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
) -> Result<Vec<u8>, HandshakeError> {
    read_bounded(reader, MAX_ANSWER_BYTES).await
}

/// Read the daemon's attach answer, inside [`ATTACH_ANSWER_TIMEOUT`].
///
/// Client-side only. The line bound is the handshake bound; the wait is the
/// longer one, because the peer is a daemon that may be absorbing an attach
/// burst rather than a silent stranger.
pub(crate) async fn read_attach_answer_async<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
) -> Result<Vec<u8>, HandshakeError> {
    match tokio::time::timeout(
        ATTACH_ANSWER_TIMEOUT,
        read_line_unbounded(reader, MAX_LINE_BYTES),
    )
    .await
    {
        Ok(result) => result,
        Err(_elapsed) => Err(HandshakeError::TimedOut),
    }
}

/// Read one line of at most `limit` bytes, inside [`EXCHANGE_TIMEOUT`].
async fn read_bounded<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
    limit: usize,
) -> Result<Vec<u8>, HandshakeError> {
    match tokio::time::timeout(EXCHANGE_TIMEOUT, read_line_unbounded(reader, limit)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(HandshakeError::TimedOut),
    }
}

/// The body of [`read_bounded`], without the timeout.
async fn read_line_unbounded<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
    limit: usize,
) -> Result<Vec<u8>, HandshakeError> {
    let mut line = Vec::new();
    loop {
        match read_byte_async(reader).await? {
            None => return Err(HandshakeError::Closed),
            Some(b'\n') => return Ok(line),
            Some(byte) => {
                if line.len() == limit - 1 {
                    return Err(HandshakeError::LineTooLong);
                }
                line.push(byte);
            }
        }
    }
}

/// Write one message as a complete line to an asynchronous stream.
///
/// The whole write must finish inside [`EXCHANGE_TIMEOUT`].
pub(crate) async fn write_line_async<W: AsyncWrite + Unpin + ?Sized, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), HandshakeError> {
    write_bounded(writer, message, MAX_LINE_BYTES).await
}

/// Write one control answer, which may be longer than a handshake line.
pub(crate) async fn write_answer_async<W: AsyncWrite + Unpin + ?Sized, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), HandshakeError> {
    write_bounded(writer, message, MAX_ANSWER_BYTES).await
}

/// Write one message as a complete line of at most `limit` bytes.
async fn write_bounded<W: AsyncWrite + Unpin + ?Sized, T: Serialize>(
    writer: &mut W,
    message: &T,
    limit: usize,
) -> Result<(), HandshakeError> {
    let line = encode_bounded(message, limit)?;
    let write = async {
        writer.write_all(&line).await?;
        writer.flush().await
    };
    match tokio::time::timeout(EXCHANGE_TIMEOUT, write).await {
        Ok(result) => result.map_err(HandshakeError::Transport),
        Err(_elapsed) => Err(HandshakeError::TimedOut),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HandshakeError, MAX_ANSWER_BYTES, MAX_LINE_BYTES, PROTOCOL, Request, Response,
        SessionRequest, Token, decode, decode_request, decode_response, encode, read_answer_async,
        read_attach_answer_async, read_line_async, write_answer_async, write_line_async,
    };
    use std::path::PathBuf;

    fn session() -> SessionRequest {
        SessionRequest {
            cwd: PathBuf::from("/abs/path"),
            server: Some("https://example.invalid".to_string()),
            tenant: None,
            codebase: None,
            token: Some(Token::new("super-secret-value")),
            resync_secs: Some(30),
            update_check: true,
        }
    }

    #[test]
    fn attach_request_round_trips_through_one_line() {
        let line = encode(&Request::attach("0.2.0", session())).expect("encode attach");
        assert_eq!(line.last(), Some(&b'\n'));
        let decoded = decode_request(&line).expect("decode attach");
        let Request::Attach {
            protocol,
            version,
            session,
        } = decoded
        else {
            panic!("expected an attach request");
        };
        assert_eq!(protocol, PROTOCOL);
        assert_eq!(version, "0.2.0");
        assert_eq!(session.cwd, PathBuf::from("/abs/path"));
        assert_eq!(
            session.token.as_ref().map(Token::expose),
            Some("super-secret-value")
        );
        assert_eq!(session.resync_secs, Some(30));
        assert!(session.update_check);
    }

    #[test]
    fn status_and_stop_requests_round_trip() {
        let status = encode(&Request::status()).expect("encode status");
        assert!(matches!(
            decode_request(&status),
            Ok(Request::Status { protocol }) if protocol == PROTOCOL
        ));
        let stop = encode(&Request::stop()).expect("encode stop");
        assert!(matches!(
            decode_request(&stop),
            Ok(Request::Stop { protocol }) if protocol == PROTOCOL
        ));
    }

    #[test]
    fn attached_and_rejected_responses_round_trip() {
        let attached = encode(&Response::attached("0.2.0", "session-1")).expect("encode attached");
        let Ok(Response::Attached {
            protocol,
            version,
            session_id,
        }) = decode_response(&attached)
        else {
            panic!("expected an attached response");
        };
        assert_eq!(protocol, PROTOCOL);
        assert_eq!(version, "0.2.0");
        assert_eq!(session_id, "session-1");

        let rejected = encode(&Response::rejected("0.2.0", "no")).expect("encode rejected");
        let Ok(Response::Rejected { reason, .. }) = decode_response(&rejected) else {
            panic!("expected a rejected response");
        };
        assert_eq!(reason, "no");
    }

    #[test]
    fn kind_names_match_the_documented_wire_form() {
        let line = encode(&Request::attach("0.2.0", session())).expect("encode attach");
        let text = String::from_utf8(line).expect("attach line is utf-8");
        assert!(text.contains("\"kind\":\"attach\""), "{text}");
        let line = encode(&Response::attached("0.2.0", "s")).expect("encode attached");
        let text = String::from_utf8(line).expect("attached line is utf-8");
        assert!(text.contains("\"kind\":\"attached\""), "{text}");
    }

    #[tokio::test]
    async fn oversized_line_is_rejected_before_it_is_parsed() {
        let mut oversized = vec![b'x'; MAX_LINE_BYTES + 16];
        oversized.push(b'\n');
        let error = read_line_async(&mut oversized.as_slice())
            .await
            .expect_err("too long");
        assert!(matches!(error, HandshakeError::LineTooLong), "{error:?}");
    }

    /// The attach answer gets a longer wait, not a longer line: its bound is
    /// still the handshake bound.
    #[tokio::test]
    async fn the_attach_answer_reader_keeps_the_handshake_line_bound() {
        let mut oversized = vec![b'x'; MAX_LINE_BYTES + 16];
        oversized.push(b'\n');
        let error = read_attach_answer_async(&mut oversized.as_slice())
            .await
            .expect_err("too long");
        assert!(matches!(error, HandshakeError::LineTooLong), "{error:?}");

        let mut wire: Vec<u8> = Vec::new();
        write_line_async(&mut wire, &Response::attached("0.2.0", "s"))
            .await
            .expect("write attached");
        let line = read_attach_answer_async(&mut wire.as_slice())
            .await
            .expect("read the attach answer");
        assert!(matches!(
            decode_response(&line),
            Ok(Response::Attached { .. })
        ));
    }

    #[tokio::test]
    async fn a_line_at_the_limit_is_accepted() {
        let mut line = vec![b'x'; MAX_LINE_BYTES - 1];
        line.push(b'\n');
        let read = read_line_async(&mut line.as_slice())
            .await
            .expect("line at the limit");
        assert_eq!(read.len(), MAX_LINE_BYTES - 1);
    }

    /// A status answer carries one entry per checkout. The daemon serves a
    /// thousand of them, so the answer must not be cut off at the handshake
    /// bound.
    #[tokio::test]
    async fn a_control_answer_may_be_longer_than_a_handshake_line() {
        let long = "c".repeat(MAX_LINE_BYTES * 2);
        let mut wire = Vec::new();
        write_answer_async(&mut wire, &long)
            .await
            .expect("write an answer above the handshake bound");
        let error = read_line_async(&mut wire.as_slice())
            .await
            .expect_err("the handshake bound still applies to a handshake line");
        assert!(matches!(error, HandshakeError::LineTooLong), "{error:?}");

        let read = read_answer_async(&mut wire.as_slice())
            .await
            .expect("read the answer");
        let decoded: String = serde_json::from_slice(&read).expect("decode the answer");
        assert_eq!(decoded, long);
    }

    #[tokio::test]
    async fn an_answer_above_the_answer_bound_is_refused() {
        let error = write_answer_async(&mut Vec::new(), &"c".repeat(MAX_ANSWER_BYTES))
            .await
            .expect_err("above the answer bound");
        assert!(matches!(error, HandshakeError::LineTooLong), "{error:?}");
    }

    #[test]
    fn oversized_message_is_rejected_by_the_encoder() {
        let mut oversized = session();
        oversized.codebase = Some("c".repeat(MAX_LINE_BYTES));
        let error = encode(&Request::attach("0.2.0", oversized)).expect_err("too long");
        assert!(matches!(error, HandshakeError::LineTooLong), "{error:?}");
    }

    #[test]
    fn mismatched_protocol_is_rejected() {
        let line = br#"{"kind":"status","protocol":99}"#;
        let error = decode_request(line).expect_err("protocol mismatch");
        assert!(
            matches!(
                error,
                HandshakeError::ProtocolMismatch {
                    expected: PROTOCOL,
                    received: 99
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let line = br#"{"kind":"detach","protocol":1}"#;
        let error = decode_request(line).expect_err("unknown kind");
        assert!(
            matches!(&error, HandshakeError::UnknownKind(kind) if kind == "detach"),
            "{error:?}"
        );
    }

    #[test]
    fn a_response_kind_is_not_accepted_as_a_request() {
        let line = encode(&Response::attached("0.2.0", "s")).expect("encode attached");
        let error = decode_request(&line).expect_err("wrong direction");
        assert!(
            matches!(&error, HandshakeError::UnknownKind(kind) if kind == "attached"),
            "{error:?}"
        );
    }

    #[test]
    fn a_malformed_line_is_rejected() {
        let error = decode_request(b"not json").expect_err("malformed");
        assert!(matches!(error, HandshakeError::Malformed(_)), "{error:?}");
    }

    #[test]
    fn debug_of_an_attach_request_hides_the_token() {
        let rendered = format!("{:?}", Request::attach("0.2.0", session()));
        assert!(!rendered.contains("super-secret-value"), "{rendered}");
        assert!(rendered.contains("Token(redacted)"), "{rendered}");
    }

    #[test]
    fn debug_of_a_token_hides_the_secret() {
        let rendered = format!("{:?}", Token::new("super-secret-value"));
        assert_eq!(rendered, "Token(redacted)");
    }

    #[tokio::test]
    async fn a_written_line_reads_back_as_the_same_message() {
        let mut wire: Vec<u8> = Vec::new();
        write_line_async(&mut wire, &Response::attached("0.2.0", "s"))
            .await
            .expect("write attached");
        let mut source = wire.as_slice();
        let line = read_line_async(&mut source).await.expect("read attached");
        assert!(matches!(
            decode_response(&line),
            Ok(Response::Attached { .. })
        ));

        let mut wire: Vec<u8> = Vec::new();
        write_line_async(&mut wire, &Request::status())
            .await
            .expect("write status");
        let mut source = wire.as_slice();
        let line = read_line_async(&mut source).await.expect("read status");
        assert!(matches!(decode_request(&line), Ok(Request::Status { .. })));
    }

    #[tokio::test]
    async fn asynchronous_read_stops_at_the_newline_byte() {
        let mut source: &[u8] = b"{\"kind\":\"stop\",\"protocol\":1}\nleftover";
        let line = read_line_async(&mut source).await.expect("read stop");
        assert!(matches!(decode_request(&line), Ok(Request::Stop { .. })));
        assert_eq!(source, b"leftover");
    }

    #[tokio::test]
    async fn asynchronous_read_reports_a_closed_peer() {
        let mut source: &[u8] = b"partial";
        let error = read_line_async(&mut source).await.expect_err("closed");
        assert!(matches!(error, HandshakeError::Closed), "{error:?}");
    }

    #[test]
    fn an_empty_known_kind_list_rejects_every_line() {
        let error = decode::<Request>(br#"{"kind":"status","protocol":1}"#, &[])
            .expect_err("no kind is known");
        assert!(matches!(error, HandshakeError::UnknownKind(_)), "{error:?}");
    }
}
