# Shared local daemon results

Date: 2026-09-17 UTC. Repository revision: `c9215a1e5b25b88520ccedc93816b13870e73a31`.
Package at measurement time: `semctl 0.1.19`. The release of these changes is
0.2.0, and the version bump follows in the same change.
Release binary sha256 prefix: `5f035ddcee80f9b7`.
Host: Linux 7.0.0-31-generic, x86-64, 16 available CPUs, 121 GiB RAM.

This report measures the shared local daemon that
[docs/daemon-architecture.md](../docs/daemon-architecture.md) describes. The
feasibility study that motivated the daemon is
[shared-daemon-feasibility.md](shared-daemon-feasibility.md), and its baseline
numbers were taken at revision `93efefd`. Every number below was taken again at
the revision named above, so the two modes are comparable.

## Result

The shared daemon passes every line of the design's measurement gate. The
default of `SEMCTX_MCP_DAEMON` therefore becomes `auto`.

For 100 MCP sessions on one checkout:

| Quantity | Standalone | Daemon | Change |
| --- | ---: | ---: | --- |
| Total PSS | 2,619.8 MiB | 119.0 MiB | 22 times less |
| Threads | 2,203 | 418 | 5.3 times fewer |
| inotify instances | 100 | 1 | one for every session |
| inotify watch entries | 2,500 | 25 | one set for the checkout |
| Manifests at startup | 100 | 1 | one scan, not 100 |
| Manifests after one edit | 100 | 1 | one reconcile, not 100 |
| CPU during the edit | 58.37 s | 0.07 s | 830 times less |
| `tools/list` p95 | 0.823 s | 0.035 s | 23 times faster |

## Method

The harness is [measure_mcp_processes.py](measure_mcp_processes.py). It is one
Python 3 file and uses the standard library only. One case does this:

1. It creates a temporary configuration directory, a temporary runtime
   directory with mode `0700`, an empty global Git configuration file, and one
   or more synthetic checkouts.
2. It starts a loopback mock HTTP server. The server answers every sync request
   with an empty `needContent` list and records the manifest.
3. It starts N `semctl mcp` processes. Each process gets `--codebase` of its own
   checkout and runs with that checkout as its working directory.
4. It sends `initialize` to each process as soon as that process is started, and
   it reads every answer through one poller. It then sends
   `notifications/initialized` and `tools/list` to every process and reads those
   answers the same way. The time from one process's request to that process's
   answer is that process's round trip.
5. It waits for the startup manifests and for every checkout to report a watch
   outcome, then it waits two seconds and samples `/proc` for every process.
6. It appends one line to one file in one checkout, waits for the manifests that
   the edit produces, waits two seconds, and samples `/proc` again. The
   difference is the CPU time of the edit.
7. It ends every process it started. In daemon mode it then runs
   `semctl daemon stop` and waits for the daemon to leave. A failure, an
   exception, and a termination signal all reach the same cleanup.

Both modes run the same steps. Standalone mode sets `SEMCTX_MCP_DAEMON=off`.
Daemon mode sets `SEMCTX_MCP_DAEMON=require` and a per-case `XDG_RUNTIME_DIR`,
and it leaves `SEMCTX_DAEMON_IDLE_SECS` at its default of 600 seconds. Daemon
mode reads the daemon's process id from `semctl daemon status --json` and
samples the daemon separately from the clients.

Every case records its environment, the kernel limits that apply to it, the
repository revision, the binary's `--version` output, and the binary's sha256.
The harness raises its own descriptor limit to three per client.

Settings common to every case: `SEMCTX_MCP_UPDATE_CHECK=0` removes the startup
update lookup, and `SEMCTX_MCP_RESYNC_SECS=0` disables the periodic re-sync.
Both isolate the startup scan and the one edit. No production credential,
source file, or indexing job took part.

### Case shapes

| Shape | Checkouts | Files per checkout | Directories per checkout | Source bytes per checkout |
| --- | ---: | ---: | ---: | ---: |
| One checkout | 1 | 2,000 | 20 | 8,178,000 |
| Several checkouts | 100, 200, or 1,000 | 40 | 4 | 163,560 |

The small tree keeps a 1,000-checkout case affordable. Building 1,000 checkouts
takes 1.5 seconds, and the whole 1,000-client 1,000-checkout case completes in
about six minutes. Every multi-checkout case states its tree size in the JSON.

Clients are spread evenly over the checkouts. Client `i` uses checkout
`i mod C`. The edit always changes one file in the first checkout.

## One checkout

Every client of these cases watches the same checkout. This is the shape the
daemon exists for.

| Mode | Clients | Checkouts | Total PSS, MiB | Daemon PSS, MiB | Total RSS, MiB | Threads | Descriptors | inotify watch entries | inotify instances |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| standalone | 1 | 1 | 21.4 | - | 24.0 | 21 | 12 | 25 | 1 |
| standalone | 30 | 1 | 764.3 | - | 1,139.2 | 661 | 360 | 750 | 30 |
| standalone | 100 | 1 | 2,619.8 | - | 3,898.1 | 2,203 | 1,200 | 2,500 | 100 |
| daemon | 1 | 1 | 22.3 | 18.5 | 32.5 | 15 | 25 | 25 | 1 |
| daemon | 30 | 1 | 53.4 | 21.7 | 293.3 | 145 | 344 | 25 | 1 |
| daemon | 100 | 1 | 119.0 | 30.0 | 910.6 | 418 | 1,114 | 25 | 1 |
| daemon | 1000 | 1 | 944.9 | 134.2 | 8,846.4 | 3,620 | 11,014 | 25 | 1 |

PSS means proportional set size. It gives each process a share of the pages it
shares with another process. RSS means resident set size, and a sum of RSS
counts a shared page once per process. Use PSS for a comparison of totals.
Neither number includes kernel watcher memory or the filesystem page cache.

| Mode | Clients | Checkouts | Startup manifests | Manifests after one edit | Client CPU during the edit, s | Daemon CPU during the edit, s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| standalone | 1 | 1 | 1 | 1 | 0.07 | - |
| standalone | 30 | 1 | 30 | 30 | 10.49 | - |
| standalone | 100 | 1 | 100 | 100 | 58.37 | - |
| daemon | 1 | 1 | 1 | 1 | 0.00 | 0.08 |
| daemon | 30 | 1 | 1 | 1 | 0.00 | 0.07 |
| daemon | 100 | 1 | 1 | 1 | 0.00 | 0.07 |
| daemon | 1000 | 1 | 1 | 1 | 0.00 | 0.07 |

The daemon submits one manifest at startup and one manifest for one edit, at
every client count. Standalone mode submits one of each per process. CPU time
is summed over the measured processes during the edit observation. It is not
elapsed time, and it excludes short-lived Git child processes.

| Mode | Clients | Checkouts | `initialize` p50 / p95 / max, s | `tools/list` p50 / p95 / max, s |
| --- | ---: | ---: | ---: | ---: |
| standalone | 1 | 1 | 0.007 / 0.007 / 0.007 | 0.001 / 0.001 / 0.001 |
| standalone | 30 | 1 | 0.021 / 0.025 / 0.028 | 0.018 / 0.026 / 0.028 |
| standalone | 100 | 1 | 0.196 / 0.247 / 0.280 | 0.400 / 0.823 / 0.866 |
| daemon | 1 | 1 | 0.321 / 0.321 / 0.321 | 0.001 / 0.001 / 0.001 |
| daemon | 30 | 1 | 0.308 / 0.316 / 0.317 | 0.006 / 0.011 / 0.011 |
| daemon | 100 | 1 | 0.306 / 0.312 / 0.323 | 0.020 / 0.035 / 0.036 |
| daemon | 1000 | 1 | 0.352 / 0.469 / 0.500 | 0.190 / 0.323 / 0.337 |

`tools/list` is the tool round trip of this report. It is one request and one
answer over an established session, so it measures what a tool call costs. The
daemon is faster than standalone mode at every client count above one, and the
difference grows with the client count.

`initialize` includes everything a session costs before it can serve: the
process start, the role decision, and, in daemon mode, the attach. Every daemon
case shows about 0.31 seconds, because the harness starts every client of a case
at once and no daemon exists yet. The next section separates that cost.

## The cost of attaching

The batch cases cannot separate "start a daemon" from "attach to a daemon",
because every client of a case starts at the same time. The attach probe starts
sessions one at a time instead.

| Mode | First session, s | Later sessions p50, s | Later sessions min / max, s |
| --- | ---: | ---: | ---: |
| standalone | 0.0032 | 0.0028 | 0.0026 / 0.0031 |
| daemon | 0.3169 | 0.0025 | 0.0024 / 0.0031 |

The first session of a daemon pays about 0.31 seconds. That is the client's
300 millisecond probe for a daemon that is already listening, plus the start of
the daemon it then launches. Every session after it attaches in about 2.5
milliseconds, which is no slower than standalone mode. The cost is paid once
per daemon, not once per session and not once per tool call.

## No checkout

These cases pin a codebase that has no local checkout, so no process watches
anything. They isolate the cost of the process and its runtime.

| Mode | Clients | Total PSS, MiB | Daemon PSS, MiB | Total RSS, MiB | Threads | Descriptors |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| standalone | 1 | 10.1 | - | 12.7 | 19 | 9 |
| standalone | 30 | 56.9 | - | 378.0 | 574 | 270 |
| standalone | 100 | 166.1 | - | 1,259.5 | 1,907 | 900 |
| daemon | 1 | 11.6 | 7.6 | 21.8 | 12 | 22 |
| daemon | 30 | 42.4 | 10.7 | 283.6 | 111 | 341 |
| daemon | 100 | 107.6 | 18.7 | 900.1 | 383 | 1,111 |
| daemon | 1000 | 934.8 | 122.7 | 8,836.3 | 3,698 | 11,011 |

One client process costs 0.81 MiB of PSS at 1,000 clients, 2 to 4 threads, and
exactly 10 descriptors. The thread count varies because Tokio reaps the
blocking thread that reads standard input after it is idle. A standalone
process at 100 processes costs 1.66 MiB of PSS, 19 threads, and 9 descriptors
with no checkout, and 26.2 MiB of PSS, 22 threads, and 12 descriptors with one
checkout.

The daemon costs 9 threads with no checkout. It costs 12 threads for one
checkout, 35 for 100 checkouts, and 75 for 1,000 checkouts, which is the
bounded worker pool plus the blocking pool it uses for scans. Its descriptor
count is 11 plus one per session.

## Several checkouts

Each client has its own checkout here, so no scan can be shared. The daemon
still shares one runtime, one HTTP transport, and one filesystem watcher.

| Mode | Clients | Checkouts | Total PSS, MiB | Daemon PSS, MiB | Total RSS, MiB | Threads | Descriptors | inotify watch entries | inotify instances |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| standalone | 100 | 100 | 264.5 | - | 1,540.5 | 2,207 | 1,200 | 900 | 100 |
| standalone | 200 | 200 | 508.3 | - | 3,076.6 | 4,327 | 2,133 | 999 | 111 |
| daemon | 100 | 100 | 127.5 | 38.4 | 922.6 | 429 | 1,114 | 504 | 1 |
| daemon | 1000 | 1000 | 1,391.8 | 581.8 | 9,432.6 | 2,075 | 11,014 | 5,004 | 1 |

| Mode | Clients | Checkouts | Startup manifests | Manifests after one edit | Client CPU during the edit, s | Daemon CPU during the edit, s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| standalone | 100 | 100 | 100 | 1 | 0.49 | - |
| standalone | 200 | 200 | 200 | 1 | 0.65 | - |
| daemon | 100 | 100 | 100 | 1 | 0.00 | 0.06 |
| daemon | 1000 | 1000 | 1,000 | 1 | 0.00 | 5.03 |

| Mode | Clients | Checkouts | `initialize` p50 / p95 / max, s | `tools/list` p50 / p95 / max, s |
| --- | ---: | ---: | ---: | ---: |
| standalone | 100 | 100 | 0.053 / 0.097 / 0.099 | 0.038 / 0.065 / 0.072 |
| standalone | 200 | 200 | 0.121 / 0.193 / 0.197 | 0.095 / 0.167 / 0.180 |
| daemon | 100 | 100 | 0.311 / 0.330 / 0.334 | 0.025 / 0.038 / 0.040 |
| daemon | 1000 | 1000 | 0.500 / 0.819 / 1.262 | 0.252 / 0.400 / 0.414 |

Both modes submit one manifest per checkout at startup, because each checkout
is a distinct source and needs its own snapshot. The daemon does not reduce
that work, and the design does not claim it does. It reduces the process cost
around it: half the memory, a fifth of the threads, and one inotify instance
instead of one per process.

The 1,000-checkout daemon case holds 1,000 coordinators with an active watcher,
1,000 sessions, and 5,004 inotify watch entries in one process. The daemon's
CPU time of 5.03 seconds in the edit window is the tail of the startup scans of
1,000 checkouts, not the cost of the one edit. The scan permits bound that work
to eight concurrent scans.

## The inotify instance wall in standalone mode

Linux limits how many inotify instances one user may hold. This host allows 128
(`fs.inotify.max_user_instances`). Every standalone `semctl mcp` process holds
one instance for its checkout. The daemon holds one instance for every checkout
it watches.

The 200-client, 200-checkout standalone case crossed that limit. 111 processes
logged `fs watcher active`, and 89 processes logged `no realtime watch; using
the short periodic re-sync`. Other processes of this operating-system user held
the remaining instances. The 89 processes without a watcher kept serving: they
completed `initialize` and `tools/list`, and they submitted their startup
manifests. They cannot observe an edit until their periodic re-sync runs, and
the harness disables that timer, so a later edit in one of those checkouts would
have gone unnoticed.

The daemon has no such wall at this scale. Its 1,000-checkout case used one
instance and 5,004 of the 1,005,716 available watch entries.

The count of `fs watcher active` lines in a daemon case is not the number of
live watches. Two sessions that attach one checkout at the same time can both
build a coordinator, and the one that loses the race releases its registration
at once. The 100-client one-checkout case logged 24 registrations that way. The
live state is the measured one: 25 watch entries and one inotify instance.

## Measurement gate

The gate is in [docs/daemon-architecture.md](../docs/daemon-architecture.md)
under "Measurement gate". Each line is evaluated against the tables above.

**1. 100 clients on one checkout: one startup manifest and one manifest per
settled edit burst. PASS.** The daemon case submitted one manifest at startup
and one manifest for the edit. Standalone mode submitted 100 of each. The same
holds at 1,000 clients on one checkout.

**2. Total PSS materially below the standalone case. PASS.** 119.0 MiB against
2,619.8 MiB for 100 clients on one checkout, which is 22 times less. With 100
separate checkouts the reduction is smaller but still material: 127.5 MiB
against 264.5 MiB.

**3. Thread and file descriptor growth bounded by the client pump cost. PASS.**
One client costs 2 to 4 threads and exactly 10 descriptors, against 22 threads
and 12 descriptors for a standalone process on a checkout. The daemon adds 9 to
75 threads for the whole process and one descriptor per session. Total threads
for 100 clients on one checkout fell from 2,203 to 418. The descriptor total is
higher in daemon mode (1,114 against 1,200 at 100 clients, and 11,014 at 1,000
clients) because each session costs one socket on each side. That growth is the
pump cost the gate allows, and it is linear with one descriptor per session.

**4. No tool latency regression beyond the budget set in the report. PASS.**
The budget set here: a tool round trip in daemon mode must not exceed the
standalone p95 at the same client count by more than 50 milliseconds, and a
session must not cost more than one second to start. Measured `tools/list` p95
in daemon mode is 0.001 s at one client (standalone 0.001 s), 0.011 s at 30
(standalone 0.026 s), and 0.035 s at 100 (standalone 0.823 s). No client count
regressed. Session startup costs 0.317 s once per daemon and about 0.0025 s for
every session after it, which is inside the budget and no slower than
standalone mode.

Every line passes. The default of `SEMCTX_MCP_DAEMON` becomes `auto`.

## Two defects these measurements found

Both were fixed before the numbers above were taken.

1. **A shared root watch was released too early.** Two registrations can name
   one root, because two sessions can attach one checkout at the same time. The
   watch hub released the platform watch when the first of them dropped, and
   the remaining coordinator then received no further event. The 30-client and
   100-client one-checkout cases showed it: one startup manifest, and no
   manifest at all for the later edit. The root watch is now reference counted,
   as the external watches already were.
2. **A control answer could not leave the daemon.** A status answer carries one
   entry per checkout, and the 64 KiB handshake bound cut it off at about 350
   checkouts. `semctl daemon status` then reported that the peer closed the
   connection during the handshake, while every MCP session kept working. A
   control answer now has its own bound of 8 MiB. The 1,000-checkout case
   answered the first status request after the fix.

## Limitations

- Every number is one run of one case. No case was repeated, so none of these
  numbers carries a confidence interval.
- The checkouts are synthetic. Every file has the same size and the same
  content, and no checkout is a Git repository.
- The server is a loopback mock. It requests no content upload, runs no
  embedding, and answers immediately. Real upload volume, server latency, and
  job polling are not represented.
- The measurement is Linux only. `/proc`, PSS, and inotify limits are Linux
  facts. The Windows and macOS paths of the daemon are compile-checked and have
  no runtime measurements.
- The workload is startup, a tool listing, and one edit. Search traffic,
  concurrent retrieval, large uploads, branch switches, credential refresh, and
  daemon restarts are not measured.
- Memory is sampled after a two-second settling period. Allocator retention and
  blocking-thread lifetimes affect the sample.
- The harness runs on the same host as the processes it measures. Its own
  polling costs CPU time, which is not attributed to any measured process.
- The inotify wall depends on what else the operating-system user runs. This
  case obtained 111 of the 128 instances, so about 17 were already in use by
  other processes of this user.

## Raw data and reproduction

| File | Content |
| --- | --- |
| [mcp-process-standalone.json](mcp-process-standalone.json) | Standalone, 1, 30, and 100 clients, one checkout |
| [mcp-process-daemon.json](mcp-process-daemon.json) | Daemon, 1, 30, 100, and 1,000 clients, one checkout |
| [mcp-process-standalone-checkouts.json](mcp-process-standalone-checkouts.json) | Standalone, 100 clients on 100 checkouts and 200 on 200 |
| [mcp-process-daemon-checkouts.json](mcp-process-daemon-checkouts.json) | Daemon, 100 clients on 100 checkouts and 1,000 on 1,000 |
| [mcp-process-attach-probe-standalone.json](mcp-process-attach-probe-standalone.json) | Standalone sessions started one at a time |
| [mcp-process-attach-probe-daemon.json](mcp-process-attach-probe-daemon.json) | Daemon sessions started one at a time |

Earlier files [mcp-process-baseline.json](mcp-process-baseline.json) and
[mcp-process-two-workers.json](mcp-process-two-workers.json) belong to the
feasibility study at revision `93efefd`. They are kept as they were.

Build the binary and run the cases from the repository root:

```sh
export CARGO_TARGET_DIR=/home/dev/.cache/semctl-v02-target
cargo build --release --locked
BIN=$CARGO_TARGET_DIR/release/semctl
python3 reports/measure_mcp_processes.py $BIN \
  reports/mcp-process-standalone.json --mode standalone --counts 1 30 100 --checkouts 1
python3 reports/measure_mcp_processes.py $BIN \
  reports/mcp-process-daemon.json --mode daemon --counts 1 30 100 1000 --checkouts 1
python3 reports/measure_mcp_processes.py $BIN \
  reports/mcp-process-standalone-checkouts.json --mode standalone --counts 100/100 200/200
python3 reports/measure_mcp_processes.py $BIN \
  reports/mcp-process-daemon-checkouts.json --mode daemon --counts 100/100 1000/1000
python3 reports/measure_mcp_processes.py $BIN \
  reports/mcp-process-attach-probe-standalone.json --mode standalone --attach-probe 10
python3 reports/measure_mcp_processes.py $BIN \
  reports/mcp-process-attach-probe-daemon.json --mode daemon --attach-probe 10
```

The harness needs Python 3, Git, and readable Linux `/proc` metrics. It removes
its temporary files after each case, and it ends every process it starts,
including on an exception and on an interrupt. The complete run takes about
45 minutes on this host.
