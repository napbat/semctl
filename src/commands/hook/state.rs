//! Per-session nudge state: the running counts that drive escalation, the
//! availability TTL cache, and per-`prompt_id` dedup. Lives in a throwaway file
//! under the OS temp dir, keyed by session id.
//!
//! Concurrency (Codex MAJOR): Claude can fire parallel `PreToolUse` calls, so the
//! read-modify-write is guarded by a per-session try-lock. If the lock can't be
//! taken the caller **skips the nudge** — it never blocks the tool. Everything
//! here is best-effort and must never panic (the never-break-a-session
//! contract): all IO errors degrade to "no state" / "no nudge".

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Seconds after which a leftover lock (crashed mid-critical-section) is stolen.
const STALE_LOCK_SECS: u64 = 10;
/// Age at which an idle session's state file is pruned.
const CLEANUP_AGE_SECS: u64 = 24 * 60 * 60;
/// Cap on remembered prompt ids (dedup ring — bounds file growth).
const MAX_PROMPT_IDS: usize = 64;
/// Refuse to read a state file larger than this — our own writes are a few KB,
/// so anything bigger is corrupt/hostile and must degrade to default state
/// rather than allocate/hang (never-break).
const MAX_STATE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NudgeState {
    pub eligible_count: u32,
    pub nudges_fired: u32,
    pub last_nudge_at_count: u32,
    #[serde(default)]
    pub nudged_prompt_ids: Vec<String>,
    #[serde(default)]
    pub avail_checked_at: u64,
    #[serde(default)]
    pub avail_ok: bool,
    /// The cwd the availability verdict was measured for — the cache is only
    /// honored when this matches the current call's cwd.
    #[serde(default)]
    pub avail_cwd: String,
    /// Last successful advisory CLI-version check for this agent session.
    #[serde(default)]
    pub update_checked_at: u64,
    /// Newer published version from that check; empty means the running CLI was
    /// current. Kept separate from the timestamp so a current result is cached.
    #[serde(default)]
    pub update_latest_version: String,
    /// Whether this session already received the update instruction.
    #[serde(default)]
    pub update_notice_emitted: bool,
}

impl NudgeState {
    /// Record that `prompt_id` has been nudged (bounded ring).
    pub fn remember_prompt(&mut self, prompt_id: &str) {
        if prompt_id.is_empty() {
            return;
        }
        self.nudged_prompt_ids.push(prompt_id.to_string());
        let len = self.nudged_prompt_ids.len();
        if len > MAX_PROMPT_IDS {
            self.nudged_prompt_ids.drain(0..len - MAX_PROMPT_IDS);
        }
    }

    pub fn already_nudged(&self, prompt_id: &str) -> bool {
        !prompt_id.is_empty() && self.nudged_prompt_ids.iter().any(|p| p == prompt_id)
    }
}

/// A filesystem-backed store for per-session nudge state.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The real per-user location: `<temp>/semctl-nudge`.
    pub fn default_store() -> Self {
        Store {
            dir: std::env::temp_dir().join("semctl-nudge"),
        }
    }

    #[cfg(test)]
    pub fn with_dir(dir: PathBuf) -> Self {
        Store { dir }
    }

    pub fn load(&self, session_id: &str) -> NudgeState {
        let path = self.state_path(session_id);
        // Never read an oversized/corrupt file into memory (never-break).
        if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_STATE_BYTES) {
            return NudgeState::default();
        }
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Best-effort atomic write (unique temp + rename).
    pub fn save(&self, session_id: &str, state: &NudgeState) {
        let _ = fs::create_dir_all(&self.dir);
        let path = self.state_path(session_id);
        let tmp = self.dir.join(format!(
            "{}.{}.tmp",
            file_key(session_id),
            std::process::id()
        ));
        if let Ok(json) = serde_json::to_string(state)
            && fs::write(&tmp, json).is_ok()
        {
            let _ = fs::rename(&tmp, &path);
        }
    }

    /// Start a fresh nudge segment: zero search/nudge state while preserving the
    /// session-wide update cache and one-shot notice flag. Called on
    /// `SessionStart` `clear`/`compact`; those shrink the context but do not start
    /// a new agent session, so an update reminder must not repeat afterward.
    ///
    /// Deliberately does NOT remove the lock file: a concurrent `PreToolUse` may
    /// hold it, and deleting a live lock would let a second `PreToolUse` into the
    /// critical section. A lock left by a crash is reclaimed by stale-steal.
    pub fn reset(&self, session_id: &str) {
        if session_id.is_empty() {
            return;
        }
        let previous = self.load(session_id);
        self.save(
            session_id,
            &NudgeState {
                update_checked_at: previous.update_checked_at,
                update_latest_version: previous.update_latest_version,
                update_notice_emitted: previous.update_notice_emitted,
                ..NudgeState::default()
            },
        );
    }

    /// Try to take the per-session lock. `None` if another process holds a
    /// fresh lock — the caller then skips. A stale lock is stolen.
    pub fn try_lock(&self, session_id: &str) -> Option<LockGuard> {
        self.try_lock_with(session_id, STALE_LOCK_SECS)
    }

    /// `try_lock` with an explicit stale window, so the steal path can be driven
    /// deterministically in tests (a `0` window makes any existing lock stale).
    fn try_lock_with(&self, session_id: &str, stale_secs: u64) -> Option<LockGuard> {
        let _ = fs::create_dir_all(&self.dir);
        let path = self.lock_path(session_id);
        match new_lock(&path) {
            Ok(()) => Some(LockGuard { path }),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if lock_is_stale(&path, stale_secs) {
                    let _ = fs::remove_file(&path);
                    new_lock(&path).ok().map(|()| LockGuard { path })
                } else {
                    None
                }
            }
            Err(_) => None, // permissions etc. — skip, never block
        }
    }

    /// Prune idle sessions' state files. Best-effort housekeeping.
    pub fn cleanup(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() > CLEANUP_AGE_SECS);
            if old {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    fn state_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", file_key(session_id)))
    }

    fn lock_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{}.lock", file_key(session_id)))
    }
}

/// RAII lock: removes the marker file on drop (normal path or early return).
pub struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn new_lock(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
}

fn lock_is_stale(path: &Path, stale_secs: u64) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map_or(true, |t| {
            t.elapsed().map_or(true, |d| d.as_secs() >= stale_secs)
        })
}

/// The on-disk filename stem for a session: a sanitized, length-capped prefix
/// (readable for debugging) plus a hash of the RAW id. The hash guarantees
/// distinct session ids get distinct files even when sanitization would collide
/// (`abc.def` and `abc_def` both sanitize to `abc_def`), so one session can
/// never cap/dedup/block another.
fn file_key(session_id: &str) -> String {
    use std::hash::{Hash, Hasher};
    // DefaultHasher::new() uses fixed keys → deterministic across processes.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    session_id.hash(&mut h);
    let mut prefix = sanitize(session_id);
    prefix.truncate(48); // sanitize() is ASCII, so this is a valid boundary
    format!("{prefix}-{:016x}", h.finish())
}

/// Session ids are host-generated (uuid/hex) but sanitize defensively so a
/// weird value can never escape the state dir. Used for the readable prefix of
/// `file_key`; the hash there provides collision-resistance.
fn sanitize(session_id: &str) -> String {
    let s: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "none".to_string() } else { s }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TempStore {
        store: Store,
        dir: PathBuf,
    }
    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    fn temp_store() -> TempStore {
        let dir = std::env::temp_dir().join(format!(
            "semctl-nudge-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        TempStore {
            store: Store::with_dir(dir.clone()),
            dir,
        }
    }

    #[test]
    fn load_missing_is_default() {
        let t = temp_store();
        assert_eq!(t.store.load("s1"), NudgeState::default());
    }

    #[test]
    fn save_load_roundtrip() {
        let t = temp_store();
        let st = NudgeState {
            eligible_count: 5,
            nudges_fired: 2,
            last_nudge_at_count: 5,
            nudged_prompt_ids: vec!["p1".into()],
            ..Default::default()
        };
        t.store.save("s1", &st);
        assert_eq!(t.store.load("s1"), st);
    }

    #[test]
    fn save_twice_second_wins_and_leaks_no_tmp() {
        // Guards the atomic-replace assumption: std::fs::rename replaces an
        // existing destination on Windows too (MOVEFILE_REPLACE_EXISTING), so a
        // second save wins and the temp file is consumed by the rename.
        let t = temp_store();
        t.store.save(
            "s1",
            &NudgeState {
                eligible_count: 1,
                ..Default::default()
            },
        );
        t.store.save(
            "s1",
            &NudgeState {
                eligible_count: 2,
                ..Default::default()
            },
        );
        assert_eq!(
            t.store.load("s1").eligible_count,
            2,
            "second save must replace the first"
        );
        let leftover_tmp = fs::read_dir(&t.dir)
            .unwrap()
            .flatten()
            .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("tmp"));
        assert!(!leftover_tmp, "rename must consume the temp file");
    }

    #[test]
    fn reset_zeroes_nudges_but_preserves_session_update_state() {
        let t = temp_store();
        t.store.save(
            "s1",
            &NudgeState {
                eligible_count: 9,
                nudges_fired: 3,
                update_checked_at: 123,
                update_latest_version: "0.2.0".into(),
                update_notice_emitted: true,
                ..Default::default()
            },
        );
        t.store.reset("s1");
        let st = t.store.load("s1");
        assert_eq!(st.eligible_count, 0);
        assert_eq!(st.nudges_fired, 0);
        assert!(st.nudged_prompt_ids.is_empty());
        assert_eq!(st.update_checked_at, 123);
        assert_eq!(st.update_latest_version, "0.2.0");
        assert!(st.update_notice_emitted);
    }

    #[test]
    fn lock_is_exclusive_then_released() {
        let t = temp_store();
        let g = t.store.try_lock("s1").expect("first lock taken");
        assert!(t.store.try_lock("s1").is_none(), "second lock blocked");
        drop(g);
        assert!(t.store.try_lock("s1").is_some(), "lock free after drop");
    }

    #[test]
    fn lock_staleness_by_window() {
        let t = temp_store();
        let _ = fs::create_dir_all(&t.dir);
        let missing = t.dir.join("nope.lock");
        assert!(lock_is_stale(&missing, 10), "a missing lock reads as stale");
        let fresh = t.dir.join("s1.lock");
        fs::write(&fresh, "").unwrap();
        assert!(
            !lock_is_stale(&fresh, 10),
            "a just-created lock is not stale"
        );
        assert!(lock_is_stale(&fresh, 0), "zero window makes any lock stale");
    }

    #[test]
    fn fresh_lock_is_not_stolen_but_stale_one_is() {
        let t = temp_store();
        let _held = t.store.try_lock("s1").expect("first holder takes the lock");
        // A genuinely fresh lock is never stolen (huge window) → contention.
        assert!(
            t.store.try_lock_with("s1", u64::MAX).is_none(),
            "fresh lock held"
        );
        // With a zero staleness window the existing lock reads as stale → stolen.
        assert!(
            t.store.try_lock_with("s1", 0).is_some(),
            "stale lock stolen"
        );
    }

    #[test]
    fn prompt_dedup_ring() {
        let mut st = NudgeState::default();
        st.remember_prompt("p1");
        assert!(st.already_nudged("p1"));
        assert!(!st.already_nudged("p2"));
        assert!(!st.already_nudged("")); // empty never counts
        for i in 0..MAX_PROMPT_IDS + 10 {
            st.remember_prompt(&format!("q{i}"));
        }
        assert!(st.nudged_prompt_ids.len() <= MAX_PROMPT_IDS);
        assert!(!st.already_nudged("p1"), "oldest evicted");
    }

    #[test]
    fn sanitize_neutralizes_traversal_and_keeps_ids_in_dir() {
        // Traversal chars are all mapped away — a hostile id can't escape the dir.
        let s = sanitize("../../etc/passwd");
        assert!(
            !s.contains('/') && !s.contains('\\') && !s.contains('.'),
            "sanitized: {s}"
        );
        assert_eq!(sanitize(""), "none"); // empty → stable placeholder
        assert_eq!(sanitize("abc-123_DEF"), "abc-123_DEF"); // a normal id round-trips
        // The concrete path for a traversal-laden id stays directly under the store.
        let t = temp_store();
        assert_eq!(
            t.store.state_path("../../evil").parent().unwrap(),
            t.dir,
            "state file must stay inside the store dir"
        );
    }

    #[test]
    fn file_key_avoids_collisions_when_sanitize_would_alias() {
        // Both sanitize to `abc_def`, but the raw-id hash keeps their files apart.
        assert_ne!(file_key("abc.def"), file_key("abc_def"));
        assert_ne!(sanitize("abc.def"), file_key("abc.def")); // key carries the hash
    }

    #[test]
    fn oversized_state_file_degrades_to_default() {
        let t = temp_store();
        let _ = fs::create_dir_all(&t.dir);
        let path = t.store.state_path("s1");
        fs::write(
            &path,
            vec![b'x'; usize::try_from(MAX_STATE_BYTES + 1).unwrap()],
        )
        .unwrap();
        assert_eq!(
            t.store.load("s1"),
            NudgeState::default(),
            "oversized file must be ignored"
        );
    }

    #[test]
    fn cleanup_is_noop_on_missing_dir_and_keeps_fresh_state() {
        let t = temp_store();
        t.store.cleanup(); // dir doesn't exist yet → must not panic
        t.store.save(
            "s1",
            &NudgeState {
                eligible_count: 3,
                ..Default::default()
            },
        );
        t.store.cleanup(); // fresh file is well within the 24h window
        assert_eq!(
            t.store.load("s1").eligible_count,
            3,
            "fresh state survives cleanup"
        );
    }
}
