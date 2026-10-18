//! Trigger coalescing and failure cooldowns for one checkout.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use tokio::time::{Instant, Interval};

use super::{
    CheckoutCoordinator, Reconciler, SyncLimits, Trigger, WatchBatch, WatcherState,
    refresh_external_watches, relevance,
};

const RETRY_DELAY: Duration = Duration::from_secs(5);
const RETRY_DELAY_MAX: Duration = Duration::from_secs(60);

pub(super) struct ReconcileLoop {
    pub(super) coordinator: Weak<CheckoutCoordinator>,
    pub(super) triggers: mpsc::Receiver<Trigger>,
    pub(super) batches: mpsc::Receiver<WatchBatch>,
    pub(super) reconciler: Arc<dyn Reconciler>,
    pub(super) scan_permits: Arc<Semaphore>,
    pub(super) limits: SyncLimits,
}

impl ReconcileLoop {
    pub(super) async fn run(mut self) {
        let Some(coordinator) = self.coordinator.upgrade() else {
            return;
        };
        let root = coordinator.root.clone();
        let overflow = coordinator.overflow.clone();
        let immediate_overflow = coordinator.immediate_overflow.clone();
        let overflow_ready = coordinator.overflow_ready.clone();
        let mut timer = coordinator.periodic_timer();
        let mut watching = matches!(coordinator.watcher_state(), WatcherState::Active);
        // A cooldown holds neither a coordinator nor a scan permit.
        drop(coordinator);

        let mut observed: HashSet<PathBuf> = HashSet::new();
        let mut pending = None;
        let mut retry = RetryBackoff::default();
        loop {
            let mut due = None;
            let mut events = Vec::new();
            tokio::select! {
                trigger = self.triggers.recv() => match trigger {
                    Some(trigger) => due = Some(trigger),
                    // No coordinator remains to own or trigger this checkout.
                    None => return,
                },
                batch = self.batches.recv(), if watching => match batch {
                    Some(batch) => events = batch.events,
                    None => watching = false,
                },
                () = next_tick(&mut timer) => due = Some(Trigger::Periodic),
                () = retry.wait(), if pending.is_some() => {},
                () = overflow_ready.notified() => {},
            }

            // Consume bursts without moving the failure deadline. Explicit
            // requests win coalescing so automatic events cannot delay them.
            while let Ok(trigger) = self.triggers.try_recv() {
                due = coalesce(due, Some(trigger));
            }
            while let Ok(batch) = self.batches.try_recv() {
                events.extend(batch.events);
            }
            if overflow.swap(false, Ordering::AcqRel) {
                due = coalesce(due, Some(Trigger::Watch));
            }
            if immediate_overflow.swap(false, Ordering::AcqRel) {
                due = coalesce(due, Some(Trigger::Explicit));
            }
            // A pending retry already scans every path. Extra events do not
            // need another policy read while that retry waits.
            if due.is_none() && pending.is_none() && !events.is_empty() {
                let (relevant, next_observed) = relevance(&root, events, observed).await;
                observed = next_observed;
                if relevant {
                    due = Some(Trigger::Watch);
                }
            }
            // Only new requests bypass the cooldown. Retrying a failed
            // startup preserves its job reporting without bypassing the wait.
            let immediate = due.is_some_and(Trigger::bypasses_cooldown);
            pending = coalesce(pending, due);
            let Some(trigger) = pending else { continue };
            if !immediate && retry.is_waiting() {
                continue;
            }
            pending = None;
            let Some(coordinator) = self.coordinator.upgrade() else {
                return;
            };
            let retry_needed = coordinator
                .run_once(trigger, &self.reconciler, &self.scan_permits, &self.limits)
                .await;
            if retry_needed {
                retry.failed();
                pending = Some(trigger);
            } else {
                retry = RetryBackoff::default();
            }
            coordinator.run.lock().await.retry_at = retry.deadline;
            coordinator.progress.notify_status();
            // A rule can select another directory. Subscribe after each run.
            if watching {
                refresh_external_watches(&coordinator).await;
            }
            drop(coordinator);
        }
    }
}

/// Keep one reason to run. Immediate requests take priority over automatic
/// triggers, including when both arrive while another reconcile runs.
fn coalesce(pending: Option<Trigger>, incoming: Option<Trigger>) -> Option<Trigger> {
    match (pending, incoming) {
        (Some(Trigger::Explicit), _) | (_, Some(Trigger::Explicit)) => Some(Trigger::Explicit),
        (Some(Trigger::Startup), _) | (_, Some(Trigger::Startup)) => Some(Trigger::Startup),
        (pending, incoming) => pending.or(incoming),
    }
}

#[derive(Default)]
struct RetryBackoff {
    delay: Duration,
    deadline: Option<Instant>,
}

impl RetryBackoff {
    fn failed(&mut self) {
        self.delay = if self.delay.is_zero() {
            RETRY_DELAY
        } else {
            self.delay.saturating_mul(2).min(RETRY_DELAY_MAX)
        };
        self.deadline = Some(Instant::now() + self.delay);
    }

    fn is_waiting(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() < deadline)
    }

    async fn wait(&self) {
        match self.deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    }
}

/// A disabled periodic timer must not disable failure retries.
async fn next_tick(timer: &mut Option<Interval>) {
    match timer {
        Some(timer) => {
            timer.tick().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests;
