//! Context service lifetimes, scoped mixins, and intercept-layer source boundaries.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
};

use seekdeep_cordis::{Context, CordisError, Fiber, FiberState, MixinMember, Plugin, ServiceKey};
use serde_json::{Value, json};

const COUNTER: ServiceKey<AtomicI32> = ServiceKey::new("counter");

#[tokio::test]
async fn provider_disposal_waits_for_affected_cleanup_without_waiting_for_other_scopes() {
    use seekdeep_cordis::fiber::EffectHandle;
    use tokio::sync::Semaphore;

    const READY: ServiceKey<()> = ServiceKey::new("ready");
    let context = Context::new();
    let owner = Fiber::active_child("provider");
    context
        .with_fiber(owner.clone())
        .provide(READY, Arc::new(()))
        .unwrap();
    let cleanup_entered = Arc::new(Semaphore::new(0));
    let cleanup_release = Arc::new(Semaphore::new(0));
    let cleaned = Arc::new(AtomicBool::new(false));
    let consumer = context
        .plugin(
            Plugin::new("consumer", [READY.name()], {
                let entered = cleanup_entered.clone();
                let release = cleanup_release.clone();
                let cleaned = cleaned.clone();
                move |ctx, _| {
                    let entered = entered.clone();
                    let release = release.clone();
                    let cleaned = cleaned.clone();
                    Box::pin(async move {
                        ctx.own(EffectHandle::new("held cleanup", move || {
                            Box::pin(async move {
                                entered.add_permits(1);
                                release.acquire_owned().await?.forget();
                                cleaned.store(true, Ordering::Release);
                                Ok(())
                            })
                        }))?;
                        Ok(())
                    })
                }
            }),
            Value::Null,
        )
        .unwrap();
    consumer.await_settled().await.unwrap();

    let isolated = context.isolate(READY);
    isolated.provide(READY, Arc::new(())).unwrap();
    let isolated_entered = Arc::new(Semaphore::new(0));
    let isolated_release = Arc::new(Semaphore::new(0));
    let unrelated = isolated
        .plugin(
            Plugin::new("isolated consumer", [READY.name()], {
                let entered = isolated_entered.clone();
                let release = isolated_release.clone();
                move |_, _| {
                    let entered = entered.clone();
                    let release = release.clone();
                    Box::pin(async move {
                        entered.add_permits(1);
                        release.acquire_owned().await?.forget();
                        Ok(())
                    })
                }
            }),
            Value::Null,
        )
        .unwrap();
    isolated_entered.acquire().await.unwrap().forget();

    let mut disposal = Box::pin(owner.dispose());
    assert!(futures::poll!(disposal.as_mut()).is_pending());
    assert!(context.get(READY).is_none());
    cleanup_entered.acquire().await.unwrap().forget();
    assert!(!cleaned.load(Ordering::Acquire));
    let finished = futures::poll!(disposal.as_mut()).is_ready();
    assert!(!finished);
    let mut trace = vec![json!({
        "cleaned": cleaned.load(Ordering::Acquire),
        "finished": finished,
        "otherScopeLoading": unrelated.fiber().state() == FiberState::Loading,
    })];
    cleanup_release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), disposal)
        .await
        .expect("provider disposal waited for an unrelated scope")
        .unwrap();
    assert!(cleaned.load(Ordering::Acquire));
    assert_eq!(consumer.fiber().state(), FiberState::Pending);
    assert_eq!(unrelated.fiber().state(), FiberState::Loading);
    trace.push(json!({
        "cleaned": cleaned.load(Ordering::Acquire),
        "finished": true,
        "otherScopeLoading": unrelated.fiber().state() == FiberState::Loading,
    }));
    isolated_release.add_permits(1);
    unrelated.await_settled().await.unwrap();
    context.fiber().dispose().await.unwrap();
    assert_eq!(Value::Array(trace), source_disposal_trace());
}

fn source_disposal_trace() -> Value {
    use std::process::Command;

    let source = seekdeep_source_oracle::source_root(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
    )
    .expect("pinned source checkout");
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--input-type=module",
            "--eval",
            r"import { Context, FiberState } from './vendor/cordis/src/index.ts';
const root = new Context();
const provider = await root.plugin(ctx => { ctx.provide('ready', {}); });
const entered = Promise.withResolvers();
const release = Promise.withResolvers();
let cleaned = false;
await root.inject(['ready'], ctx => {
  ctx.effect(() => async () => {
    entered.resolve();
    await release.promise;
    cleaned = true;
  });
});
const isolated = root.isolate('ready');
isolated.provide('ready', {});
const isolatedEntered = Promise.withResolvers();
const isolatedRelease = Promise.withResolvers();
const unrelated = isolated.inject(['ready'], async () => {
  isolatedEntered.resolve();
  await isolatedRelease.promise;
});
await isolatedEntered.promise;
let finished = false;
const disposal = provider.dispose().then(() => { finished = true; });
await entered.promise;
const trace = [{ cleaned, finished, otherScopeLoading: unrelated.state === FiberState.LOADING }];
release.resolve();
await disposal;
trace.push({ cleaned, finished, otherScopeLoading: unrelated.state === FiberState.LOADING });
isolatedRelease.resolve();
await unrelated;
await root.fiber.dispose();
console.log(JSON.stringify(trace));
",
        ])
        .current_dir(source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

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
