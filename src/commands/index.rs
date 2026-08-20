//! `semctl index [path]` — register the folder as a Local codebase (if it
//! isn't one already) and sync its files to the server, then wait for the
//! embed job to finish.
//!
//! This is the thin one-shot wrapper: it delegates the walk / manifest-diff /
//! upload to the shared [`crate::sync`] engine (with a persistent stamp/hash
//! cache), then polls the queued job to completion. The `semctl mcp` auto-index
//! drives the same engine in the background.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::cli::Cli;
use crate::client::{self, Client, api};
use crate::sync::SyncProgress;

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
// Tight enough that a fast single-file sync isn't masked by poll granularity —
// the job usually finishes in a few seconds.
const POLL_EVERY: std::time::Duration = std::time::Duration::from_millis(500);
/// Emit a heartbeat often enough that a large repository never looks hung,
/// without turning the 500 ms status poll into terminal spam.
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
struct JobProgress {
    processed: i64,
    total: i64,
    embedded: i64,
    deleted: i64,
    failed: i64,
}

impl From<&api::JobStatus> for JobProgress {
    fn from(job: &api::JobStatus) -> Self {
        Self {
            processed: job.files_embedded + job.files_deleted + job.files_failed,
            total: job.files_to_embed + job.files_to_delete,
            embedded: job.files_embedded,
            deleted: job.files_deleted,
            failed: job.files_failed,
        }
    }
}

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

    // Persist stamps/hashes before upload so a restarted command only reads
    // files that changed while it was away. Source contents are not cached.
    let cache = Mutex::new(crate::sync::SyncCache::persistent());
    let outcome =
        crate::sync::sync_with_progress(&client, &dir, &cache, report_sync_progress).await?;
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

    if args.no_wait {
        info!(job = %outcome.job_id, "queued; not waiting for embed");
        return Ok(());
    }
    wait_for_job(&client, &outcome.job_id).await
}

fn report_sync_progress(progress: &SyncProgress) {
    info!("{}", sync_progress_message(progress));
}

fn sync_progress_message(progress: &SyncProgress) -> String {
    match progress {
        SyncProgress::Preparing => "preparing index".to_string(),
        SyncProgress::Scanning { root } => format!("scanning files in {}", root.display()),
        SyncProgress::Planning {
            files,
            cached_files,
        } => format!("scanned {files} files ({cached_files} hashes reused) — checking for changes"),
        SyncProgress::Uploading {
            uploaded_files,
            total_files,
        } => format!("uploading: {uploaded_files}/{total_files} files"),
        SyncProgress::Finalizing => "finalizing upload".to_string(),
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
                chunks = job.chunk_count.unwrap_or(0),
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
            let progress = JobProgress::from(&job);
            let phase = if job.started_at.is_some() {
                "embedding"
            } else {
                "queued"
            };
            info!(
                %job_id,
                elapsed_secs = start.elapsed().as_secs(),
                embedded = progress.embedded,
                deleted = progress.deleted,
                failed = progress.failed,
                "{phase}: {}/{} processed",
                progress.processed,
                progress.total,
            );
            next_progress = now + PROGRESS_EVERY;
        }
        debug!(%job_id, "embedding…");
        tokio::time::sleep(POLL_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{JobProgress, SyncProgress, api, sync_progress_message, terminal_result};

    fn job(completed: bool, failed: i64, error: Option<&str>) -> api::JobStatus {
        api::JobStatus {
            files_to_embed: 12,
            files_to_delete: 3,
            files_embedded: 7,
            files_deleted: 2,
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
    fn progress_counts_failed_files_as_processed() {
        let progress = JobProgress::from(&job(false, 3, None));
        assert_eq!(progress.processed, 12);
        assert_eq!(progress.total, 15);
    }

    #[test]
    fn upload_progress_reports_completed_and_total_files() {
        let message = sync_progress_message(&SyncProgress::Uploading {
            uploaded_files: 3,
            total_files: 8,
        });
        assert_eq!(message, "uploading: 3/8 files");
    }

    #[test]
    fn scan_summary_reports_reused_hashes() {
        let message = sync_progress_message(&SyncProgress::Planning {
            files: 80,
            cached_files: 73,
        });
        assert_eq!(
            message,
            "scanned 80 files (73 hashes reused) — checking for changes"
        );
    }

    #[test]
    fn scan_progress_names_the_effective_root() {
        let message = sync_progress_message(&SyncProgress::Scanning {
            root: PathBuf::from("repo"),
        });
        assert_eq!(message, "scanning files in repo");
    }
}
