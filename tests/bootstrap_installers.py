#!/usr/bin/env python3
"""Test bootstrap verification with local downloads and a harmless executable."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SHELLS: list[str] = []
PYTHON = Path(sys.executable).resolve()
DIGEST = hashlib.sha256(PYTHON.read_bytes()).hexdigest()


def executable(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(0o755)


def shell_fixture(directory: Path, checksum_tool: str) -> list[str]:
    tools = directory / "tools"
    tools.mkdir()
    for name in ("awk", "chmod", "cp", "mktemp", "rm"):
        command = shutil.which(name)
        if command is None:
            raise RuntimeError(f"shell fixture requires {name}")
        executable(tools / name, f"#!/bin/sh\nexec {shlex.quote(Path(command).as_posix())} \"$@\"\n")
    executable(
        tools / "uname",
        '#!/bin/sh\ncase "$1" in -s) printf "Linux\\n";; -m) printf "x86_64\\n";; esac\n',
    )
    executable(
        tools / "curl",
        '#!/bin/sh\ncase "$2" in\n'
        '  */checksums-sha256.txt) cp "$SEMCTL_TEST_MANIFEST" "$4";;\n'
        '  */semctl-linux-x64) cp "$SEMCTL_TEST_BINARY" "$4";;\n'
        '  *) exit 22;;\nesac\n',
    )
    if checksum_tool != "missing":
        name = "shasum" if checksum_tool == "shasum" else "sha256sum"
        executable(
            tools / name,
            '#!/bin/sh\nprintf "%s  fixture\\n" "$SEMCTL_TEST_DIGEST"\n'
            + ("exit 9\n" if checksum_tool == "failed" else "exit 0\n"),
        )
    return [shutil.which("sh") or "sh", str(ROOT / "install-cli.sh")]


def powershell_fixture(directory: Path, checksum_tool: str) -> list[str]:
    script = directory / "harness.ps1"
    chmod = "" if os.name == "nt" else "        & /bin/chmod +x $OutFile\n"
    content = """$ErrorActionPreference = 'Stop'
$env:PROCESSOR_ARCHITECTURE = 'AMD64'
function Invoke-WebRequest {
    param($Uri, $OutFile, [switch]$UseBasicParsing)
    if ($Uri.EndsWith('/checksums-sha256.txt')) {
        Copy-Item -LiteralPath $env:SEMCTL_TEST_MANIFEST -Destination $OutFile
    } elseif ($Uri.EndsWith('/semctl-windows-x64.exe')) {
        Copy-Item -LiteralPath $env:SEMCTL_TEST_BINARY -Destination $OutFile
""" + chmod + """    } else {
        throw "unexpected fixture URL: $Uri"
    }
}
"""
    if checksum_tool == "missing":
        content += "function Get-FileHash { throw [System.Management.Automation.CommandNotFoundException]::new('checksum tool missing') }\n"
    elif checksum_tool == "failed":
        content += "function Get-FileHash { throw 'checksum tool failed' }\n"
    content += "& (Join-Path $env:SEMCTL_TEST_ROOT 'install-cli.ps1')\n"
    script.write_text(content, encoding="utf-8")
    command = shutil.which("pwsh") or shutil.which("powershell") or "pwsh"
    return [command, "-NoLogo", "-NoProfile", "-NonInteractive", "-File", str(script)]


class BootstrapInstallers(unittest.TestCase):
    def run_case(
        self,
        shell: str,
        manifest: str | None,
        *,
        success: bool = False,
        checksum_tool: str = "present",
        binary_present: bool = True,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="semctl-bootstrap-test-") as temporary:
            directory = Path(temporary)
            binary = directory / "download"
            if binary_present:
                shutil.copyfile(PYTHON, binary)
            manifest_path = directory / "manifest"
            if manifest is not None:
                asset = "semctl-linux-x64" if shell == "sh" else "semctl-windows-x64.exe"
                manifest_path.write_text(manifest.replace("ASSET", asset), encoding="utf-8")
            # The downloaded Python executable receives the real install arguments.
            # Its script writes only inside this isolated working directory.
            (directory / "install").write_text(
                "import pathlib, sys\nassert sys.argv[1:] == ['--all']\n"
                "pathlib.Path('invoked').write_text('verified binary executed')\n",
                encoding="utf-8",
            )
            environment = dict(
                os.environ,
                SEMCTL_RELEASE_BASE="https://bootstrap.invalid/assets",
                SEMCTL_TEST_ROOT=str(ROOT),
                SEMCTL_TEST_BINARY=str(binary),
                SEMCTL_TEST_MANIFEST=str(manifest_path),
                SEMCTL_TEST_DIGEST=DIGEST,
                PYTHONHOME=sys.base_prefix,
                TMPDIR=str(directory),
                TMP=str(directory),
                TEMP=str(directory),
            )
            if shell == "sh":
                command = shell_fixture(directory, checksum_tool)
                environment["PATH"] = str(directory / "tools")
                if os.name == "nt":
                    environment["PATH"] += os.pathsep + str(PYTHON.parent)
            else:
                command = powershell_fixture(directory, checksum_tool)
                environment["PATH"] = str(PYTHON.parent) + os.pathsep + environment.get("PATH", "")
            result = subprocess.run(
                command,
                cwd=directory,
                env=environment,
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
            detail = result.stdout + result.stderr
            self.assertEqual(result.returncode == 0, success, detail)
            self.assertEqual((directory / "invoked").exists(), success, detail)

    def test_only_verified_binaries_execute(self) -> None:
        manifests = {
            "unavailable": None,
            "empty": "",
            "missing asset": f"{DIGEST}  other-binary\n",
            "filename suffix": f"{DIGEST}  other-ASSET\n",
            "short digest": "abcd  ASSET\n",
            "invalid digest": f"{'z' * 64}  ASSET\n",
            "extra field": f"{DIGEST}  ASSET  extra\n",
            "duplicate entry": f"{DIGEST}  ASSET\n{DIGEST}  ASSET\n",
            "conflicting entry": f"{DIGEST}  ASSET\n{'0' * 64}  ASSET\n",
            "hash mismatch": f"{'0' * 64}  ASSET\n",
        }
        for shell in SHELLS:
            for name, manifest in manifests.items():
                with self.subTest(shell=shell, case=name):
                    self.run_case(shell, manifest)

    def test_checksum_tool_failures_prevent_execution(self) -> None:
        for shell in SHELLS:
            for checksum_tool in ("missing", "failed"):
                with self.subTest(shell=shell, checksum_tool=checksum_tool):
                    self.run_case(shell, f"{DIGEST}  ASSET\n", checksum_tool=checksum_tool)

    def test_binary_download_failure_prevents_execution(self) -> None:
        for shell in SHELLS:
            with self.subTest(shell=shell):
                self.run_case(shell, f"{DIGEST}  ASSET\n", binary_present=False)

    def test_valid_text_and_binary_checksum_entries_allow_execution(self) -> None:
        for shell in SHELLS:
            for manifest in (f"{DIGEST}  ASSET\n", f"{DIGEST.upper()} *ASSET\n"):
                with self.subTest(shell=shell, manifest=manifest):
                    self.run_case(shell, manifest, success=True)

    def test_shasum_fallback_allows_verified_execution(self) -> None:
        if "sh" in SHELLS:
            self.run_case("sh", f"{DIGEST}  ASSET\n", checksum_tool="shasum", success=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shell", choices=("sh", "pwsh", "all"), default="all")
    arguments = parser.parse_args()
    SHELLS = ["sh", "pwsh"] if arguments.shell == "all" else [arguments.shell]
    for selected in SHELLS:
        if shutil.which(selected) is None and not (
            selected == "pwsh" and shutil.which("powershell") is not None
        ):
            parser.error(f"{selected} is required; use --shell to select an available shell")
    unittest.main(argv=[sys.argv[0]], verbosity=2)
