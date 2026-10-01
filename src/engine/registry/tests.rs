//! Coordinator identity, reuse, idle release, and first-index gates.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::{CheckoutKey, CheckoutRegistry, CoordinatorLease};
use crate::client::Client;
use crate::engine::coordinator::IdleReconciler;
use crate::session::CredentialSource;

/// A registry whose reconciles never finish, so a coordinator keeps exactly the
/// state a test gave it.
fn registry(idle_grace: Duration) -> CheckoutRegistry {
    CheckoutRegistry::for_test(Arc::new(IdleReconciler), idle_grace)
}

/// A root no test creates: registration then fails, which keeps these tests off
/// the platform watcher, and the reconcile never touches it.
fn root(name: &str) -> PathBuf {
    PathBuf::from("/semctl-test-checkouts").join(name)
}

/// A checkout is one coordinator, whichever session asks for it. That is the
/// whole point of the registry: one watcher and one cache per checkout.
#[tokio::test]
async fn one_key_is_one_coordinator() {
    let registry = registry(Duration::from_mins(5));
    let client = Client::for_test("codebase", None);

    let first = registry
        .attach(client.clone(), root("shared"), Some(0))
        .await
        .expect("attach the first session");
    let second = registry
        .attach(client, root("shared"), Some(0))
        .await
        .expect("attach the second session");

    assert!(Arc::ptr_eq(first.coordinator(), second.coordinator()));
    assert_eq!(registry.len(), 1);
    assert_eq!(first.coordinator().status().await.leases, 2);
}

/// Two sessions with different credentials must never share a coordinator: one
/// may read and write what the other may not.
#[tokio::test]
async fn different_credential_scopes_do_not_share_a_checkout() {
    let registry = registry(Duration::from_mins(5));
    let stored = Client::for_test("codebase", None);
    let invocation = stored
        .clone()
        .with_credentials(CredentialSource::from_test_token("session-token"));

    let first = registry
        .attach(stored, root("same"), Some(0))
        .await
        .expect("attach the stored login");
    let second = registry
        .attach(invocation, root("same"), Some(0))
        .await
        .expect("attach the invocation token");

    assert!(!Arc::ptr_eq(first.coordinator(), second.coordinator()));
    assert_eq!(registry.len(), 2, "one root, two authorizations");
}

/// Different roots are different checkouts even under one codebase id.
#[tokio::test]
async fn different_roots_are_different_coordinators() {
    let registry = registry(Duration::from_mins(5));
    let client = Client::for_test("codebase", None);

    let first = registry
        .attach(client.clone(), root("first"), Some(0))
        .await
        .expect("attach the first checkout");
    let second = registry
        .attach(client, root("second"), Some(0))
        .await
        .expect("attach the second checkout");

    assert!(!Arc::ptr_eq(first.coordinator(), second.coordinator()));
    assert_eq!(registry.len(), 2);
}

/// A coordinator outlives the session that created it for the idle grace, and
/// then goes. Otherwise a process that serves 1,000 short sessions would keep
/// 1,000 watchers.
#[tokio::test]
async fn an_unleased_coordinator_is_released_after_the_idle_grace() {
    let registry = registry(Duration::ZERO);
    let client = Client::for_test("codebase", None);
    let lease = registry
        .attach(client.clone(), root("idle"), Some(0))
        .await
        .expect("attach");

    registry.sweep_idle();
    assert_eq!(registry.len(), 1, "a held coordinator is never released");

    drop(lease);
    registry.sweep_idle();
    assert_eq!(registry.len(), 0);

    // A later session gets a fresh coordinator rather than a cancelled one.
    let lease = registry
        .attach(client, root("idle"), Some(0))
        .await
        .expect("re-attach");
    assert_eq!(registry.len(), 1);
    assert_eq!(lease.coordinator().status().await.leases, 1);
}

/// Attaching inside the grace must reuse the coordinator, so a reconnecting
/// host keeps the warm content cache and the existing watch.
#[tokio::test]
async fn an_attach_inside_the_grace_reuses_the_coordinator() {
    let registry = registry(Duration::from_mins(5));
    let client = Client::for_test("codebase", None);
    let first = registry
        .attach(client.clone(), root("warm"), Some(0))
        .await
        .expect("attach");
    let coordinator = first.coordinator().clone();
    drop(first);
    registry.sweep_idle();

    let second = registry
        .attach(client, root("warm"), Some(0))
        .await
        .expect("re-attach");

    assert!(Arc::ptr_eq(second.coordinator(), &coordinator));
}

/// `index_codebase` waits on a gate that exists before the codebase is
/// registered, and two callers on one checkout share it.
#[tokio::test]
async fn a_first_index_gate_is_shared_while_it_is_pending() {
    let registry = registry(Duration::from_mins(5));
    let client = Client::for_test("codebase", None);

    let (first_lease, first_gate) = registry
        .attach_first_index(client.clone(), root("first-index"), Some(0))
        .await
        .expect("attach the first caller");
    let (_second_lease, second_gate) = registry
        .attach_first_index(client, root("first-index"), Some(0))
        .await
        .expect("attach the second caller");

    assert!(Arc::ptr_eq(&first_gate, &second_gate));
    assert!(Arc::ptr_eq(
        &first_lease
            .coordinator()
            .gate()
            .await
            .expect("the coordinator keeps its gate"),
        &first_gate
    ));
}

/// A failed first index must be retryable. The next caller gets a fresh gate,
/// not the old failure.
#[tokio::test]
async fn a_failed_first_index_gate_is_replaced() {
    let registry = registry(Duration::from_mins(5));
    let client = Client::for_test("codebase", None);
    let (_lease, failed) = registry
        .attach_first_index(client.clone(), root("retry"), Some(0))
        .await
        .expect("attach the first caller");
    failed.finish(Err("registration failed".to_string())).await;

    let (_lease, retried) = registry
        .attach_first_index(client.clone(), root("retry"), Some(0))
        .await
        .expect("attach the retrying caller");

    assert!(!Arc::ptr_eq(&failed, &retried));
    assert_eq!(retried.outcome().await, None, "the retry starts pending");

    // A successful gate, by contrast, is shared.
    retried.finish(Ok(())).await;
    let (_lease, reused) = registry
        .attach_first_index(client, root("retry"), Some(0))
        .await
        .expect("attach after success");
    assert!(Arc::ptr_eq(&retried, &reused));
}

/// A plain attach and a first index are the same checkout, so one coordinator
/// serves both and the gate stays with it.
#[tokio::test]
async fn a_plain_attach_and_a_first_index_share_one_coordinator() {
    let registry = registry(Duration::from_mins(5));
    let client = Client::for_test("codebase", None);
    let plain = registry
        .attach(client.clone(), root("both"), Some(0))
        .await
        .expect("plain attach");

    let (first_index, gate) = registry
        .attach_first_index(client, root("both"), Some(0))
        .await
        .expect("first index attach");

    assert!(Arc::ptr_eq(plain.coordinator(), first_index.coordinator()));
    assert_eq!(registry.len(), 1);
    assert!(Arc::ptr_eq(
        &plain
            .coordinator()
            .gate()
            .await
            .expect("the gate is on the shared coordinator"),
        &gate
    ));
}

/// The lease is what keeps a coordinator alive, and dropping it must not stop
/// the checkout for the sessions that still hold one.
#[tokio::test]
async fn releasing_one_lease_keeps_the_checkout_for_the_others() {
    let registry = registry(Duration::ZERO);
    let client = Client::for_test("codebase", None);
    let first = registry
        .attach(client.clone(), root("two-sessions"), Some(0))
        .await
        .expect("attach the first session");
    let second = registry
        .attach(client, root("two-sessions"), Some(0))
        .await
        .expect("attach the second session");

    drop(first);
    registry.sweep_idle();

    assert_eq!(registry.len(), 1);
    assert_eq!(second.coordinator().status().await.leases, 1);
}

/// The key is the identity of shared work. It must not depend on how the root
/// was spelled, and it must not carry a credential.
#[tokio::test]
async fn a_checkout_key_names_the_root_and_hides_the_credential() {
    let temp = tempfile::tempdir().expect("temporary checkout");
    let canonical = std::fs::canonicalize(temp.path()).expect("canonical checkout");
    let client = Client::for_test("codebase", None);

    let direct = CheckoutKey::for_client(&client, canonical.clone()).await;
    let indirect = CheckoutKey::for_client(&client, temp.path().join(".")).await;
    assert_eq!(direct, indirect, "one checkout is one key");
    assert_eq!(direct.root(), canonical);

    let with_token = client.with_credentials(CredentialSource::from_test_token("secret-token"));
    let scoped = CheckoutKey::for_client(&with_token, canonical).await;
    assert_ne!(direct, scoped);
    assert!(
        !format!("{scoped:?}").contains("secret-token"),
        "a key must never carry a token"
    );
}

/// A lease keeps its coordinator's identity available for the session's own
/// bookkeeping.
#[tokio::test]
async fn a_lease_reports_the_key_it_was_taken_for() {
    let registry = registry(Duration::from_mins(5));
    let client = Client::for_test("codebase", None);
    let lease = registry
        .attach(client.clone(), root("keyed"), Some(0))
        .await
        .expect("attach");

    assert_eq!(
        lease.key(),
        &CheckoutKey::for_client(&client, root("keyed")).await
    );
    assert_eq!(lease.coordinator().root(), root("keyed"));
}

/// Cancellation is ordered: the task first, then the watch, then the client.
/// After it, the coordinator reports no watcher and accepts no more runs.
#[tokio::test]
async fn cancelling_a_coordinator_releases_its_watch() {
    let registry = registry(Duration::ZERO);
    let client = Client::for_test("codebase", None);
    let lease = registry
        .attach(client, root("cancelled"), Some(0))
        .await
        .expect("attach");
    let coordinator = lease.coordinator().clone();
    let leases = CoordinatorLease::take(coordinator.clone());

    drop(lease);
    drop(leases);
    registry.sweep_idle();

    assert_eq!(registry.len(), 0);
    assert_eq!(
        coordinator.watcher_state(),
        crate::engine::coordinator::WatcherState::Unavailable("coordinator cancelled".to_string())
    );
}
