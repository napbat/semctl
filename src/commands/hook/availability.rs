//! Whether semctl can currently serve this repo — the gate the nudge fires
//! behind.
//!
//! Deliberately STRICTER than the inherited `codebase::resolve()`, which returns
//! a cached codebase id even when the verifying server GET errors
//! (`codebase.rs:48`) so the MCP server survives a flaky network. For the nudge
//! that leniency is wrong: a server/auth error must mean "unavailable → stay
//! silent", never a stale cache hit that steers the agent at tools currently
//! returning nothing.
//!
//! Two consequences of "strict":
//! - **Only negatives are cached.** A `true` verdict is never reused — every
//!   fire re-probes, so a `semctl auth logout` or outage is respected on the very
//!   next nudge instead of lingering for a TTL. Fires are cooldown-sparse, so
//!   re-probing costs a single GET a few times per segment. Negatives ARE cached
//!   (scoped to the cwd) so a down/logged-out server isn't re-probed on every
//!   fire, and the timeout cost isn't paid repeatedly.
//! - **No git on this path.** The probe only trusts an already-cached codebase
//!   id (verified by one GET). It never falls back to `codebase::resolve()`'s
//!   git remote lookup — a `spawn_blocking` git that outlives the timeout would
//!   still delay process exit (the runtime joins blocking tasks at shutdown),
//!   which would hang the hook. Keeping the probe a pure async GET means the
//!   timeout can actually preempt it. `SessionStart` caches the id, so it is
//!   present by the time `PreToolUse` fires; if it isn't, we stay silent.

use std::path::PathBuf;
use std::time::Duration;

use crate::cli::Cli;
use crate::client::{self, api};

use super::env_parse;
use super::state::{NudgeState, now_secs};

/// Cap on the availability probe. Kept comfortably below the state lock's
/// stale-steal window (`STALE_LOCK_SECS = 10`, state.rs): the lock is held
/// across this `.await`, so the probe must finish well within that window or a
/// parallel process could steal a still-live lock. The probe is a pure async
/// GET (no blocking subprocess — see the module doc), so the timer can always
/// preempt it. A timeout counts as unavailable → the nudge stays silent,
/// honoring the never-break contract.
const PROBE_TIMEOUT_SECS: u64 = 4;

/// Availability, caching only negatives (scoped to `cwd`); updates the cache
/// fields on `state`.
pub async fn is_available_cached(cli: &Cli, cwd: &str, state: &mut NudgeState) -> bool {
    let ttl = env_parse::<u64>("SEMCTX_NUDGE_AVAIL_TTL").unwrap_or(60);
    let now = now_secs();
    if let Some(cached) = cached_verdict(state, now, ttl, cwd) {
        return cached; // a fresh negative for this cwd → silent, no probe
    }
    // Bounded so a stalled server/auth GET can't hang the tool or outlive the
    // lock's stale-steal window; a timeout is "unavailable" → silent.
    let ok = tokio::time::timeout(Duration::from_secs(PROBE_TIMEOUT_SECS), probe(cli, cwd))
        .await
        .unwrap_or(false);
    if ok {
        // Never cache a positive — re-probe each fire so logout/outage is
        // respected immediately (fires are cooldown-sparse, so this is cheap).
        state.avail_checked_at = 0;
    } else {
        state.avail_checked_at = now;
        state.avail_ok = false;
        state.avail_cwd = cwd.to_string();
    }
    ok
}

/// A fresh cached verdict, if any — only ever a cached NEGATIVE for the same
/// `cwd`. Positives are never cached, a future `avail_checked_at` is rejected,
/// and a stale positive from an older state file is ignored. Pure —
/// unit-testable without a server.
fn cached_verdict(state: &NudgeState, now: u64, ttl: u64, cwd: &str) -> Option<bool> {
    let fresh_negative = !state.avail_ok
        && state.avail_checked_at != 0
        && state.avail_checked_at <= now // reject a corrupted future timestamp
        && now.saturating_sub(state.avail_checked_at) < ttl
        && state.avail_cwd == cwd;
    fresh_negative.then_some(false)
}

/// One strict check: `true` only when logged in, the repo maps to an
/// already-cached indexed codebase, and the server currently answers. Never
/// shells out to git (see the module doc): if the id isn't cached, unavailable.
async fn probe(cli: &Cli, cwd: &str) -> bool {
    let Ok(cl) = client::from_cli(cli) else {
        return false;
    };
    let Some(dir) = resolve_dir(cwd) else {
        return false;
    };
    let Some((id, _)) = crate::config::load()
        .ok()
        .and_then(|c| c.cached_codebase_for(&dir))
    else {
        return false; // no cached id → don't resolve via git; stay silent
    };
    matches!(
        cl.get_opt::<api::CodebaseSummary>(&format!("/v1/codebases/{id}"))
            .await,
        Ok(Some(_))
    )
}

fn resolve_dir(cwd: &str) -> Option<PathBuf> {
    if cwd.is_empty() {
        std::env::current_dir().ok()
    } else {
        Some(PathBuf::from(cwd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_negative_is_cached_but_positive_is_not() {
        // A fresh negative for the same cwd is served without probing.
        let neg = NudgeState {
            avail_checked_at: 1000,
            avail_ok: false,
            avail_cwd: "/repo".into(),
            ..Default::default()
        };
        assert_eq!(cached_verdict(&neg, 1030, 60, "/repo"), Some(false));
        // A positive is NEVER served from cache — every fire re-probes.
        let pos = NudgeState {
            avail_checked_at: 1000,
            avail_ok: true,
            avail_cwd: "/repo".into(),
            ..Default::default()
        };
        assert_eq!(cached_verdict(&pos, 1030, 60, "/repo"), None);
    }

    #[test]
    fn negative_cache_expires_is_scoped_and_rejects_future_stamp() {
        let neg = NudgeState {
            avail_checked_at: 1000,
            avail_ok: false,
            avail_cwd: "/repo".into(),
            ..Default::default()
        };
        assert_eq!(cached_verdict(&neg, 1061, 60, "/repo"), None); // 61s > 60s TTL
        assert_eq!(cached_verdict(&neg, 1030, 60, "/other"), None); // different cwd
        assert_eq!(cached_verdict(&neg, 999, 60, "/repo"), None); // now < checked_at → future stamp
    }

    #[test]
    fn never_probed_has_no_cached_verdict() {
        let st = NudgeState::default(); // avail_checked_at == 0
        assert_eq!(cached_verdict(&st, 1000, 60, "/repo"), None);
    }
}
