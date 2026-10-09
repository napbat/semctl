//! Authorized, bounded uploads for one submitted manifest.
//!
//! One failed batch does not stop the upload. A batch that can succeed later
//! is sent once more, alone, after the other batches. The sync is then
//! completed for every batch the server received. The server offers the
//! missing files to the next sync again. Only a refusal of the sync itself,
//! or a local failure to read the checkout, stops the upload.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use super::blocking::Cancellation;
use super::scan::PreparedFile;
use super::{SyncLimits, SyncProgress, blocking, source, walker};
use crate::client::{Client, ResponseError, api};

const UPLOAD_BATCH_BYTES: usize = 4 * 1024 * 1024;
const UPLOAD_BATCH_FILES: usize = 256;
/// One checkout's own limit. The process-wide limit is [`SyncLimits`].
const UPLOAD_PARALLEL_REQUESTS: usize = 4;
/// The wait before a failed request is sent again, when the response did not
/// ask for a delay of its own.
const RETRY_DELAY: Duration = Duration::from_secs(2);
/// One response cannot hold an upload back longer than this.
const RETRY_DELAY_MAX: Duration = Duration::from_secs(30);
/// A failure report names at most this many paths for one reason.
const REPORTED_PATHS: usize = 5;
/// A failure report names at most this many different reasons.
const REPORTED_REASONS: usize = 3;

/// One requested file that the server did not receive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UploadFailure {
    pub(crate) path: String,
    pub(crate) reason: String,
}

/// What one upload delivered.
#[derive(Debug, Default)]
pub(super) struct Delivery {
    pub(super) uploaded: usize,
    pub(super) failed: Vec<UploadFailure>,
}

/// What an upload does after one request fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disposition {
    /// The request can succeed later: the connection failed, a gateway timed
    /// out or closed the request, or the server failed.
    Retry,
    /// The server refused this batch. The other batches can still succeed.
    Skip,
    /// The sync cannot continue. The server refused the credentials, the
    /// codebase, or the sync job, so every other request gets the same answer.
    Abort,
}

/// Only entries from the submitted manifest can become pending uploads.
pub(super) struct Files {
    root: PathBuf,
    pending: VecDeque<api::ManifestEntry>,
    changed: HashMap<String, PreparedFile>,
    policy: super::policy::SourcePolicy,
}

/// The content of one upload request and the manifest entries it came from.
/// A failed batch keeps only its entries, so it does not hold its content.
#[derive(Debug, Default)]
struct Batch {
    entries: Vec<api::ManifestEntry>,
    files: Vec<api::SyncFileContent>,
}

impl Files {
    pub(super) fn new(
        root: PathBuf,
        needed: &[String],
        manifest: Vec<api::ManifestEntry>,
        changed: HashMap<String, PreparedFile>,
        policy: super::policy::SourcePolicy,
    ) -> Result<Self> {
        let mut allowed: HashMap<_, _> = manifest
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let mut pending = VecDeque::with_capacity(needed.len());
        for path in needed {
            let entry = allowed.remove(path).with_context(|| {
                format!("sync requested an unknown or duplicate manifest path: {path}")
            })?;
            pending.push_back(entry);
        }
        Ok(Self {
            root,
            pending,
            changed,
            policy,
        })
    }

    async fn next(mut self) -> Result<(Self, Batch)> {
        blocking::run(move |cancellation| {
            let batch = self.prepare_next(&cancellation)?;
            Ok((self, batch))
        })
        .await
        .context("prepare upload batch")
    }

    fn prepare_next(&mut self, cancellation: &Cancellation) -> Result<Batch> {
        self.policy.verify(cancellation)?;
        let mut batch = Batch::default();
        let mut bytes = 0;
        while let Some(entry) = self.pending.front() {
            cancellation.check()?;
            let size = usize::try_from(entry.size).context("invalid manifest file size")?;
            if !batch.files.is_empty()
                && (batch.files.len() >= UPLOAD_BATCH_FILES || bytes + size > UPLOAD_BATCH_BYTES)
            {
                break;
            }
            let path = &entry.path;
            // Check the live path even when the authorized content was retained.
            // A file or ancestor can become a symlink after the manifest scan.
            source::checked_path(&self.root, path)?;
            let prepared = if let Some(prepared) = self.changed.remove(path) {
                prepared
            } else {
                let content = source::read(&self.root, path, cancellation)?
                    .with_context(|| format!("{path} became non-UTF-8 after the manifest scan"))?;
                PreparedFile::new(content)
            };
            ensure!(
                prepared.hash == entry.hash,
                "{path} changed after the manifest scan; retry sync"
            );
            ensure!(
                walker::has_uploadable_content(&prepared.content),
                "{path} became blank after the manifest scan"
            );
            bytes += prepared.content.len();
            batch.files.push(api::SyncFileContent {
                path: path.clone(),
                content: prepared.content,
                hash: Some(prepared.hash),
            });
            batch.entries.extend(self.pending.pop_front());
        }
        cancellation.check()?;
        self.policy.verify(cancellation)?;
        Ok(batch)
    }
}

/// Prepare one batch at a time. The retained scan content, pending batch, and
/// in-flight requests each have a bound independent of total checkout size.
pub(super) async fn run(
    client: &Client,
    codebase_id: &str,
    job_id: &str,
    files: Files,
    source_id: &str,
    limits: &SyncLimits,
    on_progress: &impl Fn(&SyncProgress),
) -> Result<Delivery> {
    let total = files.pending.len();
    if total == 0 {
        return Ok(Delivery::default());
    }
    report_upload_progress(on_progress, 0, total);
    let mut upload = Upload {
        client,
        url: format!("/v1/codebases/{codebase_id}/sync/{job_id}"),
        source_id,
        limits,
        total,
        uploaded: 0,
        failed: Vec::new(),
        deferred: Vec::new(),
        requested_delay: None,
    };
    let (files, batch) = files.next().await?;
    // Preserve the single-request wire format used by older servers.
    if files.pending.is_empty() {
        return upload.single(batch, on_progress).await;
    }
    let files = upload.in_parallel(files, batch, on_progress).await?;
    upload.retry_deferred(files, on_progress).await?;
    upload.complete(on_progress).await
}

/// One upload's request target and its running account of each batch.
struct Upload<'a> {
    client: &'a Client,
    url: String,
    source_id: &'a str,
    limits: &'a SyncLimits,
    total: usize,
    uploaded: usize,
    failed: Vec<UploadFailure>,
    /// The entries of batches that failed with a retryable error.
    deferred: Vec<api::ManifestEntry>,
    /// The longest delay that a failed response asked for.
    requested_delay: Option<Duration>,
}

/// One finished batch request and the manifest entries it carried.
struct Sent {
    entries: Vec<api::ManifestEntry>,
    result: Result<()>,
}

impl Upload<'_> {
    fn request(
        &self,
        files: Vec<api::SyncFileContent>,
        final_batch: Option<bool>,
    ) -> api::SyncContentRequest {
        api::SyncContentRequest {
            files,
            source_id: self.source_id.to_string(),
            r#final: final_batch,
        }
    }

    /// Send the only batch as one request that also completes the sync.
    async fn single(
        mut self,
        batch: Batch,
        on_progress: &impl Fn(&SyncProgress),
    ) -> Result<Delivery> {
        let count = batch.files.len();
        let request = self.request(batch.files, None);
        self.put_with_retry(&request)
            .await
            .with_context(|| format!("upload {count} files"))?;
        self.uploaded = count;
        report_upload_progress(on_progress, count, self.total);
        Ok(self.into_delivery())
    }

    /// Send every batch, at most [`UPLOAD_PARALLEL_REQUESTS`] at a time. A
    /// failed batch does not stop the batches after it.
    async fn in_parallel(
        &mut self,
        mut files: Files,
        mut batch: Batch,
        on_progress: &impl Fn(&SyncProgress),
    ) -> Result<Files> {
        // Dropping the set aborts its requests, so an error that stops the
        // upload also stops every batch still in flight.
        let mut set = JoinSet::new();
        loop {
            if set.len() >= UPLOAD_PARALLEL_REQUESTS {
                self.settle(join_one(&mut set).await?)?;
                report_upload_progress(on_progress, self.uploaded, self.total);
            }
            self.spawn(&mut set, batch);
            if files.pending.is_empty() {
                break;
            }
            (files, batch) = files.next().await?;
        }
        while !set.is_empty() {
            self.settle(join_one(&mut set).await?)?;
            report_upload_progress(on_progress, self.uploaded, self.total);
        }
        Ok(files)
    }

    fn spawn(&self, set: &mut JoinSet<Sent>, batch: Batch) {
        let client = self.client.clone();
        let url = self.url.clone();
        let limits = self.limits.clone();
        let request = self.request(batch.files, Some(false));
        let entries = batch.entries;
        set.spawn(async move {
            let result = put(&client, &url, &request, &limits).await;
            Sent { entries, result }
        });
    }

    /// Account for one finished batch. Only a refusal of the sync itself
    /// returns an error.
    fn settle(&mut self, sent: Sent) -> Result<()> {
        let Sent { entries, result } = sent;
        let count = entries.len();
        let Err(error) = result else {
            self.uploaded += count;
            debug!(
                uploaded = self.uploaded,
                total = self.total,
                "uploaded batch"
            );
            return Ok(());
        };
        match disposition(&error) {
            Disposition::Abort => Err(error.context(format!("upload {count} files"))),
            Disposition::Retry => {
                warn!(
                    files = count,
                    error = %format!("{error:#}"),
                    "upload batch failed; it is sent again after the other batches"
                );
                self.requested_delay = self.requested_delay.max(response_delay(&error));
                self.deferred.extend(entries);
                Ok(())
            }
            Disposition::Skip => {
                self.fail(entries, &error);
                Ok(())
            }
        }
    }

    /// Send each deferred batch once more, one request at a time. A request
    /// that is alone has the full connection, so a batch that timed out while
    /// it shared the connection can finish. The content is read again from the
    /// checkout and must still match its manifest hash.
    async fn retry_deferred(
        &mut self,
        mut files: Files,
        on_progress: &impl Fn(&SyncProgress),
    ) -> Result<()> {
        if self.deferred.is_empty() {
            return Ok(());
        }
        let delay = retry_delay(self.requested_delay);
        info!(
            files = self.deferred.len(),
            delay_secs = delay.as_secs(),
            "retrying failed upload batches one at a time"
        );
        tokio::time::sleep(delay).await;
        files.pending.extend(std::mem::take(&mut self.deferred));
        while !files.pending.is_empty() {
            let (rest, batch) = files.next().await?;
            files = rest;
            let count = batch.entries.len();
            let request = self.request(batch.files, Some(false));
            match put(self.client, &self.url, &request, self.limits).await {
                Ok(()) => self.uploaded += count,
                Err(error) if disposition(&error) == Disposition::Abort => {
                    return Err(error.context(format!("upload {count} files")));
                }
                Err(error) => self.fail(batch.entries, &error),
            }
            report_upload_progress(on_progress, self.uploaded, self.total);
        }
        Ok(())
    }

    /// Complete the sync for every batch the server received. The server
    /// closes the job without the missing files and offers them to the next
    /// sync again.
    async fn complete(self, on_progress: &impl Fn(&SyncProgress)) -> Result<Delivery> {
        if self.uploaded == 0
            && let Some(summary) = failure_summary(self.uploaded, &self.failed)
        {
            // Nothing arrived, so there is no partial sync to complete. The
            // server gives this job to the next sync of the checkout.
            bail!(summary);
        }
        on_progress(&SyncProgress::Finalizing);
        let request = self.request(Vec::new(), Some(true));
        self.put_with_retry(&request)
            .await
            .context("complete upload")?;
        Ok(self.into_delivery())
    }

    /// Send one request, and send it once more when the failure can be
    /// temporary.
    async fn put_with_retry(&self, request: &api::SyncContentRequest) -> Result<()> {
        match put(self.client, &self.url, request, self.limits).await {
            Err(error) if disposition(&error) == Disposition::Retry => {
                let delay = retry_delay(response_delay(&error));
                warn!(
                    error = %format!("{error:#}"),
                    delay_secs = delay.as_secs(),
                    "upload request failed; retrying"
                );
                tokio::time::sleep(delay).await;
                put(self.client, &self.url, request, self.limits).await
            }
            result => result,
        }
    }

    fn fail(&mut self, entries: Vec<api::ManifestEntry>, error: &anyhow::Error) {
        let reason = format!("{error:#}");
        warn!(
            files = entries.len(),
            error = %reason,
            "upload batch failed; the sync continues without it"
        );
        self.failed
            .extend(entries.into_iter().map(|entry| UploadFailure {
                path: entry.path,
                reason: reason.clone(),
            }));
    }

    fn into_delivery(self) -> Delivery {
        Delivery {
            uploaded: self.uploaded,
            failed: self.failed,
        }
    }
}

/// Send one sync request. The permit is held for the whole request, so the
/// process never has more upload requests in flight than the engine allows.
async fn put(
    client: &Client,
    url: &str,
    request: &api::SyncContentRequest,
    limits: &SyncLimits,
) -> Result<()> {
    let _permit = limits.upload_permit().await;
    client
        .put::<_, serde_json::Value>(url, request)
        .await
        .map(drop)
}

/// Classify a failed request. An error that is neither a response nor a
/// transport failure is local and stops the upload.
fn disposition(error: &anyhow::Error) -> Disposition {
    if let Some(response) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ResponseError>())
    {
        return match response.status().as_u16() {
            401 | 403 | 404 | 409 | 410 => Disposition::Abort,
            408 | 425 | 429 | 499 | 500..=599 => Disposition::Retry,
            _ => Disposition::Skip,
        };
    }
    if error
        .chain()
        .any(<dyn std::error::Error>::is::<reqwest::Error>)
    {
        return Disposition::Retry;
    }
    Disposition::Abort
}

fn response_delay(error: &anyhow::Error) -> Option<Duration> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ResponseError>())
        .and_then(ResponseError::retry_after)
}

fn retry_delay(requested: Option<Duration>) -> Duration {
    requested.unwrap_or(RETRY_DELAY).min(RETRY_DELAY_MAX)
}

/// Describe the requested files that the server did not receive. `None` means
/// that the server received every requested file.
pub(super) fn failure_summary(uploaded: usize, failed: &[UploadFailure]) -> Option<String> {
    if failed.is_empty() {
        return None;
    }
    let mut reasons: Vec<(&str, Vec<&str>)> = Vec::new();
    for failure in failed {
        match reasons
            .iter_mut()
            .find(|(reason, _)| *reason == failure.reason)
        {
            Some((_, paths)) => paths.push(&failure.path),
            None => reasons.push((&failure.reason, vec![&failure.path])),
        }
    }
    let mut details: Vec<String> = reasons
        .iter()
        .take(REPORTED_REASONS)
        .map(|(reason, paths)| {
            let named = paths
                .iter()
                .take(REPORTED_PATHS)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            match paths.len().saturating_sub(REPORTED_PATHS) {
                0 => format!("{named} ({reason})"),
                more => format!("{named} and {more} more ({reason})"),
            }
        })
        .collect();
    if reasons.len() > REPORTED_REASONS {
        details.push(format!(
            "{} more failure reason(s)",
            reasons.len() - REPORTED_REASONS
        ));
    }
    Some(format!(
        "{} of {} requested file(s) were not uploaded: {}",
        failed.len(),
        uploaded + failed.len(),
        details.join("; ")
    ))
}

fn report_upload_progress(
    on_progress: &impl Fn(&SyncProgress),
    uploaded_files: usize,
    total_files: usize,
) {
    on_progress(&SyncProgress::Uploading {
        uploaded_files,
        total_files,
    });
}

/// Await the next finished upload task. The caller awaits only a set that is
/// not empty.
async fn join_one(set: &mut JoinSet<Sent>) -> Result<Sent> {
    set.join_next()
        .await
        .context("no upload task is in flight")?
        .context("upload task")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn prepare(root: &Path, entries: &[(&str, &str)], retain: bool) -> Files {
        let mut manifest = Vec::new();
        let mut changed = HashMap::new();
        let mut requested = Vec::new();
        for (path, content) in entries {
            let absolute = root.join(path);
            fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            fs::write(&absolute, content).unwrap();
            let prepared = PreparedFile::new((*content).to_string());
            manifest.push(api::ManifestEntry {
                path: (*path).to_string(),
                hash: prepared.hash.clone(),
                size: i64::try_from(content.len()).unwrap(),
            });
            requested.push((*path).to_string());
            if retain {
                changed.insert((*path).to_string(), prepared);
            }
        }
        Files::new(
            fs::canonicalize(root).unwrap(),
            &requested,
            manifest,
            changed,
            super::super::policy::SourcePolicy::load(root, &Cancellation::default()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn upload_reuses_content_and_hash_from_the_manifest_scan() {
        let temp = tempfile::tempdir().unwrap();
        let mut files = prepare(temp.path(), &[("main.rs", "fn original() {}\n")], true);
        let expected = files.pending[0].hash.clone();
        fs::write(temp.path().join("main.rs"), "fn later_edit() {}\n").unwrap();
        let batch = files.prepare_next(&Cancellation::default()).unwrap();
        assert_eq!(batch.files[0].content, "fn original() {}\n");
        assert_eq!(batch.files[0].hash.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn server_requests_must_belong_to_the_submitted_manifest() {
        let temp = tempfile::tempdir().unwrap();
        for requested in [
            "../outside.txt".to_string(),
            temp.path().join("outside.txt").display().to_string(),
            "ignored.txt".to_string(),
        ] {
            let result = Files::new(
                temp.path().to_path_buf(),
                &[requested],
                Vec::new(),
                HashMap::new(),
                super::super::policy::SourcePolicy::load(temp.path(), &Cancellation::default())
                    .unwrap(),
            );
            assert!(result.is_err());
        }
    }

    #[test]
    fn duplicate_server_requests_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let files = prepare(temp.path(), &[("main.rs", "fn main() {}\n")], true);
        let result = Files::new(
            files.root,
            &["main.rs".into(), "main.rs".into()],
            files.pending.into(),
            files.changed,
            files.policy,
        );
        assert!(result.is_err());
    }

    #[test]
    fn reread_content_must_match_the_manifest_hash() {
        let temp = tempfile::tempdir().unwrap();
        let mut files = prepare(temp.path(), &[("main.rs", "fn old() {}\n")], false);
        fs::write(temp.path().join("main.rs"), "fn new() {}\n").unwrap();
        let error = files.prepare_next(&Cancellation::default()).unwrap_err();
        assert!(error.to_string().contains("changed after the manifest"));
    }

    #[test]
    fn reread_blank_content_is_rejected_before_upload() {
        let temp = tempfile::tempdir().unwrap();
        let mut files = prepare(temp.path(), &[("main.rs", "fn old() {}\n")], false);
        fs::write(temp.path().join("main.rs"), " \n\t").unwrap();
        assert!(files.prepare_next(&Cancellation::default()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_replacement_cannot_escape_the_checkout() {
        for retain in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("checkout");
            fs::create_dir(&root).unwrap();
            let mut files = prepare(&root, &[("main.rs", "fn main() {}\n")], retain);
            let outside = temp.path().join("outside.txt");
            fs::write(&outside, "TEST DATA OUTSIDE CHECKOUT").unwrap();
            fs::remove_file(root.join("main.rs")).unwrap();
            std::os::unix::fs::symlink(&outside, root.join("main.rs")).unwrap();
            let error = files.prepare_next(&Cancellation::default()).unwrap_err();
            assert!(error.to_string().contains("escapes its root"));
        }
    }

    #[test]
    fn tiny_files_are_split_by_count() {
        let temp = tempfile::tempdir().unwrap();
        let names: Vec<_> = (0..=UPLOAD_BATCH_FILES)
            .map(|n| format!("src/{n}.rs"))
            .collect();
        let entries: Vec<_> = names
            .iter()
            .map(|path| (path.as_str(), "fn tiny() {}\n"))
            .collect();
        let mut files = prepare(temp.path(), &entries, true);
        assert_eq!(
            files
                .prepare_next(&Cancellation::default())
                .unwrap()
                .files
                .len(),
            UPLOAD_BATCH_FILES
        );
        assert_eq!(
            files
                .prepare_next(&Cancellation::default())
                .unwrap()
                .files
                .len(),
            1
        );
    }

    #[test]
    fn large_files_are_read_one_batch_at_a_time() {
        let temp = tempfile::tempdir().unwrap();
        let content = "x\n".repeat(UPLOAD_BATCH_BYTES / 2);
        let mut files = prepare(
            temp.path(),
            &[("first.txt", &content), ("second.txt", &content)],
            false,
        );
        assert_eq!(
            files
                .prepare_next(&Cancellation::default())
                .unwrap()
                .files
                .len(),
            1
        );
        fs::remove_file(temp.path().join("second.txt")).unwrap();
        assert!(files.prepare_next(&Cancellation::default()).is_err());
    }

    fn response_error(status: u16) -> anyhow::Error {
        let status = reqwest::StatusCode::from_u16(status).unwrap();
        anyhow::Error::new(ResponseError::new(status, None, format!("PUT -> {status}")))
            .context("upload 1 files")
    }

    #[test]
    fn temporary_failures_are_retried_and_refusals_of_the_sync_stop_the_upload() {
        for status in [408, 429, 499, 500, 502, 503, 504] {
            assert_eq!(
                disposition(&response_error(status)),
                Disposition::Retry,
                "{status}"
            );
        }
        for status in [400, 413, 422] {
            assert_eq!(
                disposition(&response_error(status)),
                Disposition::Skip,
                "{status}"
            );
        }
        for status in [401, 403, 404, 409, 410] {
            assert_eq!(
                disposition(&response_error(status)),
                Disposition::Abort,
                "{status}"
            );
        }
        assert_eq!(
            disposition(&anyhow::anyhow!("serialize PUT body")),
            Disposition::Abort,
            "a local failure is not a server answer and must not be retried"
        );
    }
}
