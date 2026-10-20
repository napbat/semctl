//! Best-effort checkout status for one connected MCP client.
//!
//! Observers hold weak coordinators, not leases. The serving scope stops them
//! even when another handler clone remains alive after a disconnect.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use rmcp::model::{CustomNotification, ServerNotification};
use rmcp::{Peer, RoleServer};
use serde::Serialize;
use tokio::task::JoinHandle;
use tracing::debug;

use crate::engine::coordinator::CheckoutCoordinator;
use crate::engine::{CheckoutKey, CoordinatorStatus};

const METHOD: &str = "notifications/semctl/sync_status";

#[derive(Default)]
pub(super) struct SyncNotifications {
    state: Mutex<Observers>,
}

#[derive(Default)]
struct Observers {
    connection: Connection,
    checkouts: HashMap<CheckoutKey, Observer>,
}

#[derive(Default)]
enum Connection {
    #[default]
    Waiting,
    Starting,
    Active(Peer<RoleServer>),
    Stopped,
}

struct Observer {
    coordinator: Weak<CheckoutCoordinator>,
    task: Option<JoinHandle<()>>,
}

/// The status shape stays identical to `sync_status`'s coordinator evidence.
/// A detached checkout has a null status and names the root to clear.
/// A null status without a root clears the session before its initial snapshots.
#[derive(Serialize)]
struct StatusNotification {
    version: u8,
    status: Option<CoordinatorStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<PathBuf>,
}

/// Keep this guard in the actual connection-serving scope, not a handler task.
pub(crate) struct SyncStatusSession(pub(super) Arc<SyncNotifications>);

impl SyncNotifications {
    pub(super) async fn start(&self, peer: Peer<RoleServer>) {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if !matches!(&state.connection, Connection::Waiting) {
                return;
            }
            state.connection = Connection::Starting;
        }
        // Finish the reset before publishing any checkout. Attachments that
        // arrive during this send stay queued with no peer or observer task.
        let reset = StatusNotification {
            version: 1,
            status: None,
            root: None,
        };
        if let Err(error) = send_status(&peer, reset).await {
            debug!(%error, "could not initialize sync status notifications");
            self.stop();
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !matches!(&state.connection, Connection::Starting) {
            return;
        }
        for observer in state.checkouts.values_mut() {
            observer.start(&peer);
        }
        state.connection = Connection::Active(peer);
    }

    pub(super) fn observe(&self, coordinator: &Arc<CheckoutCoordinator>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(&state.connection, Connection::Stopped) {
            return;
        }
        let peer = match &state.connection {
            Connection::Active(peer) => Some(peer.clone()),
            _ => None,
        };
        if let Entry::Vacant(entry) = state.checkouts.entry(coordinator.key().clone()) {
            let mut observer = Observer {
                coordinator: Arc::downgrade(coordinator),
                task: None,
            };
            if let Some(peer) = &peer {
                observer.start(peer);
            }
            entry.insert(observer);
        }
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.connection = Connection::Stopped;
        // Dropping each observer aborts its task, including a blocked send.
        state.checkouts.clear();
    }
}

impl Observer {
    fn start(&mut self, peer: &Peer<RoleServer>) {
        let Some(coordinator) = self.coordinator.upgrade() else {
            return;
        };
        let changes = coordinator.subscribe_status();
        let root = coordinator.root().to_path_buf();
        let coordinator = Arc::downgrade(&coordinator);
        self.task = Some(tokio::spawn(observe(
            coordinator,
            changes,
            root,
            peer.clone(),
        )));
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for SyncNotifications {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Drop for SyncStatusSession {
    fn drop(&mut self) {
        self.0.stop();
    }
}

async fn observe(
    coordinator: Weak<CheckoutCoordinator>,
    mut changes: tokio::sync::watch::Receiver<Option<crate::sync::SyncProgress>>,
    root: PathBuf,
    peer: Peer<RoleServer>,
) {
    loop {
        let status = if let Some(coordinator) = coordinator.upgrade() {
            Some(coordinator.status().await)
        } else {
            None
        };
        let detached = status.is_none();
        let notification = StatusNotification {
            version: 1,
            status,
            root: detached.then(|| root.clone()),
        };
        // The snapshot owns its data. No coordinator or session lock, strong
        // coordinator handle, or lease survives across the network send.
        if let Err(error) = send_status(&peer, notification).await {
            debug!(%error, "could not deliver checkout status notification");
            return;
        }
        if detached {
            // Notifications are best effort. A closed transport ends this
            // observer; it must not retry or affect reconciliation.
            return;
        }
        // A closed publisher also wakes the loop. Its weak coordinator is
        // then gone, so the next iteration sends the explicit detach.
        let _ = changes.changed().await;
    }
}

async fn send_status(
    peer: &Peer<RoleServer>,
    notification: StatusNotification,
) -> anyhow::Result<()> {
    let params = serde_json::to_value(notification)?;
    peer.send_notification(ServerNotification::CustomNotification(
        CustomNotification::new(METHOD, Some(params)),
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
