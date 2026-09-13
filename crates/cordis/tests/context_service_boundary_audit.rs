//! Context registration admission, scoped mixins, and intercept-layer source boundaries.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::{
    Arc,
    atomic::{AtomicI32, AtomicUsize, Ordering},
};

use seekdeep_cordis::{Context, CordisError, Fiber, MixinMember, ServiceKey};
use serde_json::{Value, json};

const COUNTER: ServiceKey<AtomicI32> = ServiceKey::new("counter");

#[tokio::test]
async fn disposed_context_rejects_registration_before_declarations_or_notifications() {
    let root = Context::new();
    let owner = Fiber::active_child("disposed-provider");
    let disposed = root.with_fiber(owner.clone());
    owner.dispose().await.unwrap();
    let changes = Arc::new(AtomicUsize::new(0));
    root.on_service_change({
        let changes = changes.clone();
        move || {
            changes.fetch_add(1, Ordering::Relaxed);
        }
    })
    .unwrap();
    assert!(matches!(
        disposed.provide_named("never-published", Arc::new(7)),
        Err(CordisError::InactiveEffect)
    ));
    assert!(!root.has_property("never-published"));
    assert_eq!(root.service_revision(), 0);
    assert_eq!(changes.load(Ordering::Relaxed), 0);
    root.accessor_read_only("taken", |_| Ok(Some(Arc::new(8))))
        .unwrap();
    assert!(matches!(
        disposed.provide_named("taken", Arc::new(9)),
        Err(CordisError::InactiveEffect)
    ));
    assert!(matches!(
        disposed.accessor_read_only("taken", |_| Ok(Some(Arc::new(10)))),
        Err(CordisError::InactiveEffect)
    ));
}

#[tokio::test]
async fn reflected_mixins_read_and_write_the_calling_context_isolation() {
    let root = Context::new();
    let scoped = root.isolate(COUNTER);
    let outer = Arc::new(AtomicI32::new(1));
    let inner = Arc::new(AtomicI32::new(2));
    root.provide(COUNTER, outer.clone()).unwrap();
    scoped.provide(COUNTER, inner.clone()).unwrap();
    let mixin = root
        .mixin(
            COUNTER,
            [
                MixinMember::read_only("counter-value", |counter: &AtomicI32| {
                    Arc::new(counter.load(Ordering::Relaxed))
                })
                .with_setter(|counter, value| {
                    let value = value.downcast_ref::<i32>().unwrap();
                    counter.store(*value, Ordering::Relaxed);
                    Ok(true)
                }),
            ],
        )
        .unwrap();
    assert_eq!(*root.property::<i32>("counter-value").unwrap().unwrap(), 1);
    assert_eq!(
        *scoped.property::<i32>("counter-value").unwrap().unwrap(),
        2
    );
    assert!(
        scoped
            .set_property("counter-value", Arc::new(9_i32))
            .unwrap()
    );
    assert_eq!(outer.load(Ordering::Relaxed), 1);
    assert_eq!(inner.load(Ordering::Relaxed), 9);
    mixin.dispose().await.unwrap();
    assert!(!root.has_property("counter-value"));
    assert!(!scoped.has_property("counter-value"));
}

#[test]
fn custom_intercept_merge_omits_falsy_base_and_head_but_retains_intercept_entries() {
    let root = Context::new()
        .intercept("service", Value::Null)
        .intercept("service", json!(false))
        .intercept("service", json!({"value": 7}));
    for value in [Value::Null, json!(false), json!(0), json!(-0.0), json!("")] {
        assert_eq!(
            root.resolve_intercepted_with("service", Some(&value), Some(&value), |layers| {
                json!(layers)
            }),
            json!([null, false, {"value": 7}])
        );
    }
    assert_eq!(
        root.resolve_intercepted_with("service", Some(&json!([])), Some(&json!({})), |layers| {
            json!(layers)
        }),
        json!([[], null, false, {"value": 7}, {}])
    );
}
