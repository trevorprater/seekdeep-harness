//! Service lifecycle observation used by dependency-reconciled native adapters.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, ServiceKey};

const VALUE: ServiceKey<usize> = ServiceKey::new("value");

#[tokio::test]
async fn observes_provision_and_withdrawal_with_relaxed_visibility() {
    let context = Context::new();
    assert_eq!(context.service_slot_revision(VALUE), 0);
    let observations = Arc::new(Mutex::new(Vec::new()));
    let observer_context = context.clone();
    context
        .on_service_change({
            let observations = observations.clone();
            move || {
                observations
                    .lock()
                    .push(observer_context.get_relaxed(VALUE).map(|value| *value));
            }
        })
        .unwrap();
    let provision = context.provide(VALUE, Arc::new(7)).unwrap();
    assert_eq!(context.service_slot_revision(VALUE), 1);
    provision.dispose().await.unwrap();
    assert_eq!(context.service_slot_revision(VALUE), 2);
    assert_eq!(*observations.lock(), vec![Some(7), None]);
}

#[tokio::test]
async fn listener_failure_is_contained_and_listener_disposal_is_exact() {
    let context = Context::new();
    let later = Arc::new(AtomicUsize::new(0));
    context
        .on_service_change(|| panic!("observer failure"))
        .unwrap();
    let removable = context
        .on_service_change({
            let later = later.clone();
            move || {
                later.fetch_add(1, Ordering::AcqRel);
            }
        })
        .unwrap();
    let first = context.provide(VALUE, Arc::new(1)).unwrap();
    assert_eq!(later.load(Ordering::Acquire), 1);
    first.dispose().await.unwrap();
    assert_eq!(later.load(Ordering::Acquire), 2);
    removable.dispose().await.unwrap();
    context.provide(VALUE, Arc::new(2)).unwrap();
    assert_eq!(later.load(Ordering::Acquire), 2);
}

#[test]
fn checked_listener_rolls_back_a_rejected_service_before_observers_or_lookup() {
    let context = Context::new();
    let observations = Arc::new(AtomicUsize::new(0));
    context
        .on_service_change({
            let observations = observations.clone();
            move || {
                observations.fetch_add(1, Ordering::AcqRel);
            }
        })
        .unwrap();
    context
        .on_service_change_checked(|name| {
            anyhow::ensure!(name != "value", "value is forbidden");
            Ok(())
        })
        .unwrap();

    let error = context.provide(VALUE, Arc::new(1)).unwrap_err();
    assert!(matches!(
        error,
        seekdeep_cordis::CordisError::ServicePublication(ref message)
            if message.contains("value is forbidden")
    ));
    assert!(context.get(VALUE).is_none());
    assert_eq!(observations.load(Ordering::Acquire), 0);
    assert_eq!(context.service_revision(), 0);
    assert_eq!(context.service_slot_revision(VALUE), 0);
}

#[test]
fn service_guards_refuse_in_registration_order() {
    let context = Context::new();
    context
        .on_service_change_checked(|_| anyhow::bail!("guard 0 refused"))
        .unwrap();
    // Thirty-two guards, because the registry keys are UUIDv7 values minted at registration: they
    // differ between runs, and a hash-ordered guard set therefore picks a different iteration order
    // each time. With this many entries the first-registered guard is usually not the one iterated
    // first, so the assertion below fails. It is not a deterministic falsifier - a hash map can
    // happen to iterate in registration order, which is exactly how a four-guard version passed -
    // but a run against the hash-ordered set surfaced "guard 14 refused" where registration order
    // requires guard 0.
    for index in 1..32 {
        context
            .on_service_change_checked(move |_| anyhow::bail!("guard {index} refused"))
            .unwrap();
    }

    // The oracle's dispatch is ordered and bail-sensitive ("returns the first bail value"), so the
    // guard registered first is the one whose refusal the caller sees. Under a hash-ordered guard
    // set this assertion is decided by hashing rather than by registration.
    let error = context.provide(VALUE, Arc::new(1)).unwrap_err();
    assert!(
        matches!(
            error,
            seekdeep_cordis::CordisError::ServicePublication(ref message)
                if message.contains("guard 0 refused")
        ),
        "{error:?}"
    );
}
