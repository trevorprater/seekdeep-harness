//! `internal/plugin` creation, mutation, rollback, and contained disposal parity.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use seekdeep_cordis::{
    Context, CordisError, EventOptions, EventReply, FiberState, Plugin, PluginFiber, ServiceKey,
};
use serde_json::Value;

fn plugin(started: Arc<AtomicUsize>) -> Plugin {
    Plugin::new("observed", std::iter::empty::<String>(), move |_, _| {
        let started = started.clone();
        Box::pin(async move {
            started.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    })
}

#[tokio::test]
async fn creation_publishes_pending_and_allows_a_late_dependency_before_scheduling() {
    let context = Context::new();
    let seen = Arc::new(AtomicUsize::new(0));
    let observed = seen.clone();
    context
        .events()
        .on_sync(
            &context,
            "internal/plugin",
            move |_, args| {
                let fiber = args.get::<PluginFiber>(0).expect("plugin fiber");
                assert_eq!(fiber.fiber().state(), FiberState::Pending);
                fiber.add_inject("late")?;
                observed.fetch_add(1, Ordering::AcqRel);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let started = Arc::new(AtomicUsize::new(0));
    let fiber = context
        .plugin(plugin(started.clone()), Value::Null)
        .unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(seen.load(Ordering::Acquire), 1);
    assert_eq!(fiber.inject(), ["late"]);
    assert_eq!(fiber.fiber().state(), FiberState::Pending);
    assert_eq!(started.load(Ordering::Acquire), 0);

    context
        .provide(
            ServiceKey::<String>::new("late"),
            Arc::new("ready".to_owned()),
        )
        .unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(fiber.fiber().state(), FiberState::Active);
    assert_eq!(started.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn synchronous_creation_failure_rolls_back_without_starting_the_plugin() {
    let context = Context::new();
    context
        .events()
        .on_sync(
            &context,
            "internal/plugin",
            |_, _| anyhow::bail!("observer refused publication"),
            EventOptions::default(),
        )
        .unwrap();
    let started = Arc::new(AtomicUsize::new(0));
    let error = context
        .plugin(plugin(started.clone()), Value::Null)
        .unwrap_err();
    assert!(matches!(error, CordisError::PluginPublication(_)));
    assert!(error.to_string().contains("observer refused publication"));
    tokio::task::yield_now().await;
    assert_eq!(started.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn disposal_publication_contains_failures_and_notifies_every_observer() {
    let context = Context::new();
    let contained = Arc::new(AtomicUsize::new(0));
    context
        .events()
        .on_sync(
            &context,
            "internal/plugin",
            |_, args| {
                let fiber = args.get::<PluginFiber>(0).expect("plugin fiber");
                if fiber.is_disposed() {
                    anyhow::bail!("disposal observer failed");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let observed = contained.clone();
    context
        .events()
        .on_sync(
            &context,
            "internal/plugin",
            move |_, args| {
                let fiber = args.get::<PluginFiber>(0).expect("plugin fiber");
                if fiber.is_disposed() {
                    observed.fetch_add(1, Ordering::AcqRel);
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let fiber = context
        .plugin(plugin(Arc::new(AtomicUsize::new(0))), Value::Null)
        .unwrap();
    fiber.await_settled().await.unwrap();
    fiber.dispose().await.unwrap();
    assert_eq!(contained.load(Ordering::Acquire), 1);
    assert_eq!(fiber.fiber().state(), FiberState::Disposed);
}

#[tokio::test]
async fn loader_entry_metadata_reaches_both_lifecycle_publications() {
    let context = Context::new();
    let observed = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = observed.clone();
    context
        .events()
        .on_sync(
            &context,
            "internal/plugin",
            move |_, args| {
                let fiber = args.get::<PluginFiber>(0).expect("plugin fiber");
                seen.lock()
                    .push((fiber.entry_name(), fiber.entry_id(), fiber.is_disposed()));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let entry_context = context
        .with_meta("loader.entry_name", serde_json::json!("@fixture/entry"))
        .with_meta("loader.entry_id", serde_json::json!("entry-1"));
    let fiber = entry_context
        .plugin(plugin(Arc::new(AtomicUsize::new(0))), Value::Null)
        .unwrap();
    fiber.await_settled().await.unwrap();
    fiber.dispose().await.unwrap();
    assert_eq!(
        observed.lock().as_slice(),
        &[
            (
                Some("@fixture/entry".to_owned()),
                Some("entry-1".to_owned()),
                false,
            ),
            (
                Some("@fixture/entry".to_owned()),
                Some("entry-1".to_owned()),
                true,
            ),
        ]
    );
}
