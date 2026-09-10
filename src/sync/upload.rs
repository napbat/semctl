//! Authorized, bounded uploads for one submitted manifest.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use tracing::debug;

use super::blocking::Cancellation;
use super::scan::PreparedFile;
use super::{SyncProgress, blocking, source, walker};
use crate::client::{Client, api};

const UPLOAD_BATCH_BYTES: usize = 4 * 1024 * 1024;
const UPLOAD_BATCH_FILES: usize = 256;
const UPLOAD_PARALLEL_REQUESTS: usize = 4;

/// Only entries from the submitted manifest can become pending uploads.
pub(super) struct Files {
    root: PathBuf,
    pending: VecDeque<api::ManifestEntry>,
    changed: HashMap<String, PreparedFile>,
    policy: super::policy::SourcePolicy,
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

    async fn next(mut self) -> Result<(Self, Vec<api::SyncFileContent>)> {
        blocking::run(move |cancellation| {
            let batch = self.prepare_next(&cancellation)?;
            Ok((self, batch))
        })
        .await
        .context("prepare upload batch")
    }

    fn prepare_next(&mut self, cancellation: &Cancellation) -> Result<Vec<api::SyncFileContent>> {
        self.policy.verify(cancellation)?;
        let mut batch = Vec::new();
        let mut bytes = 0;
        while let Some(entry) = self.pending.front() {
            cancellation.check()?;
            let size = usize::try_from(entry.size).context("invalid manifest file size")?;
            if !batch.is_empty()
                && (batch.len() >= UPLOAD_BATCH_FILES || bytes + size > UPLOAD_BATCH_BYTES)
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
            batch.push(api::SyncFileContent {
                path: path.clone(),
                content: prepared.content,
                hash: Some(prepared.hash),
            });
            self.pending.pop_front();
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
    on_progress: &impl Fn(&SyncProgress),
) -> Result<usize> {
    let total = files.pending.len();
    if total == 0 {
        return Ok(0);
    }
    report_upload_progress(on_progress, 0, total);
    let url = format!("/v1/codebases/{codebase_id}/sync/{job_id}");
    let (mut files, mut batch) = files.next().await?;
    // Preserve the single-request wire format used by older servers.
    if files.pending.is_empty() {
        let uploaded = upload_batch(client, &url, source_id, batch, None).await?;
        report_upload_progress(on_progress, uploaded, total);
        return Ok(uploaded);
    }

    let mut set = tokio::task::JoinSet::new();
    let mut uploaded = 0;
    loop {
        if set.len() >= UPLOAD_PARALLEL_REQUESTS {
            uploaded += join_one(&mut set).await?;
            debug!(uploaded, total, "uploaded batch");
            report_upload_progress(on_progress, uploaded, total);
        }
        let upload_client = client.clone();
        let upload_url = url.clone();
        let upload_source = source_id.to_string();
        set.spawn(async move {
            upload_batch(
                &upload_client,
                &upload_url,
                &upload_source,
                batch,
                Some(false),
            )
            .await
        });
        if files.pending.is_empty() {
            break;
        }
        (files, batch) = files.next().await?;
    }
    while !set.is_empty() {
        uploaded += join_one(&mut set).await?;
        report_upload_progress(on_progress, uploaded, total);
    }
    on_progress(&SyncProgress::Finalizing);
    client
        .put::<_, serde_json::Value>(
            &url,
            &api::SyncContentRequest {
                files: Vec::new(),
                source_id: source_id.to_string(),
                r#final: Some(true),
            },
        )
        .await
        .context("complete upload")?;
    Ok(uploaded)
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

async fn upload_batch(
    client: &Client,
    url: &str,
    source_id: &str,
    files: Vec<api::SyncFileContent>,
    final_batch: Option<bool>,
) -> Result<usize> {
    let n = files.len();
    client
        .put::<_, serde_json::Value>(
            url,
            &api::SyncContentRequest {
                files,
                source_id: source_id.to_string(),
                r#final: final_batch,
            },
        )
        .await
        .with_context(|| format!("upload {n} files"))?;
    Ok(n)
}

/// Await the next finished upload task, flattening the join error and the task's
/// own result into one `Result`.
async fn join_one(set: &mut tokio::task::JoinSet<Result<usize>>) -> Result<usize> {
    match set.join_next().await {
        Some(joined) => joined.map_err(|e| anyhow::anyhow!("upload task: {e}"))?,
        None => Ok(0),
    }
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
        assert_eq!(batch[0].content, "fn original() {}\n");
        assert_eq!(batch[0].hash.as_deref(), Some(expected.as_str()));
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
            files.prepare_next(&Cancellation::default()).unwrap().len(),
            UPLOAD_BATCH_FILES
        );
        assert_eq!(
            files.prepare_next(&Cancellation::default()).unwrap().len(),
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
            files.prepare_next(&Cancellation::default()).unwrap().len(),
            1
        );
        fs::remove_file(temp.path().join("second.txt")).unwrap();
        assert!(files.prepare_next(&Cancellation::default()).is_err());
    }
}
