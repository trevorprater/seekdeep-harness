//! Real native registry and compiled companion startup through the test host.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, EventOptions, EventReply, FiberState, Plugin, PluginFiber,
    TEST_INVARIANT_READY_SERVICE,
};
use seekdeep_invariants::{
    INVARIANTS, InvariantInstaller,
    noop::NOOP_INVARIANTS,
    test_host::{TestInvariantCompanions, TestInvariantHost, companion_loader},
};
use seekdeep_schemastery::ValidationError;
use serde_json::{Value, json};
use tokio::sync::oneshot;

const OWNER: &str = "../packages/core/tools/src/invariant.ts";
const TEST_PATH: &str = "/repo/packages/core/tools/tests/tools.spec.ts";

fn spawn(future: BoxFuture<'static, ()>) {
    tokio::spawn(future);
}

fn make_host(root: &Context, path: &str, companions: TestInvariantCompanions) -> TestInvariantHost {
    TestInvariantHost::new(root.clone(), path, companions, None, spawn)
}

fn companions(
    plugins: impl IntoIterator<Item = (&'static str, Plugin)>,
) -> TestInvariantCompanions {
    Arc::new(parking_lot::RwLock::new(
        plugins
            .into_iter()
            .map(|(path, plugin)| {
                (
                    path.to_owned(),
                    companion_loader(move || {
                        let plugin = plugin.clone();
                        async move { Ok(plugin) }
                    }),
                )
            })
            .collect(),
    ))
}

fn probe(name: &str, calls: &Arc<AtomicUsize>) -> Plugin {
    let calls = calls.clone();
    Plugin::new(name, std::iter::empty::<String>(), move |_, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    })
}

struct Delayed {
    plugin: Plugin,
    started: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
}

fn delayed() -> Delayed {
    let (started, ready) = oneshot::channel();
    let (release, wait) = oneshot::channel();
    let channels = Arc::new(Mutex::new(Some((started, wait))));
    let plugin = Plugin::new("delayed-companion", ["invariants"], move |_, _| {
        let (started, wait) = channels.lock().take().unwrap();
        Box::pin(async move {
            let _ = started.send(());
            wait.await?;
            Ok(())
        })
    });
    Delayed {
        plugin,
        started: ready,
        release,
    }
}

#[tokio::test]
async fn exhaustive_topology_loads_all_98_compiled_companions_and_reserves_every_owner() {
    let root = Context::new();
    let loaders = TestInvariantCompanions::default();
    let loads = Arc::new(AtomicUsize::new(0));
    for descriptor in NOOP_INVARIANTS {
        let loads = loads.clone();
        loaders.write().insert(
            format!("../{}", descriptor.source_surface()),
            companion_loader(move || {
                loads.fetch_add(1, Ordering::SeqCst);
                async move { Ok(descriptor.plugin()) }
            }),
        );
    }
    let host = make_host(&root, "/repo/scripts/test-invariants.spec.ts", loaders);
    let target = host
        .plugin(&root, probe("probe", &Arc::default()), Value::Null)
        .unwrap();
    target.await_ready().await.unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 98);
    let registry = root.get(INVARIANTS).unwrap();
    for descriptor in NOOP_INVARIANTS {
        assert!(registry.is_registered(descriptor.package_name()));
        assert!(
            registry
                .register(descriptor.package_name(), InvariantInstaller::noop())
                .unwrap_err()
                .to_string()
                .contains("already registered")
        );
    }
    root.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn manual_tree_is_untouched_and_non_package_roots_are_service_only() {
    let root = Context::new();
    let host = make_host(
        &root,
        "/repo/packages/core/tools/tests/invariant.spec.ts",
        TestInvariantCompanions::default(),
    );
    let target = host
        .plugin(&root, probe("manual", &Arc::default()), Value::Null)
        .unwrap();
    target.await_ready().await.unwrap();
    assert!(root.get(INVARIANTS).is_none());
    assert!(target.raw().inject().is_empty());
    root.fiber().restart().await.unwrap();

    let root = Context::new();
    let host = make_host(
        &root,
        "/repo/examples/headless-agent/tests/index.spec.ts",
        TestInvariantCompanions::default(),
    );
    let target = host
        .plugin(&root, probe("ordinary", &Arc::default()), Value::Null)
        .unwrap();
    target.await_ready().await.unwrap();
    assert!(root.get(INVARIANTS).is_some());
    assert_eq!(target.raw().inject(), [TEST_INVARIANT_READY_SERVICE]);
    root.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn pending_validation_failure_is_disposed_and_repeated_waiters_share_the_cause() {
    let root = Context::new();
    let control = delayed();
    let host = make_host(&root, TEST_PATH, companions([(OWNER, control.plugin)]));
    let calls = Arc::new(AtomicUsize::new(0));
    let plugin = probe("invalid", &calls).with_config_validator(|_| {
        Err(ValidationError {
            message: "$.requiredValue is required".to_owned(),
            path: Vec::new(),
        }
        .into())
    });
    let target = host.plugin(&root, plugin, json!({})).unwrap();
    control.started.await.unwrap();
    assert_eq!(target.raw().fiber().state(), FiberState::Pending);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    control.release.send(()).unwrap();
    let first = target.await_ready().await.unwrap_err();
    assert!(first.downcast_ref::<ValidationError>().is_some());
    assert!(Arc::ptr_eq(
        &first,
        &target.await_ready().await.unwrap_err()
    ));
    assert_eq!(target.raw().fiber().state(), FiberState::Disposed);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    root.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn valid_callback_failure_remains_inspectable_after_delayed_readiness() {
    let root = Context::new();
    let control = delayed();
    let host = make_host(&root, TEST_PATH, companions([(OWNER, control.plugin)]));
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = calls.clone();
    let plugin = Plugin::new("failed", std::iter::empty::<String>(), move |_, _| {
        callback_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { anyhow::bail!("valid plugin apply failed") })
    })
    .with_config_validator(|value| Ok(value.clone()));
    let target = host.plugin(&root, plugin.clone(), json!({})).unwrap();
    control.started.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    control.release.send(()).unwrap();
    let failure = target.await_ready().await.unwrap_err();
    assert!(Arc::ptr_eq(&failure, &target.raw().failure().unwrap()));
    assert_eq!(failure.to_string(), "valid plugin apply failed");
    assert_eq!(target.raw().fiber().state(), FiberState::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(root.registry().get(&plugin).unwrap().fibers.len(), 1);
    root.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn companions_and_targets_can_await_nested_startup_and_explicit_mounts_deduplicate() {
    let root = Context::new();
    let control = delayed();
    let delayed_plugin = control.plugin.clone();
    let order = Arc::new(Mutex::new(Vec::new()));
    let companion_order = order.clone();
    let nested = Plugin::new("nested-companion", ["invariants"], move |context, _| {
        let order = companion_order.clone();
        Box::pin(async move {
            context
                .plugin(
                    Plugin::new(
                        "companion-child",
                        std::iter::empty::<String>(),
                        move |_, _| {
                            order.lock().push("companion-child");
                            Box::pin(async { Ok(()) })
                        },
                    ),
                    Value::Null,
                )?
                .await_settled()
                .await?;
            Ok(())
        })
    });
    let host = make_host(
        &root,
        "/repo/scripts/test-invariants.spec.ts",
        companions([
            (OWNER, control.plugin),
            ("../packages/core/session/src/invariant.ts", nested),
        ]),
    );
    let target_order = order.clone();
    let target_plugin = Plugin::new("target", ["existing"], move |context, _| {
        let order = target_order.clone();
        Box::pin(async move {
            order.lock().push("target");
            context
                .plugin(
                    Plugin::new("target-child", std::iter::empty::<String>(), move |_, _| {
                        order.lock().push("target-child");
                        Box::pin(async { Ok(()) })
                    }),
                    Value::Null,
                )?
                .await_settled()
                .await?;
            Ok(())
        })
    });
    root.provide_named("existing", Arc::new(true)).unwrap();
    let target = host
        .plugin(&root, target_plugin.clone(), Value::Null)
        .unwrap();
    assert_eq!(
        target.raw().inject(),
        ["existing", TEST_INVARIANT_READY_SERVICE]
    );
    assert_eq!(
        root.registry().get(&target_plugin).unwrap().plugin_id,
        target_plugin.id()
    );
    control.started.await.unwrap();
    assert!(!order.lock().contains(&"target"));
    control.release.send(()).unwrap();
    target.await_ready().await.unwrap();
    assert_eq!(*order.lock(), ["companion-child", "target", "target-child"]);
    let derived = root
        .isolate_named("unrelated")
        .intercept("unrelated", json!({}));
    for context in [&root, &derived] {
        host.plugin(context, host.registry_plugin(), Value::Null)
            .unwrap()
            .await_ready()
            .await
            .unwrap();
        host.plugin(context, delayed_plugin.clone(), Value::Null)
            .unwrap()
            .await_ready()
            .await
            .unwrap();
    }
    assert_eq!(
        root.registry()
            .get(&host.registry_plugin())
            .unwrap()
            .fibers
            .len(),
        1
    );
    assert_eq!(
        root.registry().get(&delayed_plugin).unwrap().fibers.len(),
        1
    );
    root.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn root_derived_and_externally_registered_pending_children_share_the_barrier() {
    let root = Context::new();
    let control = delayed();
    let host = make_host(&root, TEST_PATH, companions([(OWNER, control.plugin)]));
    let calls = Arc::new(AtomicUsize::new(0));
    let target = host
        .plugin(&root, probe("root-target", &calls), Value::Null)
        .unwrap();
    let derived = root
        .isolate_named("derived")
        .intercept("derived", json!({}));
    let derived_target = host
        .plugin(&derived, probe("derived-target", &calls), Value::Null)
        .unwrap();
    let child = target
        .raw()
        .context()
        .plugin(probe("pending-child", &calls), Value::Null)
        .unwrap();
    control.started.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for raw in [target.raw(), derived_target.raw(), &child] {
        assert_eq!(raw.fiber().state(), FiberState::Pending);
        assert!(
            raw.inject()
                .iter()
                .any(|name| name == TEST_INVARIANT_READY_SERVICE)
        );
    }
    control.release.send(()).unwrap();
    target.await_ready().await.unwrap();
    derived_target.await_ready().await.unwrap();
    child.await_settled().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    root.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn lazy_load_and_companion_startup_failures_reject_without_starting_targets() {
    for phase in ["load", "startup"] {
        let root = Context::new();
        let failure = Arc::new(anyhow::anyhow!("companion {phase} failed"));
        let loaders = TestInvariantCompanions::default();
        let lazy_failure = failure.clone();
        let failing = Plugin::new("failing-companion", ["invariants"], |_, _| {
            Box::pin(async { anyhow::bail!("companion startup failed") })
        });
        let companion = failing.clone();
        loaders.write().insert(
            OWNER.to_owned(),
            companion_loader(move || {
                let failure = lazy_failure.clone();
                let plugin = companion.clone();
                async move {
                    if phase == "load" {
                        Err(failure)
                    } else {
                        Ok(plugin)
                    }
                }
            }),
        );
        let host = make_host(&root, TEST_PATH, loaders);
        let calls = Arc::new(AtomicUsize::new(0));
        let target = host
            .plugin(&root, probe("target", &calls), Value::Null)
            .unwrap();
        let error = target.await_ready().await.unwrap_err();
        if phase == "load" {
            assert!(Arc::ptr_eq(&error, &failure));
        } else {
            let failed = root.registry().get(&failing).unwrap().fibers[0].clone();
            assert!(Arc::ptr_eq(&error, &failed.failure().unwrap()));
        }
        assert!(Arc::ptr_eq(
            &error,
            &target.await_ready().await.unwrap_err()
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(target.raw().fiber().state(), FiberState::Pending);
        target.dispose().await.unwrap();
        assert_eq!(target.raw().fiber().state(), FiberState::Disposed);
        root.fiber().restart().await.unwrap();
    }
}

#[tokio::test]
async fn disposal_of_pending_target_does_not_wait_for_companion_readiness() {
    let root = Context::new();
    let control = delayed();
    let host = make_host(&root, TEST_PATH, companions([(OWNER, control.plugin)]));
    let calls = Arc::new(AtomicUsize::new(0));
    let target = host
        .plugin(&root, probe("disposed", &calls), Value::Null)
        .unwrap();
    control.started.await.unwrap();
    target.dispose().await.unwrap();
    assert_eq!(target.raw().fiber().state(), FiberState::Disposed);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    control.release.send(()).unwrap();
    target.await_ready().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    root.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn a_failed_loader_does_not_skip_or_cancel_other_lazy_module_loads() {
    let root = Context::new();
    let loaders = TestInvariantCompanions::default();
    let failure = Arc::new(anyhow::anyhow!("first lazy import failed"));
    let failed = failure.clone();
    loaders.write().insert(
        OWNER.to_owned(),
        companion_loader(move || {
            let failure = failed.clone();
            async move { Err(failure) }
        }),
    );
    let (release, wait) = oneshot::channel();
    let (finished, complete) = oneshot::channel();
    let channels = Arc::new(Mutex::new(Some((wait, finished))));
    let loaded = Plugin::new("late-companion", ["invariants"], |_, _| {
        Box::pin(async { Ok(()) })
    });
    let module = loaded.clone();
    let invoked = Arc::new(AtomicUsize::new(0));
    let calls = invoked.clone();
    loaders.write().insert(
        "../packages/z/late/src/invariant.ts".to_owned(),
        companion_loader(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            let (wait, finished) = channels.lock().take().unwrap();
            let module = module.clone();
            async move {
                wait.await.unwrap();
                let _ = finished.send(());
                Ok(module)
            }
        }),
    );
    let host = make_host(&root, "/repo/scripts/test-invariants.spec.ts", loaders);
    let target = host
        .plugin(&root, probe("target", &Arc::default()), Value::Null)
        .unwrap();
    assert!(Arc::ptr_eq(
        &target.await_ready().await.unwrap_err(),
        &failure
    ));
    assert_eq!(invoked.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    complete.await.unwrap();
    assert!(!root.registry().has(&loaded));
    root.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn rejected_companion_publication_does_not_grant_later_targets_a_barrier_bypass() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let companion = probe("rejected-companion", &calls).with_additional_inject(["invariants"]);
    let rejected_id = companion.id();
    let reject = root
        .events()
        .on_sync(
            &root,
            "internal/plugin",
            move |_, args| {
                if args
                    .get::<PluginFiber>(0)
                    .is_some_and(|fiber| fiber.plugin_id() == rejected_id)
                {
                    anyhow::bail!("companion publication rejected");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let host = make_host(&root, TEST_PATH, companions([(OWNER, companion.clone())]));
    let first = host
        .plugin(&root, probe("first", &Arc::default()), Value::Null)
        .unwrap();
    let failure = first.await_ready().await.unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("companion publication rejected")
    );
    reject.dispose().await.unwrap();
    let later = host.plugin(&root, companion, Value::Null).unwrap();
    assert!(Arc::ptr_eq(
        &failure,
        &later.await_ready().await.unwrap_err()
    ));
    later.raw().await_settled().await.unwrap();
    assert!(
        later
            .raw()
            .inject()
            .iter()
            .any(|name| name == TEST_INVARIANT_READY_SERVICE)
    );
    assert_eq!(later.raw().fiber().state(), FiberState::Pending);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    root.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn missing_vanished_malformed_and_inactive_companions_fail_at_their_own_boundary() {
    let root = Context::new();
    let host = make_host(&root, TEST_PATH, TestInvariantCompanions::default());
    let error = host
        .plugin(&root, probe("missing", &Arc::default()), Value::Null)
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        format!("test invariants: package test has no companion at {OWNER}")
    );
    root.fiber().restart().await.unwrap();

    for case in ["vanished", "malformed", "inactive"] {
        let root = Context::new();
        let inject = if case == "malformed" {
            vec![]
        } else if case == "inactive" {
            vec!["invariants", "missingDependency"]
        } else {
            vec!["invariants"]
        };
        let plugin = Plugin::new("companion", inject, |_, _| Box::pin(async { Ok(()) }));
        let loaders = companions([(OWNER, plugin)]);
        let host = make_host(&root, TEST_PATH, loaders.clone());
        let target = host
            .plugin(&root, probe("target", &Arc::default()), Value::Null)
            .unwrap();
        if case == "vanished" {
            loaders.write().clear();
        }
        let error = target.await_ready().await.unwrap_err();
        let expected = match case {
            "vanished" => format!("test invariants: selected companion vanished at {OWNER}"),
            "malformed" => format!("test invariants: {OWNER} must inject the invariant service"),
            _ => format!("test invariants: {OWNER} settled without becoming active"),
        };
        assert_eq!(error.to_string(), expected);
        root.fiber().restart().await.unwrap();
    }
}

#[tokio::test]
async fn attachment_companion_joins_its_supplied_store_and_foreign_roots_are_rejected() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let attachment = Plugin::new(
        "test-attachments",
        std::iter::empty::<String>(),
        |context, _| {
            Box::pin(async move {
                context.provide_named("attachments", Arc::new(true))?;
                Ok(())
            })
        },
    );
    let companion =
        probe("attachment-companion", &calls).with_additional_inject(["invariants", "attachments"]);
    let loaders = companions([(
        "../packages/attachment/attachment-local/src/invariant.ts",
        companion,
    )]);
    let host = TestInvariantHost::new(
        root.clone(),
        "/repo/packages/attachment/attachment-local/tests/local.spec.ts",
        loaders,
        Some(attachment),
        spawn,
    );
    host.plugin(&root, probe("target", &Arc::default()), Value::Null)
        .unwrap()
        .await_ready()
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        host.plugin(
            &Context::new(),
            probe("foreign", &Arc::default()),
            Value::Null
        )
        .is_err()
    );
    root.fiber().restart().await.unwrap();
}
