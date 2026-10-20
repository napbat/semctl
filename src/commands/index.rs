//! `semctl index [path]` — register the folder as a Local codebase (if it
//! isn't one already) and sync its files to the server, then wait for the
//! embed job to finish.
//!
//! This is the thin one-shot wrapper: it delegates the walk / manifest-diff /
//! upload to the shared [`crate::sync`] engine (with a persistent stamp/hash
//! cache), then polls the queued job to completion. The `semctl mcp` auto-index
//! drives the same engine in the background.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::cli::Cli;
use crate::client::{self, Client, api};
use crate::engine::Scheduler;
use crate::sync::{SyncLimits, SyncProgress};

#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Directory to index. Defaults to the current directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Return as soon as the upload is queued instead of waiting for the
    /// server to finish embedding.
    #[arg(long)]
    pub no_wait: bool,
}

/// How long to wait for the embed job before giving up the poll (the job keeps
/// running server-side regardless).
const JOB_WAIT: std::time::Duration = std::time::Duration::from_mins(10);
// Fast jobs finish during this window. Poll them closely so status latency does
// not mask indexing latency. Long jobs back off to bound steady database load.
const RESPONSIVE_POLL_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);
const RESPONSIVE_POLL_EVERY: std::time::Duration = std::time::Duration::from_millis(100);
const STEADY_POLL_EVERY: std::time::Duration = std::time::Duration::from_millis(500);
/// Emit a heartbeat often enough that a large repository never looks hung,
/// without turning the status poll into terminal spam.
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// A completed embed job is successful only when the worker reported neither a
/// job-level error nor failed files. Keeping this separate from the upload
/// result is deliberate: the catalog sync has already succeeded at this point.
fn terminal_result(job_id: &str, job: &api::JobStatus) -> Result<bool> {
    if job.completed_at.is_none() {
        return Ok(false);
    }
    if let Some(error) = job.error.as_deref() {
        bail!("embedding failed for job {job_id}: {error}");
    }
    if job.files_failed > 0 {
        bail!(
            "embedding failed for job {job_id}: {} file(s) failed",
            job.files_failed
        );
    }
    Ok(true)
}

pub async fn run(args: IndexArgs, cli: &Cli) -> Result<()> {
    let client = client::from_cli(cli)?;
    let dir = std::fs::canonicalize(&args.path)
        .with_context(|| format!("resolve path {}", args.path.display()))?;

    // Persist hashes and filter decisions. Every scan verifies current bytes
    // before reusing a decision. Source contents are not cached on disk.
    let cache = Mutex::new(crate::sync::SyncCache::persistent());
    // One command is one process, so its scheduler is its own. It exists to
    // bound this sync's uploads with the same rule the shared engine applies.
    let limits = SyncLimits::new(Scheduler::from_environment().upload_permits());
    let outcome =
        crate::sync::sync_with_progress(&client, &dir, &cache, &limits, report_sync_progress)
            .await?;
    let upload_failures = outcome.upload_failures();
    if outcome.uploaded == 0 && outcome.to_delete == 0 {
        info!(codebase = %outcome.codebase_id, "up to date — nothing to upload");
    } else {
        info!(
            codebase = %outcome.codebase_id,
            uploaded = outcome.uploaded,
            to_delete = outcome.to_delete,
            "queued sync",
        );
    }
    if upload_failures.is_some() {
        warn!(
            failed = outcome.failed.len(),
            "some requested files were not uploaded; the server indexes the other files"
        );
    }

    let waited = if args.no_wait {
        info!(job = %outcome.job_id, "queued; not waiting for embed");
        Ok(())
    } else {
        wait_for_job(&client, &outcome.job_id).await
    };
    let Some(failures) = upload_failures else {
        return waited;
    };
    // The job closes without the missing files, so its own error restates the
    // shortfall. The upload failure is the cause the user can act on.
    let retry = "run `semctl index` again to upload them";
    Err(match waited {
        Ok(()) => anyhow!("{failures}; {retry}"),
        Err(job) => job.context(format!("{failures}; {retry}")),
    })
}

fn report_sync_progress(progress: &SyncProgress) {
    info!("{progress}");
}

fn poll_interval(elapsed: std::time::Duration) -> std::time::Duration {
    if elapsed < RESPONSIVE_POLL_WINDOW {
        RESPONSIVE_POLL_EVERY
    } else {
        STEADY_POLL_EVERY
    }
}

/// Poll the index job until the worker finishes embedding, reporting the
/// outcome. The job runs server-side regardless of whether we keep waiting, so
/// a timeout here isn't a failure — just stop watching.
async fn wait_for_job(client: &Client, job_id: &str) -> Result<()> {
    let start = std::time::Instant::now();
    let mut next_progress = start;
    loop {
        let job: api::JobStatus = client
            .get(&format!("/v1/jobs/{job_id}"))
            .await
            .with_context(|| format!("poll job {job_id}"))?;
        if terminal_result(job_id, &job)? {
            info!(
                embedded = job.files_embedded,
                failed = job.files_failed,
                chunks = ?job.chunk_count,
                "indexed",
            );
            return Ok(());
        }
        if start.elapsed() >= JOB_WAIT {
            info!(
                %job_id,
                secs = JOB_WAIT.as_secs(),
                "still embedding; continues server-side — check back with search",
            );
            return Ok(());
        }

        let now = std::time::Instant::now();
        if now >= next_progress {
            // Worker counters are independent reports, not a completion ratio.
            let phase = if job.started_at.is_some() {
                "embedding"
            } else {
                "queued"
            };
            info!(
                %job_id,
                elapsed_secs = start.elapsed().as_secs(),
                embedded = job.files_embedded,
                deleted = ?job.files_deleted,
                failed = job.files_failed,
                planned_embed = ?job.files_to_embed,
                planned_delete = ?job.files_to_delete,
                "{phase}: reported file counts (None means unknown)",
            );
            next_progress = now + PROGRESS_EVERY;
        }
        debug!(%job_id, "embedding…");
        tokio::time::sleep(poll_interval(start.elapsed())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{api, poll_interval, terminal_result};

    #[test]
    fn polling_is_responsive_for_fast_jobs_then_backs_off() {
        assert_eq!(
            poll_interval(std::time::Duration::ZERO),
            std::time::Duration::from_millis(100)
        );
        assert_eq!(
            poll_interval(std::time::Duration::from_millis(4_999)),
            std::time::Duration::from_millis(100)
        );
        assert_eq!(
            poll_interval(std::time::Duration::from_secs(5)),
            std::time::Duration::from_millis(500)
        );
    }

    fn job(completed: bool, failed: i64, error: Option<&str>) -> api::JobStatus {
        api::JobStatus {
            files_to_embed: Some(12),
            files_to_delete: Some(3),
            files_embedded: 7,
            files_deleted: Some(2),
            files_failed: failed,
            chunk_count: Some(21),
            error: error.map(str::to_owned),
            started_at: Some("2026-07-14T00:00:00Z".into()),
            completed_at: completed.then(|| "2026-07-14T00:01:00Z".into()),
        }
    }

    #[test]
    fn incomplete_job_is_not_terminal() {
        assert!(!terminal_result("job-1", &job(false, 0, None)).unwrap());
    }

    #[test]
    fn completed_worker_error_fails_the_command_as_embedding() {
        let error = terminal_result("job-2", &job(true, 12, Some("store unavailable")))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "embedding failed for job job-2: store unavailable");
    }

    #[test]
    fn failed_files_without_a_job_error_still_fail_the_command() {
        let error = terminal_result("job-3", &job(true, 4, None))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "embedding failed for job job-3: 4 file(s) failed");
    }

    #[test]
    fn completion_does_not_require_matching_or_present_plan_counts() {
        let mut completed = job(true, 0, None);
        assert!(terminal_result("job-4", &completed).unwrap());
        completed.files_to_embed = None;
        completed.files_to_delete = None;
        completed.files_deleted = None;
        assert!(terminal_result("job-4", &completed).unwrap());
        completed.completed_at = None;
        assert!(!terminal_result("job-4", &completed).unwrap());
    }
}
