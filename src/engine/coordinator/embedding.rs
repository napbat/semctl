//! Current-job evidence from the coordinator's existing embedding waiter.

use std::sync::{Arc, Weak};

use serde::{Deserialize, Serialize};

use super::{JOB_POLL_EVERY, ProgressSlot, lock};
use crate::client::{Client, api};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EmbeddingState {
    Queued,
    Running,
    Completed,
    Failed,
    Unavailable,
}

/// The last observation of this coordinator's current meaningful server job.
/// Completion is job evidence, not a live source-freshness certificate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EmbeddingStatus {
    pub(crate) job_id: String,
    pub(crate) state: EmbeddingState,
    pub(crate) files_embedded: Option<u64>,
    pub(crate) files_to_embed: Option<u64>,
    pub(crate) files_failed: Option<u64>,
    pub(crate) error: Option<String>,
}

#[derive(Default)]
pub(super) struct EmbeddingSlot {
    generation: u64,
    status: Option<EmbeddingStatus>,
}

/// A weak, generation-bound publisher. Superseded jobs cannot change current evidence.
#[derive(Clone)]
pub(crate) struct JobObserver {
    progress: Weak<ProgressSlot>,
    generation: u64,
}

impl ProgressSlot {
    pub(super) fn observe_job(self: &Arc<Self>, job_id: String) -> JobObserver {
        let mut slot = lock(&self.embedding);
        slot.generation = slot.generation.wrapping_add(1);
        slot.status = Some(EmbeddingStatus {
            job_id,
            state: EmbeddingState::Queued,
            files_embedded: None,
            files_to_embed: None,
            files_failed: None,
            error: None,
        });
        let generation = slot.generation;
        drop(slot);
        self.notify_status();
        JobObserver {
            progress: Arc::downgrade(self),
            generation,
        }
    }

    pub(super) fn embedding(&self) -> Option<EmbeddingStatus> {
        lock(&self.embedding).status.clone()
    }
}

impl JobObserver {
    fn update(&self, change: impl FnOnce(&mut EmbeddingStatus)) {
        let Some(progress) = self.progress.upgrade() else {
            return;
        };
        let mut slot = lock(&progress.embedding);
        if slot.generation != self.generation {
            return;
        }
        if let Some(status) = slot.status.as_mut() {
            change(status);
        }
        drop(slot);
        progress.notify_status();
    }

    fn observed(&self, job_id: &str, job: &api::JobStatus) -> Option<Result<(), String>> {
        let terminal = initial_job_result(job_id, job);
        self.update(|status| {
            status.files_embedded = u64::try_from(job.files_embedded).ok();
            status.files_to_embed = job.files_to_embed.and_then(|n| u64::try_from(n).ok());
            status.files_failed = u64::try_from(job.files_failed).ok();
            status.error = match &terminal {
                Some(Err(reason)) => Some(reason.clone()),
                _ => job.error.clone(),
            };
            status.state = match &terminal {
                Some(Ok(())) => EmbeddingState::Completed,
                Some(Err(_)) => EmbeddingState::Failed,
                None if job.files_failed > 0 => EmbeddingState::Failed,
                None if job.started_at.is_some() => EmbeddingState::Running,
                None => EmbeddingState::Queued,
            };
        });
        terminal
    }

    /// Also handles test reconcilers, which return a result without server counters.
    pub(super) fn finished(&self, result: &Result<(), String>) {
        self.update(|status| match result {
            Ok(()) => {
                status.state = if status.error.is_none() && status.files_failed.unwrap_or(0) == 0 {
                    EmbeddingState::Completed
                } else {
                    EmbeddingState::Failed
                }
            }
            Err(reason) => {
                if status.state != EmbeddingState::Unavailable {
                    status.state = EmbeddingState::Failed;
                }
                status.error = Some(reason.clone());
            }
        });
    }
}

/// Poll one job at the existing cadence. The owner cancels this future when superseded.
pub(super) async fn wait_for_embedding_job(
    client: &Client,
    job_id: &str,
    observer: &JobObserver,
) -> Result<(), String> {
    loop {
        let job = match client
            .get::<api::JobStatus>(&format!("/v1/jobs/{job_id}"))
            .await
        {
            Ok(job) => job,
            Err(error) => {
                let reason = format!("poll embedding job {job_id}: {error}");
                observer.update(|status| {
                    status.state = EmbeddingState::Unavailable;
                    status.error = Some(reason.clone());
                });
                return Err(reason);
            }
        };
        if let Some(result) = observer.observed(job_id, &job) {
            return result;
        }
        tokio::time::sleep(JOB_POLL_EVERY).await;
    }
}

/// A terminal timestamp with no failed files is completion. Invalid counters are not success.
pub(crate) fn initial_job_result(job_id: &str, job: &api::JobStatus) -> Option<Result<(), String>> {
    if let Some(error) = &job.error {
        return Some(Err(format!("embedding job {job_id} failed: {error}")));
    }
    if job.files_embedded < 0
        || job.files_failed < 0
        || job
            .files_to_embed
            .is_some_and(|n| n < 0 || job.files_embedded > n)
    {
        return Some(Err(format!(
            "embedding job {job_id} reported invalid progress counters"
        )));
    }
    job.completed_at.as_ref()?;
    if job.files_failed > 0 {
        return Some(Err(format!(
            "embedding job {job_id} completed with {} failed file(s)",
            job.files_failed
        )));
    }
    Some(Ok(()))
}

#[cfg(test)]
mod tests;
