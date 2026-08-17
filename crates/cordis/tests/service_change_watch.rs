//! Service lifecycle observation used by dependency-reconciled native adapters.

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
    provision.dispose().await.unwrap();
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
