//! Background auto-index orchestration for `semctl mcp`.
//!
//! Keeps the launch directory indexed for the server's lifetime by driving the
//! [`super::sync`] engine off the serving path: a startup walk + sync, a realtime
//! FS [`super::watcher`] for low-latency pickup, and a periodic re-sync as the
//! drift backstop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, oneshot};
use tracing::{debug, info, warn};

use super::{JobRegistry, SyncCache, SyncOutcome, record_job, sync, watcher};
use crate::client::Client;

/// Keep the launch directory indexed for the server's lifetime: a startup walk
/// and sync, a realtime FS watcher for low-latency pickup, and a periodic
/// re-sync as the drift backstop — entirely off the serving path so none of it
/// can stall the JSON-RPC channel. Detached; best-effort.
pub fn spawn_indexing(client: Client, dir: PathBuf, jobs: Arc<JobRegistry>) {
    spawn(client, dir, jobs, None);
}

/// Start the normal indexing/watcher lifecycle and return a one-shot result for
/// its startup sync. The MCP first-index readiness gate awaits this handle, then
/// polls the queued server job through embedding completion.
pub fn spawn_indexing_tracked(
    client: Client,
    dir: PathBuf,
    jobs: Arc<JobRegistry>,
) -> oneshot::Receiver<Result<SyncOutcome, String>> {
    let (tx, rx) = oneshot::channel();
    spawn(client, dir, jobs, Some(tx));
    rx
}

fn spawn(
    client: Client,
    dir: PathBuf,
    jobs: Arc<JobRegistry>,
    initial_result: Option<oneshot::Sender<Result<SyncOutcome, String>>>,
) {
    tokio::spawn(async move {
        // Watch the whole Git working copy even when the MCP host launched from
        // a nested directory. The sync manifest is complete desired state, not
        // a subtree patch.
        let dir = crate::codebase::working_copy_root(&dir).await;
        // One stamp cache shared across startup / periodic / watch syncs; the
        // Mutex inside also serializes them so only one runs at a time.
        let cache = Arc::new(Mutex::new(SyncCache::default()));

        // Startup sync intentionally races watcher registration so edits made
        // during a long initial walk are still observed. Git metadata capture
        // is safe alongside MCP/watch I/O on Windows (see `codebase::git`).
        spawn_startup_reconcile(
            client.clone(),
            dir.clone(),
            cache.clone(),
            jobs.clone(),
            initial_result,
        );
        match resync_secs() {
            0 => info!("periodic re-sync disabled (SEMCTX_MCP_RESYNC_SECS=0)"),
            secs => {
                info!(secs, "periodic re-sync enabled");
                spawn_periodic_resync(
                    client.clone(),
                    dir.clone(),
                    secs,
                    cache.clone(),
                    jobs.clone(),
                );
            }
        }

        // The watcher's recursive registration walks the tree to seed its file-id
        // cache — blocking and potentially long on a big directory — so run it on
        // the blocking pool rather than a runtime worker. The returned debouncer
        // guard is held here for the task's (and thus the server's) lifetime;
        // dropping it would stop the watch.
        let watcher = tokio::task::spawn_blocking(move || watcher::spawn(client, dir, cache, jobs))
            .await
            .ok()
            .flatten();
        if watcher.is_some() {
            std::future::pending::<()>().await;
        }
    });
}

/// Schedule the startup trigger for the shared [`sync`] reconcile engine.
/// Detached so it runs alongside `serve`; the optional result channel exists
/// only for first-index readiness, not as a separate indexing implementation.
fn spawn_startup_reconcile(
    client: Client,
    dir: PathBuf,
    cache: Arc<Mutex<SyncCache>>,
    jobs: Arc<JobRegistry>,
    initial_result: Option<oneshot::Sender<Result<SyncOutcome, String>>>,
) {
    tokio::spawn(async move {
        match sync(&client, &dir, &cache).await {
            Ok(o) => {
                info!(
                    codebase = %o.codebase_id,
                    job = %o.job_id,
                    uploaded = o.uploaded,
                    to_delete = o.to_delete,
                    "auto-index queued",
                );
                // Always record the first job — it's the one `sync_status`
                // reports until a later re-sync pushes changes.
                record_job(&jobs, &o).await;
                if let Some(tx) = initial_result {
                    let _ = tx.send(Ok(o));
                }
            }
            Err(e) => {
                let reason = format!("{e:#}");
                warn!(error = %reason, "auto-index failed; serving existing index");
                if let Some(tx) = initial_result {
                    let _ = tx.send(Err(reason));
                }
            }
        }
    });
}

/// Default interval for the MCP server's periodic re-sync. Override with
/// `SEMCTX_MCP_RESYNC_SECS`; `0` disables it (the startup index still runs).
const DEFAULT_RESYNC_SECS: u64 = 60;

fn resync_secs() -> u64 {
    std::env::var("SEMCTX_MCP_RESYNC_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_RESYNC_SECS)
}

/// Spawn the periodic re-sync loop: every `secs` seconds, re-walk `dir` and push
/// whatever changed. The drift backstop behind the realtime watcher — it catches
/// edits the FS events drop (offline changes, dropped notifications, server-side
/// wipes). The first tick is offset by `secs` so it doesn't race the startup
/// index. Detached; best-effort, logs failures to stderr.
fn spawn_periodic_resync(
    client: Client,
    dir: PathBuf,
    secs: u64,
    cache: Arc<Mutex<SyncCache>>,
    jobs: Arc<JobRegistry>,
) {
    tokio::spawn(async move {
        let period = Duration::from_secs(secs);
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            // `sync` logs its step-by-step progress at debug; here we surface an
            // info line only when the tick actually changed something, so an
            // idle server doesn't chatter every interval.
            match sync(&client, &dir, &cache).await {
                Ok(o) if o.uploaded > 0 || o.to_delete > 0 => {
                    info!(
                        uploaded = o.uploaded,
                        to_delete = o.to_delete,
                        job = %o.job_id,
                        "re-sync pushed changes",
                    );
                    // Only a change-pushing tick advances `sync_status`; a no-op
                    // tick leaves it pointing at the last meaningful job.
                    record_job(&jobs, &o).await;
                }
                Ok(_) => debug!("re-sync: no changes"),
                Err(e) => warn!(error = %format!("{e:#}"), "periodic re-sync failed"),
            }
        }
    });
}
