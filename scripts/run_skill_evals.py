#!/usr/bin/env python3
"""Replay the retrieval-skill golden prompts in fresh Codex or Claude sessions.

This runner records raw JSONL events and a manifest for review. It deliberately
does not ask a model to grade itself; CI or a human reviewer can compare tool
selection, arguments, errors, and final output with expected_behavior.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PLUGIN_ROOT = ROOT / "plugins" / "semctx"
EVALS = PLUGIN_ROOT / "skills" / "codebase-retrieval" / "evals.json"
DEFAULT_RESULTS = ROOT / "target" / "skill-evals"


@dataclass(frozen=True)
class HostAdapter:
    executable: str
    build_command: Callable[[str], list[str]]
    preflight: Callable[[], tuple[bool, str]] | None = None


def cases(document: dict) -> list[dict]:
    result: list[dict] = []
    for index, case in enumerate(document["functional"], start=1):
        result.append(
            {
                "id": f"functional-{index:02d}",
                "kind": "functional",
                "query": case["query"],
                "expected_behavior": case["expected_behavior"],
            }
        )
    for kind in ("should_trigger", "should_not_trigger"):
        for index, query in enumerate(document["triggering"][kind], start=1):
            result.append(
                {
                    "id": f"{kind.replace('_', '-')}-{index:02d}",
                    "kind": kind,
                    "query": query,
                    "expected_behavior": [kind.replace("_", " ")],
                }
            )
    return result


def codex_command(query: str) -> list[str]:
    return [
        "codex",
        "exec",
        "--ephemeral",
        "--json",
        "--sandbox",
        "read-only",
        "--cd",
        str(ROOT),
        query,
    ]


def claude_command(query: str) -> list[str]:
    return [
        "claude",
        "--print",
        "--verbose",
        "--output-format",
        "stream-json",
        "--include-hook-events",
        "--no-session-persistence",
        "--permission-mode",
        "plan",
        "--plugin-dir",
        str(PLUGIN_ROOT),
        query,
    ]

def omp_command(query: str) -> list[str]:
    return [
        "omp",
        "--mode",
        "json",
        "--print",
        "--no-session",
        "--approval-mode",
        "yolo",
        "--tools",
        "read,grep,glob,lsp",
        "--plugin-dir",
        str(PLUGIN_ROOT),
        query,
    ]


def codex_plugin_is_current() -> tuple[bool, str]:
    expected = json.loads(
        (PLUGIN_ROOT / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8")
    )["version"]
    listed = subprocess.run(
        ["codex", "plugin", "list", "--json"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if listed.returncode:
        return False, f"`codex plugin list --json` failed: {listed.stderr.strip()}"
    try:
        plugins = json.loads(listed.stdout)["installed"]
        installed = next(item for item in plugins if item["pluginId"] == "semctx@semctx")
    except (json.JSONDecodeError, KeyError, StopIteration) as error:
        return False, f"cannot find installed semctx@semctx plugin: {error}"
    actual = installed.get("version")
    return (
        actual == expected,
        f"installed semctx@semctx is {actual}; worktree package is {expected}",
    )


HOST_ADAPTERS = {
    "codex": HostAdapter("codex", codex_command, codex_plugin_is_current),
    "claude": HostAdapter("claude", claude_command),
    "omp": HostAdapter("omp", omp_command),
}


def command(host: str, query: str) -> list[str]:
    return HOST_ADAPTERS[host].build_command(query)


def display_path(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", choices=(*HOST_ADAPTERS, "all"), default="all")
    parser.add_argument("--case", help="run one case id")
    parser.add_argument("--results-dir", type=Path, default=DEFAULT_RESULTS)
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--allow-stale-plugin",
        action="store_true",
        help="record Codex runs even when its installed plugin version differs",
    )
    args = parser.parse_args()

    document = json.loads(EVALS.read_text(encoding="utf-8"))
    unknown_hosts = set(document["hosts"]) - HOST_ADAPTERS.keys()
    if unknown_hosts:
        print(
            f"evals.json names hosts with no runner adapter: {sorted(unknown_hosts)}",
            file=sys.stderr,
        )
        return 2
    hosts = document["hosts"] if args.host == "all" else [args.host]
    selected = [case for case in cases(document) if not args.case or case["id"] == args.case]
    if not selected:
        print(f"unknown case: {args.case}", file=sys.stderr)
        return 2

    for host in hosts:
        executable = HOST_ADAPTERS[host].executable
        if not shutil.which(executable):
            print(f"{executable} CLI is not on PATH", file=sys.stderr)
            return 2
    if not args.allow_stale_plugin:
        for host in hosts:
            preflight = HOST_ADAPTERS[host].preflight
            if preflight is None:
                continue
            current, detail = preflight()
            if current:
                continue
            print(
                f"{detail}. Update/reinstall the {host} plugin before evaluating, "
                "or pass --allow-stale-plugin for a deliberate baseline.",
                file=sys.stderr,
            )
            return 2

    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    run_dir = args.results_dir / stamp
    manifest = {
        "started_at": datetime.now(UTC).isoformat(),
        "hosts": hosts,
        "cases": [],
    }
    failed = False

    for host in hosts:
        for case in selected:
            cmd = command(host, case["query"])
            record = {**case, "host": host, "command": cmd}
            if args.dry_run:
                # Keep stdout portable to Windows consoles whose active encoding
                # cannot represent every fixture character. Recorded files below
                # remain UTF-8 with readable Unicode.
                print(json.dumps(record, ensure_ascii=True))
                continue

            run_dir.mkdir(parents=True, exist_ok=True)
            output_path = run_dir / f"{host}-{case['id']}.jsonl"
            started = time.monotonic()
            try:
                completed = subprocess.run(
                    cmd,
                    cwd=ROOT,
                    capture_output=True,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                    timeout=args.timeout,
                    check=False,
                )
                record["exit_code"] = completed.returncode
                record["duration_seconds"] = round(time.monotonic() - started, 3)
                record["stderr"] = completed.stderr
                output_path.write_text(completed.stdout, encoding="utf-8")
                failed = failed or completed.returncode != 0
            except subprocess.TimeoutExpired as error:
                record["exit_code"] = None
                record["duration_seconds"] = round(time.monotonic() - started, 3)
                record["error"] = f"timed out after {args.timeout}s"
                timed_out_output = error.stdout or ""
                if isinstance(timed_out_output, bytes):
                    timed_out_output = timed_out_output.decode("utf-8", errors="replace")
                output_path.write_text(timed_out_output, encoding="utf-8")
                failed = True
            record["events"] = display_path(output_path)
            manifest["cases"].append(record)
            print(f"{host} {case['id']}: {record['exit_code']}")

    if not args.dry_run:
        manifest["completed_at"] = datetime.now(UTC).isoformat()
        (run_dir / "manifest.json").write_text(
            json.dumps(manifest, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        print(f"results: {run_dir}")
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
