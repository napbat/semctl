//! Ownership and cooperative cancellation for sync filesystem work.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, ensure};
use tokio::task::JoinHandle;

/// A blocking worker checks this flag between filesystem operations.
#[derive(Clone, Default)]
pub(super) struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub(super) fn check(&self) -> Result<()> {
        ensure!(!self.0.load(Ordering::Acquire), "sync cancelled");
        Ok(())
    }
}

struct Worker<T> {
    cancellation: Cancellation,
    task: JoinHandle<Result<T>>,
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.cancellation.0.store(true, Ordering::Release);
        // Abort prevents a queued worker from starting. An active filesystem
        // operation must finish before the worker can observe cancellation.
        self.task.abort();
    }
}

/// Run owned filesystem work outside the runtime workers. The closure must
/// retain every lock it needs until it exits, including after cancellation.
pub(super) async fn run<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(Cancellation) -> Result<T> + Send + 'static,
{
    let cancellation = Cancellation::default();
    let worker_cancellation = cancellation.clone();
    let mut worker = Worker {
        cancellation,
        task: tokio::task::spawn_blocking(move || operation(worker_cancellation)),
    };
    (&mut worker.task).await.context("sync filesystem worker")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn filesystem_work_runs_on_a_different_thread() {
        let runtime_thread = std::thread::current().id();
        let worker_thread = run(|_| Ok(std::thread::current().id())).await.unwrap();
        assert_ne!(runtime_thread, worker_thread);
    }

    #[tokio::test]
    async fn dropping_the_caller_cancels_its_active_worker() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(run(move |cancellation| {
            started_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            cancelled_tx.send(cancellation.check().is_err()).unwrap();
            Ok(())
        }));
        started_rx.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        resume_tx.send(()).unwrap();
        assert!(cancelled_rx.await.unwrap());
    }

    #[tokio::test]
    async fn cancellation_keeps_the_worker_lock_until_the_worker_exits() {
        let state = Arc::new(tokio::sync::Mutex::new(()));
        let owned = state.clone().lock_owned().await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(run(move |_| {
            let guard = owned;
            started_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            drop(guard);
            finished_tx.send(()).unwrap();
            Ok(())
        }));
        started_rx.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(state.try_lock().is_err());
        resume_tx.send(()).unwrap();
        finished_rx.await.unwrap();
        assert!(state.try_lock().is_ok());
    }
}
