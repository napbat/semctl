//! The daemon role: own the endpoint, serve one session per connection, and
//! exit when nothing needs this process any more.
//!
//! The loop is deliberately small. It accepts, it reaps finished session
//! tasks, and it watches three reasons to stop: an operating-system signal, a
//! `stop` control request, and an idle period. Everything a connection does
//! happens in its own task, so one slow session cannot hold up the accept
//! loop or another session.
//!
//! Ownership and cancellation:
//!
//! - Every connection is one task in one [`JoinSet`]. The task owns its
//!   [`Stream`], so ending the task closes that connection.
//! - Every task holds a connection guard, and an attached session holds a
//!   session guard as well. The guards are what the idle timer and the status
//!   line count, and a dropped guard is also correct for a task that was
//!   aborted.
//! - The drain aborts every task, which closes every connection. A connected
//!   client observes end of file, which is the same event as a daemon that
//!   went away, and its host reconnects if it wants another session.

use std::io;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use super::{VERSION, session, status};
use crate::engine::{Engine, EngineSettings};
use crate::ipc::handshake::{self, HandshakeError, PROTOCOL, Request, Response};
use crate::ipc::{Election, Endpoint, Listener, Stream};
use crate::session::PER_SESSION_VARS;

/// How long the daemon stays alive with no connection.
const IDLE_SECS_VAR: &str = "SEMCTX_DAEMON_IDLE_SECS";

/// Idle delay when `SEMCTX_DAEMON_IDLE_SECS` is unset or unreadable.
const DEFAULT_IDLE: Duration = Duration::from_secs(600);

/// Longest the drain waits for the session tasks it aborted.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Worker threads of the daemon runtime. Two keep a small machine serving
/// while one worker waits; eight bound the cost on a large one.
const WORKER_RANGE: RangeInclusive<usize> = 2..=8;

/// Blocking threads of the daemon runtime. Every tree scan, policy load, and
/// watch registration runs on one of these.
const MAX_BLOCKING_THREADS: usize = 64;

/// Run the daemon role, including its own runtime.
///
/// `main` calls this before it builds any other runtime: this runtime is
/// bounded on purpose, because one daemon serves every session of this user
/// and must not size itself as if it served one.
pub(crate) fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads(std::thread::available_parallelism().ok()))
        .max_blocking_threads(MAX_BLOCKING_THREADS)
        .enable_all()
        .build()
        .context("build the daemon runtime")?;
    runtime.block_on(serve())
}

/// How many worker threads the daemon runtime gets.
fn worker_threads(parallelism: Option<std::num::NonZero<usize>>) -> usize {
    parallelism
        .map_or(*WORKER_RANGE.start(), std::num::NonZero::get)
        .clamp(*WORKER_RANGE.start(), *WORKER_RANGE.end())
}

/// Serve the local endpoint until this daemon drains.
///
/// A lost election is a normal outcome and not an error: another daemon
/// already serves this endpoint, so this process has nothing to do and exits
/// with status 0.
///
/// Call this inside a Tokio runtime. [`run`] builds the daemon's own; the
/// command dispatcher in [`crate::commands::daemon`] reaches this function
/// when a runtime already exists.
pub(crate) async fn serve() -> Result<()> {
    let endpoint = Endpoint::current().context("locate the local daemon endpoint")?;
    let listener = match Listener::bind(&endpoint).context("bind the local daemon endpoint")? {
        Election::Lost => {
            info!(
                endpoint = endpoint.id(),
                "another semctl daemon already serves this endpoint; exiting"
            );
            return Ok(());
        }
        Election::Won(listener) => listener,
    };
    warn_about_session_environment();

    let idle_after = idle_after(std::env::var(IDLE_SECS_VAR).ok().as_deref());
    let daemon = Arc::new(Daemon {
        engine: Engine::new(EngineSettings::from_environment()),
        sessions: Arc::new(Sessions::new()),
        stop: Notify::new(),
        started: Instant::now(),
        pid: std::process::id(),
    });
    info!(
        endpoint = endpoint.id(),
        pid = daemon.pid,
        version = VERSION,
        idle_secs = idle_after.as_secs(),
        "semctl daemon serves the local endpoint"
    );

    let mut tasks = JoinSet::new();
    let reason = accept_loop(&listener, &daemon, &mut tasks, idle_after).await;
    drain(listener, tasks, daemon, reason).await;
    Ok(())
}

/// Warn once about a per-session variable in the daemon's own environment.
///
/// A client removes these before it spawns a daemon, so one that is present
/// came from a daemon started by hand. The daemon never reads them: every
/// session is described by its own attach body.
fn warn_about_session_environment() {
    for name in PER_SESSION_VARS {
        if std::env::var_os(name).is_some() {
            warn!(
                variable = name,
                "this daemon ignores the per-session variable in its own environment; \
                 every session is described by its attach request"
            );
        }
    }
}

/// The idle delay. An absent or unreadable value keeps the default, which is
/// the rule every other environment key in this program follows.
///
/// `0` means "exit as soon as nothing is connected", which is what a test that
/// measures the idle exit asks for.
fn idle_after(raw: Option<&str>) -> Duration {
    raw.and_then(|value| value.trim().parse().ok())
        .map_or(DEFAULT_IDLE, Duration::from_secs)
}

/// Everything one daemon shares across its connections.
pub(super) struct Daemon {
    /// The shared engine. Every session holds a clone, so a second session on
    /// one checkout adds a lease and nothing else.
    engine: Arc<Engine>,
    /// What the idle timer and the status line count.
    sessions: Arc<Sessions>,
    /// Notified by a `stop` control request, after its answer is on the wire.
    stop: Notify,
    /// When this daemon won its election.
    started: Instant,
    /// This daemon's process id. It names every session of this daemon.
    pid: u32,
}

impl Daemon {
    pub(super) fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    pub(super) fn sessions(&self) -> &Arc<Sessions> {
        &self.sessions
    }

    /// Seconds since this daemon won its election.
    pub(super) fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    pub(super) fn pid(&self) -> u32 {
        self.pid
    }
}

/// Why the accept loop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainReason {
    /// The operating system asked this process to stop.
    Signal,
    /// A client sent a `stop` control request.
    Stop,
    /// Nothing was connected for the idle delay.
    Idle,
    /// The endpoint stopped accepting connections.
    ListenerFailed,
}

/// Accept connections until something asks this daemon to stop.
async fn accept_loop(
    listener: &Listener,
    daemon: &Arc<Daemon>,
    tasks: &mut JoinSet<()>,
    idle_after: Duration,
) -> DrainReason {
    // Built once and polled across every iteration: the signal handler must be
    // installed before the first connection, and a signal that arrives while
    // another branch is running must not be lost.
    let mut shutdown = std::pin::pin!(shutdown_signal());
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    // Reap what finished since the last connection, so a
                    // long-lived daemon keeps no entry per session it served.
                    // Reaping here rather than in its own branch keeps one
                    // mutable borrow of the set in one place.
                    while let Some(finished) = tasks.try_join_next() {
                        if let Err(error) = finished
                            && !error.is_cancelled()
                        {
                            warn!(%error, "a session task ended unexpectedly");
                        }
                    }
                    // The guard is taken here, not in the task: a connection
                    // that is accepted but not yet running must already hold
                    // the daemon open.
                    let connection = Sessions::connect(daemon.sessions.clone());
                    let daemon = daemon.clone();
                    tasks.spawn(handle(daemon, stream, connection));
                }
                Err(error) => {
                    warn!(error = format!("{error:#}"), "the local endpoint stopped accepting");
                    return DrainReason::ListenerFailed;
                }
            },
            () = shutdown.as_mut() => return DrainReason::Signal,
            () = daemon.stop.notified() => return DrainReason::Stop,
            () = daemon.sessions.wait_idle_for(idle_after) => return DrainReason::Idle,
        }
    }
}

/// Resolve when the operating system asks this daemon to stop.
///
/// Unix listens for `SIGTERM` as well as the console interrupt, because a
/// service manager and `semctl daemon stop`'s neighbors both use it. Windows
/// has no `SIGTERM`; its console control events arrive through `ctrl_c`.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    outcome = tokio::signal::ctrl_c() => await_interrupt(outcome).await,
                    _ = terminate.recv() => {}
                }
                return;
            }
            Err(error) => warn!(
                %error,
                "cannot listen for SIGTERM; this daemon still stops on an interrupt, \
                 on a stop request, and when idle"
            ),
        }
    }
    await_interrupt(tokio::signal::ctrl_c().await).await;
}

/// Treat a failed handler installation as "no interrupt will arrive".
///
/// Returning would look like a signal and drain a healthy daemon at once. The
/// other two stop reasons still work.
async fn await_interrupt(outcome: io::Result<()>) {
    if let Err(error) = outcome {
        warn!(
            %error,
            "cannot listen for the interrupt signal; this daemon still stops on a \
             stop request and when idle"
        );
        std::future::pending::<()>().await;
    }
}

/// Stop serving, in this order:
///
/// 1. Drop the listener. The daemon stops accepting, and on Unix the socket
///    file goes with it, so the next client starts a new daemon instead of
///    connecting to one that is leaving.
/// 2. Abort every connection task. Each task owns its connection, so every
///    connected client observes end of file.
/// 3. Release the engine, which cancels every coordinator, releases every
///    watch registration, and drops the shared transport.
///
/// The whole drain is bounded by [`DRAIN_TIMEOUT`]. A session that cannot be
/// aborted inside it — one inside blocking work, which is not a cancellation
/// point — keeps its handle on the engine, and process exit releases the rest.
async fn drain(
    listener: Listener,
    mut tasks: JoinSet<()>,
    daemon: Arc<Daemon>,
    reason: DrainReason,
) {
    info!(
        ?reason,
        sessions = daemon.sessions.session_count(),
        "semctl daemon is draining"
    );
    drop(listener);
    tasks.abort_all();
    let joined = tokio::time::timeout(DRAIN_TIMEOUT, async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    if joined.is_err() {
        warn!("a session did not end inside the drain timeout; exiting anyway");
    }
    let checkouts = daemon.engine.registry().len();
    // The last strong handle, unless a task outlived the timeout above.
    drop(daemon);
    info!(?reason, checkouts, "semctl daemon stopped");
}

/// Read one handshake line from a new connection and do what it asks.
///
/// A connection whose first line is absent, too long, malformed, or late is
/// closed without an answer beyond the protocol case below. That is a probe or
/// a broken client, not an event a person needs to see, so it is logged at
/// debug level.
async fn handle(daemon: Arc<Daemon>, mut stream: Stream, connection: ConnectionGuard) {
    // `read_line_async` bounds the read with the handshake exchange timeout.
    let line = match handshake::read_line_async(&mut stream).await {
        Ok(line) => line,
        Err(error) => {
            debug!(%error, "closing a connection that sent no usable handshake");
            return;
        }
    };
    let request = match handshake::decode_request(&line) {
        Ok(request) => request,
        // A peer that speaks another wire version deserves a reason: its
        // endpoint name matched, so the mismatch is worth reporting.
        Err(error @ HandshakeError::ProtocolMismatch { .. }) => {
            reject(&mut stream, &error.to_string()).await;
            return;
        }
        Err(error) => {
            debug!(%error, "closing a connection with an unusable handshake");
            return;
        }
    };
    match request {
        Request::Attach {
            protocol,
            version,
            session,
        } => {
            if let Some(reason) = version_mismatch(protocol, &version) {
                reject(&mut stream, &reason).await;
                return;
            }
            session::serve(&daemon, stream, *session, connection).await;
        }
        Request::Status { .. } => answer_status(&daemon, &mut stream).await,
        Request::Stop { .. } => answer_stop(&daemon, &mut stream).await,
    }
}

/// Why this daemon cannot serve a client of another build.
///
/// The endpoint identity already carries the version, so a mismatch here means
/// the client reached this endpoint some other way. One process cannot serve
/// two protocol versions, so it says so instead of guessing.
fn version_mismatch(protocol: u32, version: &str) -> Option<String> {
    if protocol != PROTOCOL {
        return Some(format!(
            "this daemon speaks handshake protocol {PROTOCOL}, not {protocol}"
        ));
    }
    if version != VERSION {
        return Some(format!(
            "this daemon is semctl {VERSION}; a semctl {version} client needs its own daemon"
        ));
    }
    None
}

/// Refuse a connection with a reason the client can print.
///
/// Best effort: a client that already went away cannot be told anything, and
/// the connection closes either way.
async fn reject(stream: &mut Stream, reason: &str) {
    debug!(reason, "refusing a connection");
    if let Err(error) =
        handshake::write_line_async(stream, &Response::rejected(VERSION, reason)).await
    {
        debug!(%error, "could not deliver the refusal");
    }
}

/// Answer one `status` request with one JSON line, then close.
async fn answer_status(daemon: &Arc<Daemon>, stream: &mut Stream) {
    let report = status::snapshot(daemon).await;
    if let Err(error) = handshake::write_line_async(stream, &report).await {
        debug!(%error, "could not deliver the status line");
    }
}

/// Answer one `stop` request, then start the drain.
///
/// The answer goes out first. The drain aborts this task, so a client that was
/// told nothing would see only end of file and could not report the daemon it
/// stopped.
async fn answer_stop(daemon: &Arc<Daemon>, stream: &mut Stream) {
    let ack = status::StopAck {
        pid: daemon.pid,
        version: VERSION.to_string(),
    };
    if let Err(error) = handshake::write_line_async(stream, &ack).await {
        debug!(%error, "could not acknowledge the stop request");
    }
    info!("a client asked this daemon to stop");
    daemon.stop.notify_one();
}

/// What one daemon has open, and how a waiter learns that it changed.
///
/// Two counters, because they answer two different questions. `sessions` is
/// what a person asked for: how many MCP sessions this daemon serves.
/// `connections` includes a connection that is still in its handshake and a
/// control request that is still being answered, so the idle timer cannot cut
/// off a client that is attaching right now.
pub(super) struct Sessions {
    attached: AtomicUsize,
    connections: AtomicUsize,
    /// Session ids issued so far. It only ever grows, so no two sessions of
    /// one daemon share an id.
    issued: AtomicU64,
    /// Notified after every change to either counter.
    changed: Notify,
}

impl Sessions {
    fn new() -> Self {
        Self {
            attached: AtomicUsize::new(0),
            connections: AtomicUsize::new(0),
            issued: AtomicU64::new(0),
            changed: Notify::new(),
        }
    }

    /// How many MCP sessions this daemon serves.
    pub(super) fn session_count(&self) -> usize {
        self.attached.load(Ordering::Acquire)
    }

    /// Count one open connection until the guard drops.
    fn connect(sessions: Arc<Self>) -> ConnectionGuard {
        sessions.connections.fetch_add(1, Ordering::AcqRel);
        sessions.changed.notify_waiters();
        ConnectionGuard { sessions }
    }

    /// Count one attached session until the guard drops, and name it.
    ///
    /// The id is `{pid}-{counter}`: the pid tells two daemons apart in a log,
    /// and the counter tells two sessions of one daemon apart.
    pub(super) fn attach(sessions: Arc<Self>, pid: u32) -> SessionGuard {
        let ordinal = sessions.issued.fetch_add(1, Ordering::AcqRel) + 1;
        sessions.attached.fetch_add(1, Ordering::AcqRel);
        sessions.changed.notify_waiters();
        SessionGuard {
            id: format!("{pid}-{ordinal}"),
            sessions,
        }
    }

    /// Resolve once nothing has been connected for `idle`.
    ///
    /// The wait watches `connections`, so an attaching client keeps the daemon
    /// alive before it is a session. `idle` of zero resolves as soon as
    /// nothing is connected.
    ///
    /// The caller polls this inside its accept loop, so every accepted
    /// connection and every reaped task restarts the wait. That is the
    /// intended reading of "idle": a daemon that is being used does not exit.
    async fn wait_idle_for(&self, idle: Duration) {
        loop {
            let changed = self.changed.notified();
            let mut changed = std::pin::pin!(changed);
            // Register before the counter is read. A connection that arrives
            // between the read and the wait would otherwise be missed.
            changed.as_mut().enable();
            if self.connections.load(Ordering::Acquire) == 0 {
                tokio::select! {
                    () = tokio::time::sleep(idle) => {
                        if self.connections.load(Ordering::Acquire) == 0 {
                            return;
                        }
                    }
                    () = changed.as_mut() => {}
                }
            } else {
                changed.as_mut().await;
            }
        }
    }

    /// Record that a counter changed, so the idle wait starts again.
    fn changed(&self) {
        self.changed.notify_waiters();
    }
}

/// One open connection, counted for the idle timer.
pub(super) struct ConnectionGuard {
    sessions: Arc<Sessions>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.sessions.connections.fetch_sub(1, Ordering::AcqRel);
        self.sessions.changed();
    }
}

/// One attached MCP session, counted for the status line.
pub(super) struct SessionGuard {
    id: String,
    sessions: Arc<Sessions>,
}

impl SessionGuard {
    /// The opaque identifier this session is named by.
    pub(super) fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.sessions.attached.fetch_sub(1, Ordering::AcqRel);
        self.sessions.changed();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{
        DEFAULT_IDLE, PROTOCOL, Sessions, VERSION, idle_after, version_mismatch, worker_threads,
    };

    #[test]
    fn the_idle_delay_falls_back_to_the_default() {
        assert_eq!(idle_after(Some("30")), Duration::from_secs(30));
        assert_eq!(idle_after(Some("  30\n")), Duration::from_secs(30));
        assert_eq!(idle_after(Some("0")), Duration::ZERO);
        assert_eq!(idle_after(None), DEFAULT_IDLE);
        assert_eq!(idle_after(Some("")), DEFAULT_IDLE);
        assert_eq!(idle_after(Some("soon")), DEFAULT_IDLE);
        assert_eq!(idle_after(Some("-5")), DEFAULT_IDLE);
    }

    #[test]
    fn the_worker_count_stays_inside_the_documented_bounds() {
        assert_eq!(worker_threads(None), 2, "an unknown machine still serves");
        assert_eq!(worker_threads(std::num::NonZero::new(1)), 2);
        assert_eq!(worker_threads(std::num::NonZero::new(4)), 4);
        assert_eq!(worker_threads(std::num::NonZero::new(64)), 8);
    }

    #[test]
    fn a_client_of_this_build_is_accepted() {
        assert_eq!(version_mismatch(PROTOCOL, VERSION), None);
    }

    #[test]
    fn another_protocol_or_version_is_refused_with_a_reason() {
        let protocol = version_mismatch(PROTOCOL + 1, VERSION).expect("a protocol mismatch");
        assert!(protocol.contains("handshake protocol"), "{protocol}");
        let version = version_mismatch(PROTOCOL, "0.0.1").expect("a version mismatch");
        assert!(version.contains("0.0.1"), "{version}");
        assert!(version.contains(VERSION), "{version}");
    }

    #[test]
    fn session_ids_carry_the_daemon_pid_and_never_repeat() {
        let sessions = Arc::new(Sessions::new());
        let first = Sessions::attach(sessions.clone(), 42);
        let second = Sessions::attach(sessions.clone(), 42);

        assert_eq!(first.id(), "42-1");
        assert_eq!(second.id(), "42-2");
        assert_eq!(sessions.session_count(), 2);
    }

    #[test]
    fn a_dropped_guard_leaves_no_session_behind() {
        let sessions = Arc::new(Sessions::new());
        {
            let _session = Sessions::attach(sessions.clone(), 1);
            let _connection = Sessions::connect(sessions.clone());
            assert_eq!(sessions.session_count(), 1);
        }
        assert_eq!(sessions.session_count(), 0);
    }

    /// The idle wait must not fire while a connection is still open, and it
    /// must fire once the last one is gone.
    #[tokio::test(start_paused = true)]
    async fn the_idle_wait_ends_only_after_the_last_connection() {
        let sessions = Arc::new(Sessions::new());
        let connection = Sessions::connect(sessions.clone());

        assert!(
            tokio::time::timeout(
                Duration::from_secs(60),
                sessions.wait_idle_for(Duration::from_secs(5))
            )
            .await
            .is_err(),
            "an open connection must keep the daemon alive"
        );

        drop(connection);
        assert!(
            tokio::time::timeout(
                Duration::from_secs(60),
                sessions.wait_idle_for(Duration::from_secs(5))
            )
            .await
            .is_ok(),
            "the idle wait must end after the last connection"
        );
    }

    /// A connection that arrives during the idle delay restarts it.
    #[tokio::test(start_paused = true)]
    async fn a_new_connection_restarts_the_idle_delay() {
        let sessions = Arc::new(Sessions::new());
        let waiting = tokio::spawn({
            let sessions = sessions.clone();
            async move { sessions.wait_idle_for(Duration::from_secs(5)).await }
        });

        tokio::time::sleep(Duration::from_secs(1)).await;
        let connection = Sessions::connect(sessions.clone());
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(
            !waiting.is_finished(),
            "the wait must restart when something connects"
        );

        drop(connection);
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(
            waiting.is_finished(),
            "the wait must end after the connection closes"
        );
    }
}
