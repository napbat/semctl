//! `semctl upgrade` — replace the running `semctl` binary with the latest build.
//!
//! Always server-mediated: **ask the server for a download link → download it →
//! swap the running binary.** There is no CLI-supplied URL; semctl is private, so
//! the server is the source of truth for where the binary lives (it hands back a
//! public link — presigned or plain — that it can repoint at will).
//!
//! All three steps are implemented CLI-side:
//! - **Resolve URL from server ([`resolve_download`]).** Anonymous GET of
//!   `/v1/cli/latest?target={triple}` → `{ version, url, sha256 }`.
//! - **Download + verify + extract ([`download_binary`]).** Plain public GET (no
//!   auth), SHA-256 checked against the downloaded artifact, then the bare
//!   executable is staged — the URL may serve a raw binary or a release archive
//!   (`.tar.gz` / `.zip`) and both are handled.
//! - **Swap ([`self_replace`]).** Cross-platform: rename-the-live-image on
//!   Windows, replace-by-inode on Unix.
//!
//! Publishing is wired end-to-end: `.github/workflows/release.yml` runs on every
//! push to `main` but builds + publishes a GitHub Release only when Cargo.toml's
//! `version` is bumped to a not-yet-released value (tagged `v<version>`). The
//! server reads that release with its git token (see
//! `CliReleaseService`) and hands back a relative download path it 302-redirects to
//! a short-lived signed CDN URL — so the binary stays private but the download
//! needs no auth. Unconfigured (before the first release, or the git token lacks
//! repo access) → the endpoint 404s and `semctl upgrade` reports "nothing published".

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::cli::Cli;
use crate::commands::install::{InstallKind, install_kind};
use crate::config;
use crate::term::{ok, say};

pub async fn run(cli: &Cli) -> Result<()> {
    // A cargo-installed binary is managed by cargo; self-replacing it in place
    // would leave cargo's own metadata pointing at a binary it no longer built.
    // Defer to cargo instead of downloading over it.
    if install_kind() == InstallKind::Cargo {
        say("This semctl was installed with cargo — update it with cargo:");
        println!(
            "    cargo install --git https://github.com/napbat/semctl --locked --force semctl"
        );
        return Ok(());
    }

    let current = std::env::current_exe().context("locate the running semctl binary")?;
    let target = release_target()?;

    // 1. Ask the server where to get the latest binary for this platform.
    let dl = resolve_download(cli.server.as_deref(), target).await?;

    // Compare by version *precedence*, not raw string equality. The server's
    // "latest" is whatever GitHub flags as the latest release, which can move
    // backwards when a release is yanked — so treating "different" as "newer" would
    // silently downgrade the running binary. Only a strictly-newer release upgrades;
    // an equal or older one is a no-op, never a downgrade.
    let running = env!("CARGO_PKG_VERSION");
    let cmp = compare_versions(&dl.version, running);
    if cmp == VersionCmp::Same || (cmp == VersionCmp::Unknown && dl.version == running) {
        ok(&format!(
            "semctl is already at the latest version (v{running})."
        ));
        return Ok(());
    }
    if cmp == VersionCmp::Older {
        ok(&format!(
            "semctl (v{running}) is newer than the latest published release (v{}); nothing to do.",
            dl.version
        ));
        return Ok(());
    }

    // 2. Download it, verifying the checksum. Stage next to the current binary so
    //    the swap stays on the same volume (a rename, never a cross-device copy).
    say(&format!(
        "Updating semctl v{} → v{} for {target}…",
        env!("CARGO_PKG_VERSION"),
        dl.version
    ));
    let dir = current.parent().unwrap_or_else(|| Path::new("."));
    let staged = download_binary(&dl.url, dir, Some(&dl.sha256)).await?;
    #[cfg(unix)]
    ensure_executable(&staged)?;

    // 3. Swap the live executable.
    self_replace::self_replace(&staged)
        .with_context(|| format!("replace {}", current.display()))?;
    let _ = std::fs::remove_file(&staged); // best-effort: self_replace may already have consumed it

    ok(&format!(
        "semctl updated to v{} at {}",
        dl.version,
        current.display()
    ));
    println!("      Restart any running `semctl mcp` / Claude Code session to pick it up.");
    Ok(())
}

/// Where to fetch the latest binary, as told by the server (the `data` payload
/// of `GET /v1/cli/latest`). Field names are the server's camelCase JSON.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Download {
    version: String,
    url: String,
    sha256: String,
}

/// The server's `ApiResponse<T>` envelope — we only need `data`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    data: Option<Download>,
}

/// Ask the server where to get the latest binary for this platform. Anonymous —
/// the endpoint is public (self-update mustn't hinge on a live session), so no
/// token is sent. The server hands back a public download URL it can repoint.
async fn resolve_download(server_override: Option<&str>, target: &str) -> Result<Download> {
    let server = config::load()?.server_url(server_override);
    let server = server.trim_end_matches('/');
    let url = format!("{server}/v1/cli/latest?target={target}");

    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("no semctl release is published for {target} yet");
    }
    let resp = resp
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let env: Envelope = resp.json().await.context("parse /v1/cli/latest response")?;
    let mut dl = env
        .data
        .ok_or_else(|| anyhow!("/v1/cli/latest returned no release data"))?;
    // The server hands back a relative download path (`/v1/cli/download/{target}`)
    // that it 302-redirects to a short-lived signed CDN URL; make it absolute
    // against the server so the plain GET in `download_binary` resolves.
    if !dl.url.starts_with("http://") && !dl.url.starts_with("https://") {
        dl.url = format!("{server}/{}", dl.url.trim_start_matches('/'));
    }
    Ok(dl)
}

/// Best-effort "is a newer CLI published?" check for the background update prompt
/// in `semctl mcp`. Returns the newer version string when the server advertises
/// one for this platform, else `None` — already latest, an unsupported platform,
/// the endpoint unconfigured (before the first release), or any network error all
/// collapse to `None`, since this is advisory only. It never touches the binary;
/// applying the update stays the explicit `semctl upgrade`.
pub(crate) async fn check_for_update(server_override: Option<&str>) -> Option<String> {
    let target = release_target().ok()?;
    let dl = resolve_download(server_override, target).await.ok()?;
    is_upgrade(&dl.version, env!("CARGO_PKG_VERSION")).then_some(dl.version)
}

/// How a published version relates to the running one, by numeric
/// `(major, minor, patch)` precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionCmp {
    /// Published version is strictly newer than the running one.
    Newer,
    /// Same version.
    Same,
    /// Published version is older than the running one (e.g. a yanked release).
    Older,
    /// One side wasn't a plain `x.y.z` and couldn't be compared.
    Unknown,
}

/// Is `published` an upgrade over `running`? Strictly-newer is; equal or older is
/// not. When either side can't be parsed we fall back to raw inequality — matching
/// the pre-precedence behaviour so an unusual version string still surfaces.
fn is_upgrade(published: &str, running: &str) -> bool {
    match compare_versions(published, running) {
        VersionCmp::Newer => true,
        VersionCmp::Same | VersionCmp::Older => false,
        VersionCmp::Unknown => published != running,
    }
}

/// Compare a published version against the running one by `(major, minor, patch)`
/// precedence. Any `-prerelease` / `+build` suffix is ignored — releases publish
/// plain `x.y.z` tags and `/releases/latest` never returns a prerelease — and an
/// unparseable side yields [`VersionCmp::Unknown`].
fn compare_versions(published: &str, running: &str) -> VersionCmp {
    match (parse_version(published), parse_version(running)) {
        (Some(p), Some(r)) => match p.cmp(&r) {
            Ordering::Greater => VersionCmp::Newer,
            Ordering::Equal => VersionCmp::Same,
            Ordering::Less => VersionCmp::Older,
        },
        _ => VersionCmp::Unknown,
    }
}

/// Parse a plain `major.minor.patch` version into a comparable tuple, dropping any
/// `-prerelease` / `+build` suffix. Returns `None` unless exactly three numeric
/// components are present.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None; // more than three components — not a plain x.y.z
    }
    Some((major, minor, patch))
}

/// Download from a public URL into `dir`, verifying its SHA-256 if one is given,
/// and return the staged path. No auth — the URL is public (presigned or plain),
/// exactly what [`resolve_download`] hands back. The URL may serve a raw binary
/// or a release archive (`.tar.gz` / `.zip`); either way the staged file is the
/// bare executable.
async fn download_binary(url: &str, dir: &Path, expected_sha256: Option<&str>) -> Result<PathBuf> {
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("download {url}"))?;
    let bytes = resp.bytes().await.context("read download body")?;

    // Verify the checksum against the downloaded artifact as-is — the release
    // publishes `sha256sum` over whatever the URL serves (archive or raw binary),
    // so this must run before any extraction.
    if let Some(want) = expected_sha256 {
        let got = sha256_hex(&bytes);
        if !got.eq_ignore_ascii_case(want) {
            bail!("sha256 mismatch for {url}: expected {want}, got {got}");
        }
        ok(&format!("sha256 verified ({got})"));
    }

    let staged = dir.join(format!("{}.new", bin_name()));
    extract_binary(&bytes, &staged).with_context(|| format!("extract semctl binary from {url}"))?;
    Ok(staged)
}

/// Write the bare `semctl` executable to `staged` from downloaded bytes, which
/// may be the raw binary or a release archive. Format is detected by magic bytes;
/// for archives we stream out just the `semctl` entry (never extract to disk).
fn extract_binary(bytes: &[u8], staged: &Path) -> Result<()> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        // gzip → the unix release `.tar.gz`.
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in archive.entries().context("read tar")? {
            let mut entry = entry.context("read tar entry")?;
            let name = entry.path().context("tar entry path")?.into_owned();
            if is_semctl_entry(name.file_name().and_then(|n| n.to_str())) {
                let mut out = std::fs::File::create(staged)
                    .with_context(|| format!("create {}", staged.display()))?;
                std::io::copy(&mut entry, &mut out).context("unpack binary from tar.gz")?;
                return Ok(());
            }
        }
        bail!("no `{}` entry inside the downloaded .tar.gz", bin_name());
    } else if bytes.starts_with(b"PK\x03\x04") {
        // zip → the Windows release `.zip`.
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("open zip")?;
        for i in 0..zip.len() {
            let mut file = zip.by_index(i).context("read zip entry")?;
            let base = file
                .name()
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(file.name());
            if is_semctl_entry(Some(base)) {
                let mut out = std::fs::File::create(staged)
                    .with_context(|| format!("create {}", staged.display()))?;
                std::io::copy(&mut file, &mut out).context("unpack binary from zip")?;
                return Ok(());
            }
        }
        bail!("no `{}` entry inside the downloaded .zip", bin_name());
    }
    // Raw executable.
    std::fs::write(staged, bytes).with_context(|| format!("write {}", staged.display()))?;
    Ok(())
}

/// Does this archive entry's file name look like the semctl binary? Accepts the
/// platform's expected name and the bare `semctl`/`semctl.exe` either way.
fn is_semctl_entry(base: Option<&str>) -> bool {
    matches!(base, Some("semctl" | "semctl.exe"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// The friendly platform tag for this machine — the operator-facing key the
/// server's `CliReleaseSettings.Assets` is keyed by (and the same tag the portal
/// install panel shows). Sent as `?target=`. Errors on any platform not published.
fn release_target() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x64",
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x64",
        ("windows", "x86_64") => "windows-x64",
        (os, arch) => bail!(
            "no prebuilt semctl binary for {os}/{arch} — build from source: \
                 `cargo install --git https://github.com/napbat/semctl --locked semctl`"
        ),
    })
}

fn bin_name() -> &'static str {
    if cfg!(windows) {
        "semctl.exe"
    } else {
        "semctl"
    }
}

/// A freshly written/copied file may lack the execute bit on Unix; set it.
#[cfg(unix)]
fn ensure_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(path, perm).with_context(|| format!("chmod +x {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{VersionCmp, compare_versions, is_upgrade, parse_version};

    #[test]
    fn parses_plain_semver() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.1.10"), Some((0, 1, 10)));
    }

    #[test]
    fn parse_ignores_prerelease_and_build_suffix() {
        assert_eq!(parse_version("0.2.0-rc.1"), Some((0, 2, 0)));
        assert_eq!(parse_version("0.2.0+build.5"), Some((0, 2, 0)));
    }

    #[test]
    fn parse_rejects_non_semver() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("v1.2.3"), None); // callers strip the leading `v`
        assert_eq!(parse_version("nightly"), None);
    }

    #[test]
    fn compares_by_precedence_not_string() {
        assert_eq!(compare_versions("0.2.0", "0.1.5"), VersionCmp::Newer);
        assert_eq!(compare_versions("0.1.0", "0.1.0"), VersionCmp::Same);
        assert_eq!(compare_versions("0.1.5", "0.2.0"), VersionCmp::Older);
        // A naive string compare orders "0.1.10" < "0.1.9"; precedence must not.
        assert_eq!(compare_versions("0.1.10", "0.1.9"), VersionCmp::Newer);
    }

    #[test]
    fn unparseable_side_is_unknown() {
        assert_eq!(compare_versions("weird", "0.1.0"), VersionCmp::Unknown);
        assert_eq!(compare_versions("0.1.0", "weird"), VersionCmp::Unknown);
    }

    #[test]
    fn is_upgrade_only_on_strictly_newer() {
        assert!(is_upgrade("0.2.0", "0.1.0"));
        assert!(!is_upgrade("0.1.0", "0.1.0"));
        // Yanked-release downgrade must NOT read as an upgrade.
        assert!(!is_upgrade("0.1.5", "0.2.0"));
    }

    #[test]
    fn is_upgrade_falls_back_to_inequality_when_unparseable() {
        assert!(is_upgrade("custom-a", "custom-b"));
        assert!(!is_upgrade("custom", "custom"));
    }
}
