#!/usr/bin/env python3
"""Measure Linux MCP process overhead with synthetic files and a loopback server."""

import argparse
import collections
import datetime
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import select
import subprocess
import tempfile
import threading
import time


class MockServer(http.server.ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 256

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.lock = threading.Lock()
        self.manifests = []

    def snapshot(self):
        with self.lock:
            return list(self.manifests)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path != "/v1/codebases/benchmark/sync":
            self.send_error(404)
            return
        with self.server.lock:
            self.server.manifests.append(
                {
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


def wait_until(predicate, description, timeout=45):
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() >= deadline:
            raise TimeoutError(description)
        time.sleep(0.05)


def send(process, message):
    process.stdin.write(json.dumps(message).encode() + b"\n")
    process.stdin.flush()


def response(process, request_id):
    deadline = time.monotonic() + 45
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([process.stdout], [], [], remaining)[0]:
            raise TimeoutError(f"MCP response {request_id}")
        line = process.stdout.readline()
        if not line:
            raise RuntimeError(f"MCP process exited: {process.poll()}")
        message = json.loads(line)
        if message.get("id") == request_id:
            if "error" in message:
                raise RuntimeError(message["error"])
            return message["result"]


def process_metrics(process):
    root = Path("/proc") / str(process.pid)
    status = dict(line.split(":", 1) for line in (root / "status").read_text().splitlines())
    memory = dict(
        line.split(":", 1)
        for line in (root / "smaps_rollup").read_text().splitlines()
        if ":" in line
    )
    memory_kib = lambda key: int(memory.get(key, "0 kB").split()[0])
    fields = (root / "stat").read_text().split(")", 1)[1].split()
    watches = 0
    for entry in (root / "fdinfo").iterdir():
        try:
            watches += sum(line.startswith("inotify wd:") for line in entry.read_text().splitlines())
        except FileNotFoundError:
            continue
    return {
        "rss_kib": memory_kib("Rss"),
        "pss_kib": memory_kib("Pss"),
        "uss_kib": memory_kib("Private_Clean") + memory_kib("Private_Dirty"),
        "threads": int(status["Threads"]),
        "fds": len(list((root / "fd").iterdir())),
        "inotify_watches": watches,
        "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
    }


def totals(processes):
    result = collections.Counter()
    for process in processes:
        result.update(process_metrics(process))
    return dict(result)


def run_case(binary, count, watched, workers):
    with tempfile.TemporaryDirectory(prefix="semctl-mcp-measure-") as temporary:
        base = Path(temporary)
        checkout = base / "checkout"
        checkout.mkdir()
        config = base / "config" / "semctl"
        config.mkdir(parents=True)
        git_config = base / "gitconfig"
        git_config.write_text("")
        if watched:
            content = "// synthetic benchmark source\n" + "let x = 1;\n" * 369
            for index in range(2000):
                directory = checkout / f"d{index // 100:02}"
                directory.mkdir(exist_ok=True)
                (directory / f"f{index:04}.rs").write_text(content)
            (config / "config.toml").write_text(
                f'[codebase_cache]\n{json.dumps(str(checkout))} = "benchmark"\n'
            )
            (config / "installation-id").write_text("a" * 64 + "\n")

        server = MockServer()
        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()
        environment = {key: os.environ[key] for key in ("PATH", "HOME", "LANG") if key in os.environ}
        environment.update(
            XDG_CONFIG_HOME=str(base / "config"),
            SEMCTX_SERVER=f"http://127.0.0.1:{server.server_port}",
            SEMCTX_TOKEN="synthetic-benchmark-token",
            SEMCTX_MCP_UPDATE_CHECK="0",
            SEMCTX_MCP_RESYNC_SECS="0",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=str(git_config),
            RUST_LOG="info",
        )
        if workers:
            environment["TOKIO_WORKER_THREADS"] = str(workers)
        processes = []
        logs = []
        try:
            started = time.monotonic()
            for index in range(count):
                log = (base / f"process-{index}.log").open("w+")
                logs.append(log)
                process = subprocess.Popen(
                    [str(binary), "--codebase", "benchmark", "mcp"],
                    cwd=checkout,
                    env=environment,
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=log,
                    bufsize=0,
                )
                processes.append(process)
                send(process, {
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                               "clientInfo": {"name": "isolated-measurement", "version": "1"}},
                })
            for process in processes:
                response(process, 1)
                send(process, {"jsonrpc": "2.0", "method": "notifications/initialized"})
                send(process, {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
            tool_counts = [len(response(process, 2)["tools"]) for process in processes]
            attach_seconds = time.monotonic() - started
            if watched:
                wait_until(lambda: len(server.snapshot()) >= count, "startup manifests")
                wait_until(
                    lambda: all("fs watcher active" in Path(log.name).read_text() for log in logs),
                    "watcher registration",
                )
            time.sleep(2)
            before = totals(processes)
            startup = server.snapshot()
            edit_manifests = []
            edit_cpu_seconds = None
            if watched:
                path = checkout / "d00" / "f0000.rs"
                with path.open("a") as file:
                    file.write("// one edit\n")
                wait_until(lambda: len(server.snapshot()) >= len(startup) + count, "edit manifests")
                time.sleep(2)
                after = totals(processes)
                edit_cpu_seconds = round(after["cpu_seconds"] - before["cpu_seconds"], 3)
                edit_manifests = server.snapshot()[len(startup):]
            if any(process.poll() is not None for process in processes):
                raise RuntimeError("A measured process exited before sampling completed")
            return {
                "processes": count,
                "watched": watched,
                "tokio_worker_threads": workers or "default",
                "attach_batch_seconds": round(attach_seconds, 3),
                "tools_per_process": sorted(set(tool_counts)),
                "settled_totals": before,
                "startup_manifests": len(startup),
                "startup_manifest_bytes": sum(item["bytes"] for item in startup),
                "startup_manifest_file_counts": sorted(set(item["files"] for item in startup)),
                "distinct_source_ids": len(set(item["source_id"] for item in startup)),
                "edit_manifests": len(edit_manifests),
                "edit_manifest_bytes": sum(item["bytes"] for item in edit_manifests),
                "edit_process_cpu_seconds": edit_cpu_seconds,
            }
        finally:
            for process in processes:
                if process.poll() is None:
                    process.terminate()
            for process in processes:
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                process.stdin.close()
                process.stdout.close()
            for log in logs:
                log.close()
            server.shutdown()
            server.server_close()
            server_thread.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--counts", type=int, nargs="+", default=[1, 30, 100])
    parser.add_argument("--workers", type=int, default=0)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    result = {
        "date_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "platform": platform.platform(),
        "available_cpus": len(os.sched_getaffinity(0)),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "method": "release binary; loopback mock; no uploads; periodic sync disabled; synthetic files",
        "cases": [],
    }
    for watched in (False, True):
        for count in args.counts:
            case = run_case(binary, count, watched, args.workers)
            result["cases"].append(case)
            args.output.write_text(json.dumps(result, indent=2) + "\n")
            print(json.dumps(case), flush=True)


if __name__ == "__main__":
    main()
