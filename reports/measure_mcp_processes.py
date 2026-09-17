#!/usr/bin/env python3
"""Measure Linux MCP process cost in standalone mode and in shared-daemon mode.

The harness starts N `semctl mcp` clients against synthetic checkouts and a
loopback mock server. It records process metrics, manifest counts, and tool
round-trip latency. In daemon mode it also records the shared daemon process.
"""

import argparse
import collections
import datetime
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import re
import resource
import selectors
import signal
import subprocess
import sys
import tempfile
import threading
import time

# One sync request path, with the codebase id the client asked for.
SYNC_PATH = re.compile(r"^/v1/codebases/([^/]+)/sync$")

# Files and directories of one synthetic checkout, by case shape. A
# multi-checkout case uses the small tree so 1,000 checkouts stay affordable.
LARGE_TREE = (2000, 20)
SMALL_TREE = (40, 4)

# Log lines that report the two outcomes of one watch registration.
WATCH_ACTIVE = "fs watcher active"
WATCH_UNAVAILABLE = "no realtime watch"

# Descriptors the harness keeps for itself, beyond three per client.
SPARE_DESCRIPTORS = 256

# Longest the harness waits for one phase of a case.
PHASE_TIMEOUT = 600.0


class MockServer(http.server.ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 512

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.lock = threading.Lock()
        self.manifests = []

    def snapshot(self):
        with self.lock:
            return list(self.manifests)

    def count(self):
        with self.lock:
            return len(self.manifests)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length)) if length else {}
        match = SYNC_PATH.match(self.path)
        if match is None:
            self.send_error(404)
            return
        with self.server.lock:
            self.server.manifests.append(
                {
                    "codebase": match.group(1),
                    "files": len(body["files"]),
                    "bytes": sum(item["size"] for item in body["files"]),
                    "source_id": body["sourceId"],
                }
            )
            job_id = str(len(self.server.manifests))
        payload = json.dumps(
            {
                "success": True,
                "data": {"jobId": job_id, "needContent": [], "toDelete": []},
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


class Client:
    """One `semctl mcp` process and the requests it owes an answer to."""

    def __init__(self, index, checkout, process, log_path):
        self.index = index
        self.checkout = checkout
        self.process = process
        self.log_path = log_path
        self.buffer = bytearray()
        self.pending = {}
        self.latency = {}
        self.closed = False

    def send(self, message, request_id=None, method=None):
        self.process.stdin.write(json.dumps(message).encode() + b"\n")
        self.process.stdin.flush()
        if request_id is not None:
            self.pending[request_id] = (time.monotonic(), method)

    def feed(self, chunk):
        """Record the arrival of every complete line in `chunk`."""
        self.buffer.extend(chunk)
        while True:
            end = self.buffer.find(b"\n")
            if end < 0:
                return
            line = bytes(self.buffer[:end])
            del self.buffer[: end + 1]
            if not line.strip():
                continue
            message = json.loads(line)
            entry = self.pending.pop(message.get("id"), None)
            if entry is None:
                continue
            started, method = entry
            if "error" in message:
                raise RuntimeError(f"client {self.index}: {message['error']}")
            self.latency[method] = time.monotonic() - started


class Fleet:
    """Every client of one case, read together so latency is not serialized."""

    def __init__(self):
        self.clients = []
        self.selector = selectors.DefaultSelector()

    def add(self, client):
        self.clients.append(client)
        self.selector.register(client.process.stdout, selectors.EVENT_READ, client)

    def pump(self, description, timeout=PHASE_TIMEOUT):
        """Read answers until every client has none pending."""
        deadline = time.monotonic() + timeout
        while any(client.pending for client in self.clients):
            if time.monotonic() >= deadline:
                waiting = sum(1 for client in self.clients if client.pending)
                raise TimeoutError(f"{description}: {waiting} clients still pending")
            for key, _events in self.selector.select(0.2):
                client = key.data
                chunk = os.read(client.process.stdout.fileno(), 65536)
                if not chunk:
                    client.closed = True
                    self.selector.unregister(client.process.stdout)
                    raise RuntimeError(
                        f"client {client.index} closed its output: {client.process.poll()}"
                    )
                client.feed(chunk)

    def percentiles(self, method):
        values = sorted(
            client.latency[method] for client in self.clients if method in client.latency
        )
        if not values:
            return None

        def at(fraction):
            index = min(len(values) - 1, max(0, round(fraction * (len(values) - 1))))
            return round(values[index], 4)

        return {
            "clients": len(values),
            "p50_seconds": at(0.5),
            "p95_seconds": at(0.95),
            "max_seconds": round(values[-1], 4),
        }

    def close(self):
        for client in self.clients:
            if client.process.poll() is None:
                client.process.terminate()
        for client in self.clients:
            try:
                client.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                client.process.kill()
                client.process.wait()
            client.process.stdin.close()
            client.process.stdout.close()
        self.selector.close()


def wait_until(predicate, description, timeout=PHASE_TIMEOUT, interval=0.05):
    """Wait for `predicate`. Return whether it became true before the deadline."""
    del description
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() >= deadline:
            return False
        time.sleep(interval)
    return True


def process_metrics(pid):
    root = Path("/proc") / str(pid)
    status = dict(line.split(":", 1) for line in (root / "status").read_text().splitlines())
    memory = dict(
        line.split(":", 1)
        for line in (root / "smaps_rollup").read_text().splitlines()
        if ":" in line
    )

    def memory_kib(key):
        return int(memory.get(key, "0 kB").split()[0])

    fields = (root / "stat").read_text().split(")", 1)[1].split()
    watches = 0
    instances = 0
    for entry in (root / "fdinfo").iterdir():
        try:
            watches += sum(
                line.startswith("inotify wd:") for line in entry.read_text().splitlines()
            )
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
    for entry in (root / "fd").iterdir():
        try:
            instances += os.readlink(entry) == "anon_inode:inotify"
        except OSError:
            continue
    return {
        "rss_kib": memory_kib("Rss"),
        "pss_kib": memory_kib("Pss"),
        "uss_kib": memory_kib("Private_Clean") + memory_kib("Private_Dirty"),
        "threads": int(status["Threads"]),
        "fds": len(list((root / "fd").iterdir())),
        "inotify_watches": watches,
        "inotify_instances": instances,
        "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
    }


def totals(pids):
    result = collections.Counter()
    for pid in pids:
        result.update(process_metrics(pid))
    return dict(result)


def combine(left, right):
    result = collections.Counter(left)
    result.update(right)
    return dict(result)


def raise_descriptor_limit(clients):
    """Give the harness three descriptors per client, plus its own."""
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    needed = clients * 3 + SPARE_DESCRIPTORS
    if soft >= needed:
        return {"soft": soft, "hard": hard, "raised_to": soft}
    target = min(hard, needed)
    resource.setrlimit(resource.RLIMIT_NOFILE, (target, hard))
    return {"soft": soft, "hard": hard, "raised_to": target}


def read_limits():
    """Kernel limits that decide whether a case can complete on this machine."""
    def read(path):
        try:
            return int(Path(path).read_text().strip())
        except OSError:
            return None

    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    processes = resource.getrlimit(resource.RLIMIT_NPROC)
    return {
        "rlimit_nofile": [soft, hard],
        "rlimit_nproc": [processes[0], processes[1]],
        "inotify_max_user_instances": read("/proc/sys/fs/inotify/max_user_instances"),
        "inotify_max_user_watches": read("/proc/sys/fs/inotify/max_user_watches"),
    }


def build_checkouts(base, count, files, directories):
    """Create `count` synthetic checkouts and the config that caches each one."""
    content = "// synthetic benchmark source\n" + "let x = 1;\n" * 369
    roots = []
    cache = []
    for checkout_index in range(count):
        root = base / f"checkout-{checkout_index:04}"
        root.mkdir()
        for file_index in range(files):
            directory = root / f"d{file_index % directories:02}"
            directory.mkdir(exist_ok=True)
            (directory / f"f{file_index:04}.rs").write_text(content)
        roots.append(root)
        cache.append(f"{json.dumps(str(root))} = {json.dumps(codebase_of(checkout_index))}")
    return roots, "[codebase_cache]\n" + "\n".join(cache) + "\n"


def codebase_of(index):
    return f"benchmark-{index:04}"


def daemon_control(binary, environment, arguments):
    return subprocess.run(
        [str(binary), "daemon", *arguments],
        env=environment,
        capture_output=True,
        text=True,
        check=False,
        timeout=60,
    )


def daemon_pid(binary, environment):
    """The running daemon's process id, and why it could not be read."""
    result = daemon_control(binary, environment, ["status", "--json"])
    if result.returncode != 0:
        return None, f"exit {result.returncode}: {result.stderr.strip()[-400:]}"
    try:
        return int(json.loads(result.stdout)["pid"]), None
    except (json.JSONDecodeError, KeyError, ValueError) as error:
        return None, f"unreadable answer: {error}"


def await_daemon_pid(binary, environment, attempts=30, pause=2.0):
    """Ask for the daemon's process id until it answers.

    `semctl daemon status` connects once and does not retry, so a burst of a
    thousand attaching clients can fill the listen backlog and make one probe
    report that no daemon is running. The harness retries; the number of
    attempts is part of the result.
    """
    reason = "not asked"
    for attempt in range(1, attempts + 1):
        pid, reason = daemon_pid(binary, environment)
        if pid is not None:
            return pid, attempt, None
        time.sleep(pause)
    return None, attempts, reason


def daemon_pid_from_proc(binary, runtime_dir):
    """Find the running daemon by its command line and its runtime directory.

    The control command is the documented way to read a daemon's process id.
    This is the fallback for a case where the control command cannot answer,
    so that a case still reports the daemon it measured.
    """
    wanted = f"{binary}\0daemon\0run\0".encode()
    marker = f"XDG_RUNTIME_DIR={runtime_dir}".encode()
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            if (entry / "cmdline").read_bytes() != wanted:
                continue
            if marker not in (entry / "environ").read_bytes().split(b"\0"):
                continue
        except OSError:
            continue
        return int(entry.name)
    return None


def daemon_status(binary, environment):
    """The daemon's own report, with its checkout list reduced to counts.

    One case can hold a thousand checkouts. Their individual entries repeat the
    same facts, so the result keeps the daemon-level report, how many checkouts
    it holds, and how many of them have a watcher.
    """
    result = daemon_control(binary, environment, ["status", "--json"])
    if result.returncode != 0:
        return None
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError:
        return None
    coordinators = report.pop("coordinators", [])
    watchers = collections.Counter(
        entry["watcher"] if isinstance(entry["watcher"], str) else "unavailable"
        for entry in coordinators
    )
    report["coordinators"] = len(coordinators)
    report["coordinator_watchers"] = dict(sorted(watchers.items()))
    report["coordinator_leases"] = sorted({entry["leases"] for entry in coordinators})
    return report


def stop_daemon(binary, environment, pid):
    """Stop the daemon and wait for it to leave. Never leave one behind."""
    if pid is None:
        pid, _reason = daemon_pid(binary, environment)
    if pid is None:
        return {"stopped": False, "reason": "no daemon answered"}
    result = daemon_control(binary, environment, ["stop"])
    gone = wait_until(lambda: not Path(f"/proc/{pid}").exists(), "daemon exit", timeout=30)
    killed = False
    if not gone:
        for number in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.kill(pid, number)
            except ProcessLookupError:
                break
            killed = True
            if wait_until(lambda: not Path(f"/proc/{pid}").exists(), "daemon exit", timeout=10):
                break
    return {
        "stopped": True,
        "pid": pid,
        "stop_exit_code": result.returncode,
        "exited_on_request": gone,
        "signalled": killed,
    }


def count_in_file(path, needle):
    try:
        return Path(path).read_text(errors="replace").count(needle)
    except OSError:
        return 0


def daemon_log_paths(runtime_dir):
    return sorted((Path(runtime_dir) / "semctl").glob("*.log"))


def watch_lines(mode, fleet, runtime_dir):
    """How many watch registrations reported each outcome."""
    if mode == "daemon":
        active = sum(count_in_file(path, WATCH_ACTIVE) for path in daemon_log_paths(runtime_dir))
        missing = sum(
            count_in_file(path, WATCH_UNAVAILABLE) for path in daemon_log_paths(runtime_dir)
        )
        return active, missing
    active = sum(count_in_file(client.log_path, WATCH_ACTIVE) for client in fleet.clients)
    missing = sum(count_in_file(client.log_path, WATCH_UNAVAILABLE) for client in fleet.clients)
    return active, missing


def manifests_by_codebase(manifests):
    counter = collections.Counter(item["codebase"] for item in manifests)
    return dict(sorted(counter.items()))


def summarize(manifests):
    per_checkout = manifests_by_codebase(manifests)
    return {
        "total": len(manifests),
        "bytes": sum(item["bytes"] for item in manifests),
        "file_counts": sorted(set(item["files"] for item in manifests)),
        "distinct_source_ids": len(set(item["source_id"] for item in manifests)),
        "checkouts_reported": len(per_checkout),
        "per_checkout_min": min(per_checkout.values()) if per_checkout else 0,
        "per_checkout_max": max(per_checkout.values()) if per_checkout else 0,
    }


def run_case(binary, count, watched, workers, mode, checkouts, revision, version, rust_log):
    """Run one case and return its record. Clean up every process it starts."""
    files, directories = LARGE_TREE if checkouts == 1 else SMALL_TREE
    limits = raise_descriptor_limit(count)
    # A daemon that lost its election can still append one line to the endpoint
    # log while the case directory is being removed. That is a cleanup race, not
    # a result, so it must not discard a completed measurement.
    with tempfile.TemporaryDirectory(
        prefix="semctl-mcp-measure-", ignore_cleanup_errors=True
    ) as temporary:
        base = Path(temporary)
        config = base / "config" / "semctl"
        config.mkdir(parents=True)
        runtime_dir = base / "run"
        runtime_dir.mkdir(mode=0o700)
        git_config = base / "gitconfig"
        git_config.write_text("")
        roots = [base / "empty"]
        roots[0].mkdir()
        setup_started = time.monotonic()
        if watched:
            roots, cache = build_checkouts(base, checkouts, files, directories)
            (config / "config.toml").write_text(cache)
            (config / "installation-id").write_text("a" * 64 + "\n")
        setup_seconds = time.monotonic() - setup_started

        server = MockServer()
        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()
        environment = {
            key: os.environ[key] for key in ("PATH", "HOME", "LANG") if key in os.environ
        }
        environment.update(
            XDG_CONFIG_HOME=str(base / "config"),
            XDG_RUNTIME_DIR=str(runtime_dir),
            SEMCTX_SERVER=f"http://127.0.0.1:{server.server_port}",
            SEMCTX_TOKEN="synthetic-benchmark-token",
            SEMCTX_MCP_UPDATE_CHECK="0",
            SEMCTX_MCP_RESYNC_SECS="0",
            SEMCTX_MCP_DAEMON="require" if mode == "daemon" else "off",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=str(git_config),
            RUST_LOG=rust_log,
        )
        if workers:
            environment["TOKIO_WORKER_THREADS"] = str(workers)
        control_environment = dict(environment)
        for name in ("SEMCTX_SERVER", "SEMCTX_TOKEN", "SEMCTX_MCP_DAEMON"):
            control_environment.pop(name, None)
        # A control command that cannot reach the daemon reports the transport
        # error at debug level. The harness keeps that reason in its result.
        control_environment["RUST_LOG"] = "debug"

        used_checkouts = min(count, len(roots)) if watched else 0
        edited_clients = sum(1 for index in range(count) if index % len(roots) == 0)
        fleet = Fleet()
        notes = []
        pid = None
        try:
            started = time.monotonic()
            for index in range(count):
                checkout = roots[index % len(roots)]
                log_path = base / f"client-{index:04}.log"
                log = log_path.open("w+")
                try:
                    process = subprocess.Popen(
                        [
                            str(binary),
                            "--codebase",
                            codebase_of(index % len(roots)),
                            "mcp",
                        ],
                        cwd=checkout,
                        env=environment,
                        stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE,
                        stderr=log,
                        bufsize=0,
                    )
                finally:
                    log.close()
                client = Client(index, checkout, process, log_path)
                fleet.add(client)
                client.send(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": "2025-11-25",
                            "capabilities": {},
                            "clientInfo": {"name": "isolated-measurement", "version": "1"},
                        },
                    },
                    request_id=1,
                    method="initialize",
                )
            fleet.pump("initialize")
            for client in fleet.clients:
                client.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                client.send(
                    {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
                    request_id=2,
                    method="tools/list",
                )
            fleet.pump("tools/list")
            attach_seconds = time.monotonic() - started
            status_attempts = None
            if mode == "daemon":
                pid, status_attempts, reason = await_daemon_pid(binary, control_environment)
                if pid is None:
                    pid = daemon_pid_from_proc(binary, runtime_dir)
                    if pid is None:
                        raise RuntimeError(
                            "no daemon answered the endpoint after every client "
                            f"attached: {reason}"
                        )
                    notes.append(
                        f"`semctl daemon status` did not answer in "
                        f"{status_attempts} attempts ({reason}); the daemon process was "
                        f"found through /proc instead"
                    )

            expected_startup = count if mode == "standalone" else used_checkouts
            startup_complete = True
            watch_complete = True
            if watched:
                startup_complete = wait_until(
                    lambda: server.count() >= expected_startup, "startup manifests"
                )
                if not startup_complete:
                    notes.append(
                        f"startup manifests stopped at {server.count()} of {expected_startup}"
                    )
                watch_complete = wait_until(
                    lambda: sum(watch_lines(mode, fleet, runtime_dir)) >= expected_startup,
                    "watch registration",
                    interval=0.5,
                )
                if not watch_complete:
                    notes.append("not every checkout reported a watch outcome")
            time.sleep(2)
            client_pids = [client.process.pid for client in fleet.clients]
            before = totals(client_pids)
            daemon_before = process_metrics(pid) if pid is not None else None
            startup = server.snapshot()
            watch_active, watch_missing = watch_lines(mode, fleet, runtime_dir)

            edit_manifests = []
            edit_cpu_seconds = None
            daemon_edit_cpu_seconds = None
            edit_complete = None
            daemon_after = None
            if watched:
                expected_edit = edited_clients if mode == "standalone" else 1
                path = roots[0] / "d00" / "f0000.rs"
                with path.open("a") as file:
                    file.write("// one edit\n")
                edit_complete = wait_until(
                    lambda: server.count() >= len(startup) + expected_edit,
                    "edit manifests",
                    timeout=120,
                )
                if not edit_complete:
                    notes.append(
                        f"edit manifests stopped at {server.count() - len(startup)} "
                        f"of {expected_edit}"
                    )
                time.sleep(2)
                after = totals(client_pids)
                edit_cpu_seconds = round(after["cpu_seconds"] - before["cpu_seconds"], 3)
                if pid is not None:
                    daemon_after = process_metrics(pid)
                    daemon_edit_cpu_seconds = round(
                        daemon_after["cpu_seconds"] - daemon_before["cpu_seconds"], 3
                    )
                edit_manifests = server.snapshot()[len(startup) :]
            exited = [
                client.index for client in fleet.clients if client.process.poll() is not None
            ]
            if exited:
                raise RuntimeError(f"clients exited before sampling completed: {exited[:10]}")

            record = {
                "mode": mode,
                "processes": count,
                "checkouts": len(roots) if watched else 0,
                "clients_per_checkout": round(count / len(roots), 3) if watched else None,
                "watched": watched,
                "files_per_checkout": files if watched else 0,
                "directories_per_checkout": directories if watched else 0,
                "tokio_worker_threads": workers or "default",
                "setup_seconds": round(setup_seconds, 3),
                "attach_batch_seconds": round(attach_seconds, 3),
                "latency": {
                    "initialize": fleet.percentiles("initialize"),
                    "tools/list": fleet.percentiles("tools/list"),
                },
                "settled_totals": before,
                "daemon_totals": daemon_before,
                "combined_totals": combine(before, daemon_before) if daemon_before else before,
                "watch_registrations_active": watch_active,
                "watch_registrations_unavailable": watch_missing,
                "expected_startup_manifests": expected_startup,
                "startup_manifests_complete": startup_complete,
                "watch_outcomes_complete": watch_complete,
                "startup": summarize(startup),
                "edit": summarize(edit_manifests),
                "edit_manifests_complete": edit_complete,
                "edit_process_cpu_seconds": edit_cpu_seconds,
                "edit_daemon_cpu_seconds": daemon_edit_cpu_seconds,
                "edit_total_cpu_seconds": (
                    round(edit_cpu_seconds + daemon_edit_cpu_seconds, 3)
                    if edit_cpu_seconds is not None and daemon_edit_cpu_seconds is not None
                    else edit_cpu_seconds
                ),
                "daemon_status_attempts": status_attempts,
                "daemon_status": daemon_status(binary, control_environment)
                if mode == "daemon"
                else None,
                "environment": {
                    key: value
                    for key, value in sorted(environment.items())
                    if key not in ("PATH", "HOME", "LANG")
                },
                "harness_descriptor_limit": limits,
                "kernel_limits": read_limits(),
                "revision": revision,
                "binary_version": version,
                "notes": notes,
            }
            # The startup manifest count is what the gate reads. Keep the old
            # field names beside the new grouped ones.
            record["startup_manifests"] = record["startup"]["total"]
            record["edit_manifests"] = record["edit"]["total"]
            return record
        finally:
            fleet.close()
            if mode == "daemon":
                stopped = stop_daemon(binary, control_environment, pid)
                notes.append(f"daemon stop: {json.dumps(stopped)}")
            server.shutdown()
            server.server_close()
            server_thread.join()


def attach_probe(binary, mode, samples, rust_log):
    """Time one session's `initialize` with and without a running daemon.

    The batch cases start every client at once, so every client of a daemon
    case pays the probe delay before the first daemon exists. This case
    separates the two costs: the first session starts a daemon, and the
    sessions after it attach to the one that is already listening.
    """
    with tempfile.TemporaryDirectory(
        prefix="semctl-attach-probe-", ignore_cleanup_errors=True
    ) as temporary:
        base = Path(temporary)
        (base / "config" / "semctl").mkdir(parents=True)
        runtime_dir = base / "run"
        runtime_dir.mkdir(mode=0o700)
        environment = {
            key: os.environ[key] for key in ("PATH", "HOME", "LANG") if key in os.environ
        }
        environment.update(
            XDG_CONFIG_HOME=str(base / "config"),
            XDG_RUNTIME_DIR=str(runtime_dir),
            SEMCTX_SERVER="http://127.0.0.1:9",
            SEMCTX_TOKEN="synthetic-benchmark-token",
            SEMCTX_MCP_UPDATE_CHECK="0",
            SEMCTX_MCP_RESYNC_SECS="0",
            SEMCTX_MCP_DAEMON="require" if mode == "daemon" else "off",
            RUST_LOG=rust_log,
        )
        control_environment = dict(environment)
        for name in ("SEMCTX_SERVER", "SEMCTX_TOKEN", "SEMCTX_MCP_DAEMON"):
            control_environment.pop(name, None)
        fleet = Fleet()
        try:
            seconds = []
            for _ in range(samples + 1):
                process = subprocess.Popen(
                    [str(binary), "--codebase", "probe", "mcp"],
                    cwd=base,
                    env=environment,
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    bufsize=0,
                )
                client = Client(len(fleet.clients), base, process, None)
                fleet.add(client)
                client.send(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": "2025-11-25",
                            "capabilities": {},
                            "clientInfo": {"name": "attach-probe", "version": "1"},
                        },
                    },
                    request_id=1,
                    method="initialize",
                )
                # One client at a time, so each measurement is its own.
                fleet.pump("attach probe", timeout=60)
                seconds.append(round(client.latency["initialize"], 4))
            warm = sorted(seconds[1:])
            return {
                "mode": mode,
                "samples": len(warm),
                "first_session_seconds": seconds[0],
                "later_session_p50_seconds": warm[len(warm) // 2],
                "later_session_min_seconds": warm[0],
                "later_session_max_seconds": warm[-1],
            }
        finally:
            fleet.close()
            if mode == "daemon":
                stop_daemon(binary, control_environment, None)


def failed_case(mode, count, checkouts, watched, error):
    """Record a case that could not complete, with the reason."""
    return {
        "mode": mode,
        "processes": count,
        "checkouts": checkouts if watched else 0,
        "watched": watched,
        "completed": False,
        "failure": f"{type(error).__name__}: {error}",
    }


def parse_case(text, default_checkouts):
    """`N` means N clients on the default checkouts. `N/C` names both."""
    if "/" in text:
        count, checkouts = text.split("/", 1)
        return int(count), int(checkouts)
    return int(text), default_checkouts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument(
        "--counts",
        nargs="+",
        default=["1", "30", "100"],
        help="clients per case, as N or N/CHECKOUTS",
    )
    parser.add_argument("--checkouts", type=int, default=1, help="checkouts per case")
    parser.add_argument("--mode", choices=("standalone", "daemon"), default="standalone")
    parser.add_argument("--workers", type=int, default=0)
    parser.add_argument(
        "--attach-probe",
        type=int,
        default=0,
        help="instead of the cases, time this many sessions started one at a time",
    )
    parser.add_argument(
        "--rust-log",
        default="info",
        help="RUST_LOG for every semctl process of a case, including the daemon",
    )
    parser.add_argument(
        "--idle",
        choices=("auto", "yes", "no"),
        default="auto",
        help="also run every case without a checkout; auto means only for one checkout",
    )
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    cases = [parse_case(text, args.checkouts) for text in args.counts]
    include_idle = args.idle == "yes" or (
        args.idle == "auto" and all(checkouts == 1 for _count, checkouts in cases)
    )
    revision = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=False,
        cwd=Path(__file__).resolve().parent,
    ).stdout.strip()
    version = subprocess.run(
        [str(binary), "--version"], capture_output=True, text=True, check=False
    ).stdout.strip()

    # A terminating signal must run the case cleanup, not skip it.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))
    result = {
        "date_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "platform": platform.platform(),
        "available_cpus": len(os.sched_getaffinity(0)),
        "revision": revision,
        "binary_version": version,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "mode": args.mode,
        "method": (
            "release binary; loopback mock; no uploads; periodic sync disabled; "
            "synthetic files; one run per case"
        ),
        "kernel_limits": read_limits(),
        "cases": [],
    }
    if args.attach_probe:
        result["attach_probe"] = attach_probe(
            binary, args.mode, args.attach_probe, args.rust_log
        )
        args.output.write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result["attach_probe"]), flush=True)
        return
    watched_values = (False, True) if include_idle else (True,)
    for watched in watched_values:
        for count, checkouts in cases:
            try:
                case = run_case(
                    binary,
                    count,
                    watched,
                    args.workers,
                    args.mode,
                    checkouts if watched else 1,
                    revision,
                    version,
                    args.rust_log,
                )
            except Exception as error:  # noqa: BLE001 - a failed case is a result
                case = failed_case(args.mode, count, checkouts, watched, error)
            result["cases"].append(case)
            args.output.write_text(json.dumps(result, indent=2) + "\n")
            print(json.dumps({key: case.get(key) for key in (
                "mode",
                "processes",
                "checkouts",
                "watched",
                "startup_manifests",
                "edit_manifests",
                "failure",
            )}), flush=True)


if __name__ == "__main__":
    main()
