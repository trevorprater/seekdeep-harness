//! Single-flight, failure retention, removal, and reconnect parity.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use seekdeep_client_ui_cordis::*;
use seekdeep_identity::SessionId;

fn row(id: &str) -> DynamicCordisInventoryRow {
    DynamicCordisInventoryRow {
        plugin_id: CordisDynamicPluginId::new(id),
        agent_id: SessionId::new("sess-1"),
        packages: Vec::new(),
        current_package_id: None,
        next_package_id: None,
        active_run: None,
        latest_run: None,
    }
}

#[test]
fn inventory_starts_unread_then_publishes_rows_and_honors_unsubscribe() {
    let inventory = CordisInventory::new();
    assert_eq!(*inventory.snapshot(), CordisInventorySnapshot::default());
    let changes = Arc::new(AtomicUsize::new(0));
    let observed = changes.clone();
    let subscription = inventory.subscribe(Arc::new(move || {
        observed.fetch_add(1, Ordering::Relaxed);
    }));
    let ticket = inventory.begin_refresh().unwrap();
    assert!(inventory.resolve(ticket, vec![row("dyn-1")]));
    assert!(inventory.snapshot().read);
    assert_eq!(inventory.snapshot().rows, vec![row("dyn-1")]);
    assert_eq!(changes.load(Ordering::Relaxed), 1);

    subscription.dispose();
    let ticket = inventory.begin_refresh().unwrap();
    assert!(inventory.resolve(ticket, vec![row("dyn-1")]));
    assert_eq!(changes.load(Ordering::Relaxed), 1);
}

#[test]
fn inventory_read_is_single_flight_and_slot_frees_after_settlement() {
    let inventory = CordisInventory::new();
    let first = inventory.begin_refresh().unwrap();
    assert_eq!(inventory.begin_refresh(), None);
    assert_eq!(inventory.begin_refresh(), None);
    assert!(inventory.resolve(first, vec![row("dyn-1")]));
    assert!(inventory.begin_refresh().is_some());
}

#[test]
fn failed_read_keeps_rows_and_reports_the_error() {
    let inventory = CordisInventory::new();
    let first = inventory.begin_refresh().unwrap();
    inventory.resolve(first, vec![row("dyn-1")]);
    let failed = inventory.begin_refresh().unwrap();
    inventory.reject(failed, Some("socket closed".to_owned()));
    let snapshot = inventory.snapshot();
    assert_eq!(snapshot.rows, vec![row("dyn-1")]);
    assert!(snapshot.read);
    assert_eq!(snapshot.error.as_deref(), Some("socket closed"));
}

#[test]
fn non_error_rejection_uses_the_source_fallback_message() {
    let inventory = CordisInventory::new();
    let ticket = inventory.begin_refresh().unwrap();
    inventory.reject(ticket, None);
    assert_eq!(
        inventory.snapshot().error.as_deref(),
        Some("reading the cordis inventory failed")
    );
}

#[test]
fn reset_forgets_host_rows_but_retains_explicit_removal_history() {
    let inventory = CordisInventory::new();
    let ticket = inventory.begin_refresh().unwrap();
    inventory.resolve(ticket, vec![row("dyn-1"), row("dyn-2")]);
    inventory.retire(&CordisDynamicPluginId::new("dyn-2"));
    inventory.reset();
    let snapshot = inventory.snapshot();
    assert!(snapshot.rows.is_empty());
    assert!(!snapshot.read);
    assert_eq!(
        snapshot.removed,
        [CordisDynamicPluginId::new("dyn-2")].into_iter().collect()
    );
}

#[test]
fn reconnect_discards_the_old_answer_and_accepts_the_fresh_read() {
    let inventory = CordisInventory::new();
    let stale = inventory.begin_refresh().unwrap();
    inventory.reset();
    let fresh = inventory.begin_refresh().unwrap();
    assert!(!inventory.resolve(stale, vec![row("old-host")]));
    assert_eq!(*inventory.snapshot(), CordisInventorySnapshot::default());
    assert!(inventory.resolve(fresh, vec![row("new-host")]));
    assert_eq!(inventory.snapshot().rows, vec![row("new-host")]);
}

#[test]
fn reconnect_swallows_the_old_failure_without_blame_or_notification() {
    let inventory = CordisInventory::new();
    let changes = Arc::new(AtomicUsize::new(0));
    let observed = changes.clone();
    let _subscription = inventory.subscribe(Arc::new(move || {
        observed.fetch_add(1, Ordering::Relaxed);
    }));
    let stale = inventory.begin_refresh().unwrap();
    inventory.reset();
    assert_eq!(changes.load(Ordering::Relaxed), 1);
    assert!(!inventory.reject(stale, Some("socket closed".to_owned())));
    assert_eq!(changes.load(Ordering::Relaxed), 1);
    assert_eq!(*inventory.snapshot(), CordisInventorySnapshot::default());
}
