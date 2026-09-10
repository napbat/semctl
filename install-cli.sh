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

# Require one valid entry before the downloaded binary can execute.
dl "$RELEASE_BASE/checksums-sha256.txt" "$tmp/checksums.txt" \
	|| die "cannot download checksum manifest"
want="$(awk -v asset="$asset" '
	$2 == asset || $2 == "*" asset {
		matches++
		if (NF != 2 || length($1) != 64 || $1 ~ /[^0-9a-fA-F]/) invalid = 1
		hash = tolower($1)
	}
	END {
		if (matches != 1 || invalid) exit 1
		print hash
	}
' "$tmp/checksums.txt")" || die "checksum manifest must contain one valid SHA-256 entry for $asset"

# Capture the command status before parsing output. A pipeline can hide failure.
if command -v sha256sum >/dev/null 2>&1; then
	checksum="$(sha256sum "$tmp/semctl")" || die "sha256sum failed"
elif command -v shasum >/dev/null 2>&1; then
	checksum="$(shasum -a 256 "$tmp/semctl")" || die "shasum failed"
else
	die "need sha256sum or shasum to verify the binary"
fi
got="${checksum%% *}"
[ "$got" = "$want" ] || die "SHA-256 mismatch: expected $want, got $got"
ok "SHA-256 verified"

chmod +x "$tmp/semctl"

# Hand off to the CLI: `install` copies the binary to a stable PATH location and
# wires up the AI tools it finds (Claude Code, Codex).
say "Installing semctl and wiring your AI tools…"
"$tmp/semctl" install --all
