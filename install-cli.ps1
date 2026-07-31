#!/usr/bin/env pwsh
# semctl installer (Windows / PowerShell).
#
#   irm https://raw.githubusercontent.com/napbat/semctl/main/install-cli.ps1 | iex
#
# Downloads the prebuilt `semctl.exe` from the latest GitHub release to a temp
# dir, verifies its SHA-256, and hands off to `semctl install --all`. The CLI
# installs itself onto a stable PATH location and wires up your AI tools — this
# script only has to fetch the binary. No Rust toolchain needed.
#
# To build from source instead:
#   cargo install --git https://github.com/napbat/semctl --locked semctl
#
# Env override:
#   SEMCTL_RELEASE_BASE  base URL of the release assets
$ErrorActionPreference = 'Stop'

$ReleaseBase = if ($env:SEMCTL_RELEASE_BASE) { $env:SEMCTL_RELEASE_BASE } else { 'https://github.com/napbat/semctl/releases/latest/download' }

function Say($m)  { Write-Host "==> $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "  ! $m" -ForegroundColor Yellow }
function Ok($m)   { Write-Host "  + $m" -ForegroundColor Green }

if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw "no prebuilt semctl for $env:PROCESSOR_ARCHITECTURE — build from source: cargo install --git https://github.com/napbat/semctl --locked semctl"
}
$asset = 'semctl-windows-x64.exe'

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ("semctl-" + [guid]::NewGuid().ToString('N')))
try {
    $bin = Join-Path $tmp 'semctl.exe'
    Say "Downloading $asset from the latest release..."
    Invoke-WebRequest -Uri "$ReleaseBase/$asset" -OutFile $bin -UseBasicParsing

    # Verify SHA-256 against the release manifest (a missing manifest only warns;
    # a MISMATCH aborts).
    $sums = Join-Path $tmp 'checksums.txt'
    $haveChecksums = $true
    try { Invoke-WebRequest -Uri "$ReleaseBase/checksums-sha256.txt" -OutFile $sums -UseBasicParsing }
    catch { $haveChecksums = $false; Warn 'could not fetch checksums - skipping verification' }
    if ($haveChecksums) {
        $line = Select-String -Path $sums -Pattern ([regex]::Escape($asset) + '$') | Select-Object -First 1
        if ($line) {
            $want = (($line.Line -split '\s+') | Where-Object { $_ })[0]
            $got  = (Get-FileHash -Algorithm SHA256 -Path $bin).Hash.ToLower()
            if ($got -ne $want.ToLower()) { throw "sha256 mismatch: expected $want, got $got" }
            Ok 'sha256 verified'
        } else {
            Warn "no checksum entry for $asset - skipping verification"
        }
    }

    # Hand off to the CLI: `install` copies the binary to a stable PATH location
    # and wires up the AI tools it finds (Claude Code, Codex).
    Say 'Installing semctl and wiring your AI tools...'
    & $bin install --all
    if ($LASTEXITCODE -ne 0) {
        throw "semctl install --all failed with exit code $LASTEXITCODE"
    }

    # The CLI persists the user PATH, but as a child process it cannot change
    # this PowerShell process. Refresh the current session so `semctl` works
    # immediately after `irm ... | iex`, without requiring a shell restart.
    $installDir = Join-Path ([Environment]::GetFolderPath(
        [Environment+SpecialFolder]::LocalApplicationData
    )) 'semctl\bin'
    $pathEntries = @($env:Path -split ';' | ForEach-Object { $_.Trim().TrimEnd('\') })
    if ($pathEntries -inotcontains $installDir.TrimEnd('\')) {
        $env:Path = "$installDir;$env:Path"
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
