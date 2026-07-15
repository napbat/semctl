#!/usr/bin/env sh
# semctl installer (Linux / macOS).
#
#   curl -fsSL https://raw.githubusercontent.com/napbat/semctl/main/install-cli.sh | sh
#   # or, if you have wget instead of curl:
#   wget -qO- https://raw.githubusercontent.com/napbat/semctl/main/install-cli.sh | sh
#
# Downloads the prebuilt `semctl` binary from the latest GitHub release to a temp
# dir, verifies its SHA-256, and hands off to `semctl install --all`. The CLI
# installs itself onto a stable PATH location and wires up your AI tools — this
# script only has to fetch the binary. No Rust toolchain needed.
#
# To build from source instead (e.g. a platform with no prebuilt binary):
#   cargo install --git https://github.com/napbat/semctl --locked semctl
#
# Env override:
#   SEMCTL_RELEASE_BASE  base URL of the release assets
#                        (default: https://github.com/napbat/semctl/releases/latest/download)
set -eu

RELEASE_BASE="${SEMCTL_RELEASE_BASE:-https://github.com/napbat/semctl/releases/latest/download}"

say()  { printf '\033[1;36m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33m  ! \033[0m %s\n' "$1"; }
ok()   { printf '\033[1;32m  ✓ \033[0m %s\n' "$1"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

# Pick a downloader that's actually present.
if command -v curl >/dev/null 2>&1; then
	dl() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
	dl() { wget -qO "$2" "$1"; }
else
	die "need curl or wget to download the binary"
fi

# Map this machine to a published release asset.
os="$(uname -s)"
arch="$(uname -m)"
case "$os-$arch" in
	Linux-x86_64)                asset="semctl-linux-x64" ;;
	Darwin-arm64|Darwin-aarch64) asset="semctl-macos-arm64" ;;
	*)
		die "no prebuilt semctl for $os/$arch — build from source:
    cargo install --git https://github.com/napbat/semctl --locked semctl"
		;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "Downloading $asset from the latest release…"
dl "$RELEASE_BASE/$asset" "$tmp/semctl" || die "download failed: $RELEASE_BASE/$asset"

# Verify the SHA-256 against the release manifest (a missing manifest only warns;
# a MISMATCH always aborts).
if dl "$RELEASE_BASE/checksums-sha256.txt" "$tmp/checksums.txt" 2>/dev/null; then
	want="$(grep " $asset\$" "$tmp/checksums.txt" 2>/dev/null | awk '{print $1}')"
	if [ -n "$want" ]; then
		if command -v sha256sum >/dev/null 2>&1; then
			got="$(sha256sum "$tmp/semctl" | awk '{print $1}')"
		elif command -v shasum >/dev/null 2>&1; then
			got="$(shasum -a 256 "$tmp/semctl" | awk '{print $1}')"
		else
			got=""
			warn "no sha256sum/shasum — skipping checksum verification"
		fi
		if [ -n "$got" ]; then
			[ "$got" = "$want" ] || die "sha256 mismatch: expected $want, got $got"
			ok "sha256 verified"
		fi
	else
		warn "no checksum entry for $asset — skipping verification"
	fi
else
	warn "couldn't fetch checksums — skipping verification"
fi

chmod +x "$tmp/semctl"

# Hand off to the CLI: `install` copies the binary to a stable PATH location and
# wires up the AI tools it finds (Claude Code, Codex).
say "Installing semctl and wiring your AI tools…"
"$tmp/semctl" install --all
