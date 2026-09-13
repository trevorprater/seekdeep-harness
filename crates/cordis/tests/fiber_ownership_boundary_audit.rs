//! Structural owners join cleanup already started through a native plugin handle.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::{FutureExt as _, channel::oneshot};
use seekdeep_cordis::{
    Context, DisposalScheduling, Fiber, FiberState, Plugin, fiber::EffectHandle,
};
use serde_json::Value;

#[tokio::test]
async fn parent_restart_joins_an_in_flight_direct_child_disposal() {
    let context = Context::new();
    let child = context
        .plugin(
            Plugin::new("owned child", std::iter::empty::<String>(), |_, _| {
                Box::pin(async { Ok(()) })
            }),
            Value::Null,
        )
        .unwrap();
    child.await_settled().await.unwrap();

    let (release, released) = oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = calls.clone();
    child
        .context()
        .own(EffectHandle::new("joined child cleanup", move || {
            entered.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                released.await?;
                Ok(())
            })
        }))
        .unwrap();

    let mut manual = Box::pin(child.dispose());
    assert!(manual.as_mut().now_or_never().is_none());
    assert_eq!(calls.load(Ordering::Acquire), 1);
    let mut parent = Box::pin(context.fiber().restart());
    assert!(
        parent.as_mut().now_or_never().is_none(),
        "the parent completed while its child's cleanup was still running"
    );
    release.send(()).unwrap();
    let (manual, parent) = futures::join!(manual, parent);
    manual.unwrap();
    parent.unwrap();
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(child.fiber().state(), FiberState::Disposed);
    assert_eq!(child.uid(), None);
    assert!(context.registry().is_empty());
}

#[tokio::test]
async fn cancelling_one_disposal_waiter_preserves_the_owned_cleanup_operation() {
    let context = Context::new();
    let child = context
        .plugin(
            Plugin::new("cancelled waiter", std::iter::empty::<String>(), |_, _| {
                Box::pin(async { Ok(()) })
            }),
            Value::Null,
        )
        .unwrap();
    child.await_settled().await.unwrap();
    let (release, released) = oneshot::channel();
    child
        .context()
        .own(EffectHandle::new("retained cleanup", move || {
            Box::pin(async move {
                released.await?;
                Ok(())
            })
        }))
        .unwrap();
    let mut first = Box::pin(child.dispose());
    assert!(first.as_mut().now_or_never().is_none());
    drop(first);
    release.send(()).unwrap();
    child.await_settled().await.unwrap();
    assert_eq!(child.fiber().state(), FiberState::Disposed);
    context.fiber().restart().await.unwrap();
}

#[test]
fn effect_cleanup_survives_cancellation_of_its_first_waiter() {
    let (release, released) = oneshot::channel();
    let effect = EffectHandle::new("retained effect", move || {
        Box::pin(async move {
            released.await?;
            anyhow::bail!("retained failure")
        })
    });
    let mut first = Box::pin(effect.dispose());
    assert!(first.as_mut().now_or_never().is_none());
    drop(first);
    release.send(()).unwrap();
    let observed = effect
        .dispose()
        .now_or_never()
        .expect("cleanup remains pollable");
    assert_eq!(observed.unwrap_err().to_string(), "retained failure");
    assert_eq!(
        futures::executor::block_on(effect.dispose())
            .unwrap_err()
            .to_string(),
        "retained failure"
    );
}

#[test]
fn cancelled_root_restart_retains_every_effect_and_finishes_its_generation() {
    let root = Fiber::root();
    let (release, released) = oneshot::channel();
    let gate = released.shared();
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..3 {
        let gate = gate.clone();
        let calls = calls.clone();
        root.own(EffectHandle::new("retained root effect", move || {
            calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                gate.await?;
                Ok(())
            })
        }))
        .unwrap();
    }
    let mut first = Box::pin(root.restart());
    assert!(first.as_mut().now_or_never().is_none());
    drop(first);
    release.send(()).unwrap();
    root.restart()
        .now_or_never()
        .expect("root teardown remains pollable")
        .unwrap();
    assert_eq!(calls.load(Ordering::Acquire), 3);
    assert_eq!(root.state(), FiberState::Active);
    root.own(EffectHandle::synchronous("next generation", || Ok(())))
        .unwrap();
    futures::executor::block_on(root.restart()).unwrap();
}

#[tokio::test]
async fn registry_join_starts_every_sibling_before_waiting_for_cleanup() {
    let context = Context::new();
    let plugin = Plugin::new("siblings", std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let (release, released) = oneshot::channel();
    let gate = released.shared();
    for _ in 0..2 {
        let child = context.plugin(plugin.clone(), Value::Null).unwrap();
        child.await_settled().await.unwrap();
        let entered = calls.clone();
        let gate = gate.clone();
        child
            .context()
            .own(EffectHandle::new("sibling cleanup", move || {
                entered.fetch_add(1, Ordering::AcqRel);
                Box::pin(async move {
                    gate.await?;
                    Ok(())
                })
            }))
            .unwrap();
    }
    let registry = context.registry();
    let mut disposal = Box::pin(registry.delete_joined(&plugin));
    assert!(disposal.as_mut().now_or_never().is_none());
    assert_eq!(calls.load(Ordering::Acquire), 2);
    release.send(()).unwrap();
    let removed = disposal.await.unwrap();
    assert!(removed.fibers.iter().all(|fiber| fiber.uid().is_none()));
    assert!(registry.is_empty());
    context.fiber().restart().await.unwrap();
}

#[test]
fn injected_concurrent_teardown_enters_every_disposer_before_awaiting() {
    let fiber = Fiber::root_with_disposal_scheduling(DisposalScheduling::Concurrent);
    let (release, released) = oneshot::channel();
    let gate = released.shared();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    for index in 0..3 {
        let gate = gate.clone();
        let order = order.clone();
        fiber
            .own(EffectHandle::new(format!("cleanup {index}"), move || {
                order.lock().unwrap().push(index);
                Box::pin(async move {
                    gate.await?;
                    Ok(())
                })
            }))
            .unwrap();
    }
    let mut disposal = Box::pin(fiber.restart());
    assert!(disposal.as_mut().now_or_never().is_none());
    assert_eq!(*order.lock().unwrap(), [2, 1, 0]);
    release.send(()).unwrap();
    futures::executor::block_on(disposal).unwrap();
}
