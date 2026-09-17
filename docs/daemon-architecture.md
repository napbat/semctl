# Shared local daemon architecture

Status: built on the `v0.2` branch, in the stages at the end of this document.
This document describes the design as it was built. It is not a change log.
The feasibility study that motivates the design is
[reports/shared-daemon-feasibility.md](../reports/shared-daemon-feasibility.md).
The measurements that decide the default are
[reports/shared-daemon-results.md](../reports/shared-daemon-results.md).

## Goals

- One local semctl process serves every MCP connection for one operating-system
  user and one configuration directory.
- One watcher, one reconcile queue, one content cache, and one HTTP transport per
  checkout, independent of how many connections use that checkout.
- The same engine serves the standalone `semctl mcp` process. Standalone mode is
  the daemon engine with exactly one in-process session over stdio.
- Scale target: 1,000 concurrent client sessions and 1,000 distinct checkouts in
  one daemon, and every mix between "1,000 sessions on one checkout" and "one
  session per checkout".
- No behavior change at the MCP tool boundary. Tool names, arguments, responses,
  and host approval flows stay as they are.

## Process model

```text
host ──stdio──▶ semctl mcp (byte pump, one current-thread runtime) ──local socket / named pipe──▶ semctl daemon
                                                                                  ├── Session (one per connection): McpServer over the stream
                                                                                  ├── Engine (shared): CheckoutRegistry, WatchHub, Scheduler, HttpTransport
                                                                                  └── CheckoutCoordinator (one per checkout key): watcher lease, reconcile queue, cache, jobs, readiness
```

Three roles share one binary:

| Role | Entry | Runtime | Responsibility |
| --- | --- | --- | --- |
| Client | `semctl mcp` with daemon mode enabled | One current-thread Tokio runtime. No worker pool. | Attach handshake, then copy bytes between stdio and the daemon connection. Start a daemon when none is running. |
| Daemon | `semctl daemon run` (hidden) | Multi-thread Tokio, bounded worker count | Accept connections, build one `Session` per connection, own the shared `Engine`, exit when idle. |
| Standalone | `semctl mcp` with daemon mode off | Multi-thread Tokio, as today | Build one `Session` from the process environment and serve it over stdio with the same `Engine` type. |

The client must decide its role before any Tokio runtime exists. `main` parses
the command line first and builds no runtime itself. Each role then builds the
runtime it needs.

The client role builds one current-thread runtime rather than no runtime at
all. A Windows named pipe must be opened for overlapped input and output,
because Windows serializes the operations on one file object opened for
synchronous input and output. Two blocking threads on one such pipe would
deadlock: the read of the answer would hold back the write of the request.
Tokio's named pipe types already open the pipe for overlapped operation, so the
pump is asynchronous on both platforms and one thread drives it. The client
still owns no engine, no watcher, and no worker pool.

## Module map

New modules. Every hand-written file stays near or below 1,000 lines. Split by
ownership when a module grows.

```text
src/session/
  mod.rs          SessionContext: validated per-connection invocation context.
  credentials.rs  CredentialSource: invocation token or stored login, never the process environment.
src/engine/
  mod.rs          Engine: owns CheckoutRegistry, WatchHub, Scheduler, HttpTransport, update note. One per process.
  registry.rs     CheckoutRegistry: CheckoutKey -> Arc<CheckoutCoordinator>, leases, idle release, gates by root.
  coordinator.rs  CheckoutCoordinator: reconcile queue, trigger coalescing, cache, last job, readiness gate, watch lease.
  scheduler.rs    Scheduler: bounded permits for scans, uploads, and remote requests.
  watch_hub.rs    WatchHub: one notify debouncer for all roots; routes events to coordinators by root.
src/ipc/
  mod.rs          Endpoint naming, runtime directory, Listener/Stream enums, connect with retry.
  handshake.rs    Attach and control wire types, protocol version, size and time limits.
  unix.rs         Unix domain socket listener, lock-file election, stale socket cleanup, peer uid check.
  windows.rs      Named pipe server and client options, first-instance election, security descriptor, QoS.
  pump.rs         Asynchronous byte pump used by the client role.
src/daemon/
  mod.rs          Role selection: daemon, client, or standalone.
  client.rs       The client role: daemon mode, attach, fallback policy.
  control.rs      The `semctl daemon status` and `semctl daemon stop` client.
  serve.rs        Listener loop, session accept, idle timer, drain, signals.
  session.rs      Handshake -> SessionContext -> McpServer::serve over the stream.
  spawn.rs        Detached daemon launch from a client, election retry.
  status.rs       The status shape both readers share.
```

Existing modules change as follows:

- `src/mcp/mod.rs`: `McpServer` takes `(SessionContext, Arc<Engine>)`. `Shared`
  keeps the session context, the base client, the launch directory, the pinned
  flag, the bound client, the freshness cache, one handle to the shared engine,
  and the coordinator leases this session holds. Everything else moves to the
  engine: `jobs`, `watched`, and `initial_indexes`.
- `src/sync/background.rs` and `src/sync/watcher.rs`: their lifecycle logic moves
  into `CheckoutCoordinator` and `WatchHub`. The sync engine in `src/sync/mod.rs`
  stays the single reconcile implementation.
- `src/client/mod.rs`: `Client` receives a shared `reqwest::Client` from
  `HttpTransport` and a `CredentialSource`. It never reads `SEMCTX_TOKEN` itself.
- `src/auth/session.rs`: token functions take a `CredentialSource` instead of
  reading the environment.
- `src/main.rs`: selects the role before building a runtime.

## Session context

`SessionContext` is the only way invocation context enters the engine. It is
built from the process environment in standalone mode and from the attach
handshake in daemon mode. Fields:

| Field | Source | Notes |
| --- | --- | --- |
| `cwd` | Client working directory | Absolute. The daemon never calls `std::env::current_dir()` or `set_current_dir()` in a session path. Relative selectors resolve against this field. |
| `server` | `--server` / `SEMCTX_SERVER` | Optional override. Config file default applies when absent. |
| `tenant` | `--tenant` / `SEMCTX_TENANT` | Optional override. |
| `codebase` | `--codebase` / `SEMCTX_CODEBASE` | Optional pin. |
| `credentials` | `SEMCTX_TOKEN` or stored login | `CredentialSource::Invocation(Secret)` or `CredentialSource::Stored`. |
| `resync_secs` | `SEMCTX_MCP_RESYNC_SECS` | Optional. Applies to coordinators this session creates. A coordinator keeps the first value it was created with. |
| `update_check` | `SEMCTX_MCP_UPDATE_CHECK` | The daemon runs one update check per process. A session with `update_check = false` does not receive the note. |

The configuration directory is process-level, not session-level. A client with a
different `XDG_CONFIG_HOME` computes a different endpoint name and therefore
attaches to a different daemon. See "Endpoint identity".

Rules:

- The daemon process must not use its own environment for any per-session value.
  The client removes `SEMCTX_TOKEN`, `SEMCTX_SERVER`, `SEMCTX_TENANT`,
  `SEMCTX_CODEBASE`, `SEMCTX_MCP_RESYNC_SECS`, and `SEMCTX_MCP_UPDATE_CHECK` from
  the environment when it spawns a daemon.
- Secrets travel only inside the handshake body over the protected connection.
  They never appear in endpoint names, arguments, logs, or status output.
- `CredentialSource::Stored` reads the credential file through the existing
  locked store on every token fetch, so a login or logout performed while the
  daemon runs is honored by the next request.

## Engine

### CheckoutKey

A coordinator is keyed by the tuple that determines which uploads and reads are
interchangeable:

```text
CheckoutKey {
    server_url: normalized,
    tenant: Option<String>,
    credential_scope: CredentialScope,   // Stored, or Invocation(blake3(token))
    root: canonical working-copy root,
}
```

The codebase id is validated binding state on the coordinator, not part of the
key. The server can move a checkout to another codebase after a remote change.
Two sessions with different credential scopes on the same root get two
coordinators. That duplicates a watcher, but never mixes authorization.

### CheckoutRegistry

- `attach(key, client, root) -> Result<CoordinatorLease>`: returns the existing
  coordinator or creates one. Creation registers the root with the `WatchHub`,
  starts the reconcile task, and queues a startup reconcile.
- `attach_first_index(key, client, root) -> Result<(CoordinatorLease, Arc<InitialIndexGate>)>`:
  the `index_codebase` path. Creates the readiness gate before registration and
  hands the startup reconcile's completion to the gate.
- `gate_for_root(root) -> Option<Arc<InitialIndexGate>>` and
  `coordinator_for_root(root)` for status and readiness lookups.
- Lease drop decrements the coordinator's lease count. A coordinator with zero
  leases stays alive for `idle_grace` (default 300 seconds), then the registry
  sweeper cancels its reconcile task, releases its watch registration, and
  removes it. A new attach during the grace period reuses it.
- A failed startup reconcile does not remove the coordinator. The next trigger
  retries. The `InitialIndexGate` records the failure for `index_codebase`
  callers, and a later successful `index_codebase` call creates a fresh gate.

### CheckoutCoordinator

- Owns `Mutex<SyncCache>`, `last_job: Option<LastJob>`, `gate: Option<Arc<InitialIndexGate>>`,
  its watch state (a `WatchRegistration` or the reason it has none), the trigger
  channel with its overflow flag, and the reconcile task handle.
- Triggers: `Startup`, `Watch`, `Periodic`, and `Explicit`. There is no separate
  edit trigger. An applied edit raises `Explicit`, because an edit and a tool
  that asks for a sync want the same thing: one reconcile, now. All triggers
  go through one bounded channel into one task. The task drains the channel
  before each run, so a burst of triggers produces one reconcile. A trigger that
  arrives during a run schedules exactly one follow-up run.
- Every reconcile acquires a scan permit from the `Scheduler` before calling
  `sync::sync`. Upload concurrency inside `sync` acquires upload permits.
- Periodic backstop: the coordinator owns its own timer. `resync_secs` comes
  from the session that created the coordinator and defaults to 60 seconds. The
  interval is `resync_secs` when the watcher is unavailable and
  `5 * resync_secs` when the watcher is active: an active watcher reports real
  edits inside its debounce window, so the timer is only a backstop against
  events the platform dropped, and running it five times less often is what
  makes 1,000 watched checkouts affordable. The first tick is offset by a
  fraction of the interval derived from the root path, so 1,000 coordinators do
  not tick together. `resync_secs = 0` disables the timer and leaves the
  startup run and explicit triggers.
- Cancellation: dropping the coordinator aborts the task. A running scan observes
  cancellation through the existing `sync::blocking::Cancellation`.
- The coordinator exposes `status() -> CoordinatorStatus` for `sync_status` and
  daemon status: root, codebase id, lease count, watcher state, last job, queue
  state, last reconcile outcome, and last error.

### WatchHub

- One `notify_debouncer_full` instance per process, created lazily. On Linux this
  means one inotify instance for all roots instead of one per root.
- `register(root) -> Result<WatchRegistration>` adds the root recursively and
  records the coordinator's sender. Root watches and external policy sources
  such as the global gitignore are both reference counted, so two registrations
  on one root share one platform watch and the hub releases it with the last of
  them. Two registrations on one root are normal: two sessions can attach one
  checkout at the same time, and two credential scopes need two coordinators
  for it. Events are routed to every coordinator that observes them.
- The debouncer callback does no filesystem I/O. It groups event paths by
  registered root (every registered root that contains the path receives the
  batch) and calls `try_send` on that coordinator's bounded channel. When the
  channel is full the hub sets the coordinator's overflow flag, which forces the
  next run to treat the batch as "something changed".
- Policy filtering (`SourcePolicy::load`, `event_is_relevant`,
  `event_may_affect_policy`) runs inside the coordinator task, on the blocking
  pool, not on the notify thread.
- A root whose registration fails leaves the coordinator in "watcher unavailable"
  mode with the short periodic interval, as today. The failure reason is kept for
  status output.
- Dropping a `WatchRegistration` unwatches the root. The hub drops the debouncer
  when the last registration is gone.

### Scheduler

| Permit | Default | Purpose |
| --- | --- | --- |
| `scan` | `clamp(available_parallelism / 2, 2, 8)` | Concurrent full-tree scans across all coordinators. |
| `upload` | 8 | Concurrent upload requests across all coordinators. Each sync keeps its own limit of 4. |
| `remote` | 64 | Concurrent interactive remote requests from tool calls. Status and cancellation paths do not take this permit. |

Defaults can be overridden by `SEMCTX_DAEMON_SCAN_PERMITS`,
`SEMCTX_DAEMON_UPLOAD_PERMITS`, and `SEMCTX_DAEMON_REMOTE_PERMITS`. Values are
clamped to `1..=1024`.

### Readiness and jobs

- `InitialIndexGate` semantics are unchanged. Gates live in the registry keyed
  by canonical root.
- Readiness is scoped by the session's leases. The lease set in `Shared` is the
  session's readiness scope: a retrieval tool with an empty cross-codebase
  selector waits for the first-index gates of the checkouts this session holds
  leases on and of no others. It never waits on another session's first index.
  `sync_status` bypasses the gates on purpose, so progress stays observable
  while a first index runs.
- `sync_status` reads the coordinator's `last_job` and watcher state. A pinned
  session without a local root reports the last job of any leased coordinator
  bound to that codebase id.

### HttpTransport

One `reqwest::Client` per process with the existing user agent and default pool
settings. `Client` instances share it. Tenant repair state and the capability
cache remain per `Client` family, which means per session. That bounds their
lifetime to the session.

## IPC

### Endpoint identity

```text
identity = blake3(config_dir_canonical ++ 0x00 ++ CARGO_PKG_VERSION ++ 0x00 ++ platform_user)
```

- Unix: `platform_user` is the numeric uid.
- Windows: `platform_user` is the user's SID string.
- The hex prefix of 16 characters is the endpoint name suffix.

Including the version makes a mixed-version fleet safe by construction. Old
clients keep attaching to the old daemon. New clients start a new daemon. The
old daemon exits when it becomes idle.

### Runtime directory and names

| Platform | Directory | Endpoint |
| --- | --- | --- |
| Linux | `$XDG_RUNTIME_DIR/semctl` when set, else `/tmp/semctl-<uid>` | `<dir>/<id>.sock`, `<dir>/<id>.lock`, `<dir>/<id>.log` |
| macOS | `$TMPDIR/semctl` when set, else `/tmp/semctl-<uid>` | same |
| Windows | none | `\\.\pipe\semctl-<id>`, log at `%LOCALAPPDATA%\semctl\daemon-<id>.log` |

Unix rules:

- Create the directory with mode `0700`. Verify it is owned by the current uid
  and has no group or other permission bits before use. Fail closed otherwise.
- The socket path must be at most 100 bytes. When the preferred directory makes
  it longer, use `/tmp/semctl-<uid>`.
- Do not use the Linux abstract socket namespace.
- The daemon verifies each accepted peer's uid with `peer_cred` and rejects a
  mismatch before reading the handshake.

Windows rules:

- Create the server with `first_pipe_instance(true)`, `reject_remote_clients(true)`,
  byte mode, and a security descriptor that grants access only to the current
  user's SID. A creation failure with access denied means another daemon owns
  the name.
- Pre-create a small pool of listening instances (4) and always create the next
  instance before awaiting the current connect.
- The client opens with `security_qos_flags(SECURITY_IDENTIFICATION)` so the
  server cannot impersonate the client. It retries on `ERROR_PIPE_BUSY` and
  `ERROR_FILE_NOT_FOUND` with backoff.
- `windows-sys` gains the `Win32_Security` and `Win32_Security_Authorization`
  features for SID lookup and descriptor construction.

### Election

- Unix: the daemon opens `<id>.lock` and calls `try_lock`. On `WouldBlock` it
  exits with status 0 because another daemon owns the endpoint. On success it
  removes a stale socket file, binds, and keeps the lock for its lifetime.
- Windows: the first-instance flag is the election.
- Clients never elect. A client that cannot connect spawns a daemon and retries
  the connection with backoff for up to 10 seconds. Several clients may spawn
  several daemon processes at once. All but one exit immediately.

### Spawn

The client spawns `current_exe daemon run` detached:

- Unix: `process_group(0)`, stdin from null, stdout to null, stderr to the log
  file opened in append mode.
- Windows: `creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)`;
  retry without `CREATE_BREAKAWAY_FROM_JOB` when the job forbids breakaway.
- The per-session environment variables listed under "Session context" are
  removed from the child environment.

### Handshake

Line-delimited JSON, UTF-8, one line per message, 5 second timeout for the
exchange. A line that arrives from a peer is at most 64 KiB, because that bound
decides how much an untrusted connection can make the reader allocate. A
control answer carries one entry per checkout, so it takes its own bound of
8 MiB.

Client to daemon:

```json
{"kind":"attach","protocol":1,"version":"0.2.0","session":{"cwd":"/abs/path","server":null,"tenant":null,"codebase":null,"token":null,"resync_secs":null,"update_check":true}}
```

Daemon to client:

```json
{"kind":"attached","protocol":1,"version":"0.2.0","session_id":"..."}
{"kind":"rejected","protocol":1,"version":"0.2.0","reason":"..."}
```

After `attached`, the connection carries raw MCP JSON-RPC lines in both
directions. The client copies bytes and never parses them.

Control requests use the same first line:

```json
{"kind":"status","protocol":1}
{"kind":"stop","protocol":1}
```

The daemon answers with one JSON line and closes the connection. `status`
reports version, pid, uptime, session count, per-coordinator status, scheduler
permit usage, and watch hub state. `stop` stops accepting, cancels sessions,
and exits.

### Client byte pump

Two tasks on the client's current-thread runtime, so neither direction can
block the other:

- Outbound task: stdin to connection. On stdin end of file, Unix shuts down the
  write half and Windows closes the connection.
- Inbound task: connection to stdout. On end of file or error the pump returns.
  Exit status is 0 after a clean daemon close and 1 after a transport error.
- The pump returns on the inbound report, then waits at most 2 seconds for the
  outbound task, which is usually still waiting for standard input.
- Writes to a closed stdout end the process with status 0.

### Daemon mode selection

`SEMCTX_MCP_DAEMON` on the client:

| Value | Behavior |
| --- | --- |
| `off` | Standalone role. |
| `auto` (default) | Attach, spawn when needed. On failure log one warning to stderr and fall back to standalone. |
| `require` | Attach, spawn when needed. On failure exit with status 1 and a clear error. |

An absent, empty, or unknown value means the default. An unknown value also
raises one warning. The default is `auto` because every line of the measurement
gate passed; see the results report.

### Daemon lifecycle

- Idle exit after `SEMCTX_DAEMON_IDLE_SECS` (default 600) with no session. The
  rule is "no session for N seconds". The deadline is an absolute instant taken
  when the last session ended, so a client that only connects cannot postpone
  it: `semctl daemon status` in a loop must not keep an unused daemon alive. An
  open connection defers the decision only at the instant the deadline passes,
  so a client that is attaching right then is not cut off. A value of `0` means
  "exit as soon as the last session ends", which is what a test that measures
  the idle exit asks for.
- `SIGTERM`, `SIGINT`, and Windows console control events start a drain: stop
  accepting, cancel sessions, release coordinators, exit.
- Runtime: multi-thread Tokio with `worker_threads = clamp(available_parallelism, 2, 8)`
  and `max_blocking_threads = 64`.
- Logs go to the endpoint log file with the same `tracing` subscriber as today.
  `RUST_LOG` on the daemon process applies.

## Concurrency and memory bounds

| Bound | Value | Enforced by |
| --- | --- | --- |
| Handshake line | 64 KiB | `ipc::handshake` |
| Control answer | 8 MiB | `ipc::handshake` |
| Sessions | No fixed cap. Each session costs one task set and one `Client`. | Listener |
| Coordinator trigger channel | 64 batches | `CheckoutCoordinator` |
| Scan concurrency | Scheduler `scan` permits | `Scheduler` |
| Upload concurrency | Scheduler `upload` permits | `Scheduler` |
| Coordinator idle retention | 300 seconds | `CheckoutRegistry` sweeper |
| Job records | One per coordinator | `CheckoutCoordinator` |
| Freshness cache | One entry per session | `Shared` |

## Environment and configuration keys

| Key | Role | Meaning |
| --- | --- | --- |
| `SEMCTX_MCP_DAEMON` | client | `off`, `auto`, `require`. Default `auto`. |
| `SEMCTX_DAEMON_IDLE_SECS` | daemon | Idle exit delay in seconds. Default 600. `0` exits as soon as the last session ends. |
| `SEMCTX_DAEMON_SCAN_PERMITS` | daemon, standalone | Scheduler override |
| `SEMCTX_DAEMON_UPLOAD_PERMITS` | daemon, standalone | Scheduler override |
| `SEMCTX_DAEMON_REMOTE_PERMITS` | daemon, standalone | Scheduler override |
| `SEMCTX_MCP_RESYNC_SECS` | session | Unchanged meaning, now per session context |
| `SEMCTX_MCP_UPDATE_CHECK` | session | Unchanged meaning |
| `SEMCTX_TOKEN` | session | Unchanged meaning, transported in the handshake |

## Testing

Unit tests, deterministic and offline:

- Handshake encode, decode, size limit, protocol mismatch, unknown kind.
- Endpoint identity is stable for the same inputs and differs by config
  directory, version, and user.
- Unix runtime directory validation rejects wrong owner and wrong mode.
- Scheduler permits clamp and block.
- Coordinator coalesces a burst of triggers into one run and runs one follow-up
  when a trigger arrives mid-run. The coordinator takes its reconcile function
  through a small trait so tests can count runs without a server.
- Registry returns the same coordinator for the same key, different coordinators
  for different credential scopes, and releases after the idle grace.
- Readiness for an empty selector waits only on the session's leased gates.

Integration tests in `tests/`, Unix in this stage, Windows when a Windows runner
exists:

- One daemon serves N clients; `daemon status` reports N sessions; sessions drop
  to zero; `daemon stop` exits.
- Two clients started at the same time produce one daemon.
- `SEMCTX_MCP_DAEMON=auto` falls back to standalone when the runtime directory
  is unusable. `require` fails.
- Two sessions attached to one temporary root share one coordinator and one
  watch registration.

Cross-platform compile checks run on every stage where the machine has the
target's C toolchain. When the toolchain is missing, the stage summary says so.

## Measurement gate

`reports/measure_mcp_processes.py` runs the same cases in both modes. Its
`--mode daemon` records the daemon process beside its clients, and its
`--checkouts` option spreads the clients over several checkouts. The gate for
flipping the default to `auto`:

- 100 clients on one checkout: one startup manifest and one manifest per settled
  edit burst.
- Total PSS materially below the standalone case.
- Thread and file descriptor growth bounded by the client pump cost.
- No tool latency regression beyond the budget set in the report.

Each line is evaluated against measurements in
[reports/shared-daemon-results.md](../reports/shared-daemon-results.md).

## Stages

| Stage | Scope | Commits | Status |
| --- | --- | --- | --- |
| 1a | `SessionContext`, `CredentialSource`, `HttpTransport`; `Client` and `auth` take them; standalone `mcp::run` builds the context from the process. | `f527804`, `c39f2cb` | Complete |
| 1b | `ipc` module: endpoint identity, runtime directory, Unix and Windows listeners and connectors, handshake types, byte pump. Built in a separate worktree on `v0.2-ipc`. | `f9e82e0`, `7d39760`, merged by `e1bf613` | Complete |
| 2 | `engine`: registry, coordinator, watch hub, scheduler. `McpServer` uses the engine. `background.rs` and `watcher.rs` responsibilities move. Readiness and jobs rescoped. | `e761b8a`, `cd049e1`, `7c3968b`, `2533055`, `364a1d5` | Complete |
| 3 | `daemon` command, client role in `main`, spawn and election, status and stop. | `ba8c5f7`, `58d5986`, `edb1d87` | Complete |
| 4 | Daemon and multi-checkout cases in the measurement harness, results recorded, documents updated, default decided, version 0.2.0. | The commits of this stage | Complete |

## Non-goals in this iteration

- Query result caching across sessions.
- Native Streamable HTTP transport from hosts.
- Routing `semctl hook` through the daemon.
- Removing the standalone role.
