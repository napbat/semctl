//! The latest milestone of the reconcile that is running.
//!
//! A reconciler publishes each milestone from a synchronous callback, and a
//! status caller reads the latest one. The slot is a `watch` channel because
//! publishing and reading never wait and never poison, so the callback cannot
//! stall an upload and a status call cannot stall a reconcile.
//! Embedding snapshots use one short lock and wake this same channel.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::watch;

use crate::sync::SyncProgress;

use super::embedding::EmbeddingSlot;

/// One coordinator's progress slot.
pub(crate) struct ProgressSlot {
    latest: watch::Sender<Option<SyncProgress>>,
    running: AtomicBool,
    pub(super) embedding: Mutex<EmbeddingSlot>,
}

impl ProgressSlot {
    pub(super) fn new() -> Self {
        Self {
            latest: watch::Sender::new(None),
            running: AtomicBool::new(false),
            embedding: Mutex::new(EmbeddingSlot::default()),
        }
    }

    /// Record a milestone of the reconcile that is running. Only the latest
    /// one is kept.
    pub(crate) fn publish(&self, progress: &SyncProgress) {
        self.latest.send_replace(Some(progress.clone()));
    }

    /// The latest milestone, or `None` when no reconcile is running or none
    /// reported one yet.
    pub(super) fn latest(&self) -> Option<SyncProgress> {
        self.latest.borrow().clone()
    }

    pub(super) fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Milestones and lifecycle changes share one bounded, coalescing channel.
    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<SyncProgress>> {
        self.latest.subscribe()
    }

    /// Wake observers after a lifecycle or first-index gate transition.
    pub(crate) fn notify_status(&self) {
        self.latest.send_modify(|_| {});
    }

    /// Hold this guard for the length of one reconcile. Its drop clears the
    /// slot, so a reconcile that ends, fails, panics, or is cancelled leaves
    /// no stale milestone behind.
    pub(super) fn scope(&self) -> ProgressScope<'_> {
        self.running.store(true, Ordering::Release);
        self.notify_status();
        ProgressScope { slot: self }
    }
}

/// Clears its slot when dropped. See [`ProgressSlot::scope`].
pub(super) struct ProgressScope<'slot> {
    slot: &'slot ProgressSlot,
}

impl Drop for ProgressScope<'_> {
    fn drop(&mut self) {
        self.slot.running.store(false, Ordering::Release);
        self.slot.latest.send_replace(None);
    }
}
