//! Exact-run Client load, queue, retract, snapshot, crash, and disposal parity.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis_client_runner::*;
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    DynamicCordisRenderFailure,
};
use seekdeep_identity::SessionId;

#[derive(Debug)]
struct TokioSpawner;

impl ClientTaskSpawner for TokioSpawner {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        tokio::spawn(future);
    }
}

struct FakeEngine {
    results: Mutex<VecDeque<Result<MountedClientPackage, ClientMountError>>>,
    gates: Mutex<VecDeque<Option<Arc<tokio::sync::Notify>>>>,
    mounts: Arc<Mutex<Vec<CordisDynamicPluginRunId>>>,
    teardowns: Arc<Mutex<Vec<(CordisDynamicPluginId, CordisDynamicPluginRunId)>>>,
    unwatched: AtomicBool,
}

impl ClientMountEngine for FakeEngine {
    fn mount(
        &self,
        request: ClientLoadRequest,
    ) -> BoxFuture<'static, Result<MountedClientPackage, ClientMountError>> {
        self.mounts.lock().push(request.plugin_run_id);
        let result = self.results.lock().pop_front().unwrap_or_else(|| {
            Ok(MountedClientPackage {
                waiting_for: Vec::new(),
                slots: Vec::new(),
                style_count: 0,
            })
        });
        let gate = self.gates.lock().pop_front().flatten();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            result
        })
    }

    fn teardown(
        &self,
        plugin_id: CordisDynamicPluginId,
        plugin_run_id: CordisDynamicPluginRunId,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.teardowns.lock().push((plugin_id, plugin_run_id));
        Box::pin(async { Ok(()) })
    }

    fn unwatch(&self) {
        self.unwatched.store(true, Ordering::Release);
    }
}

struct Harness {
    runtime: Arc<DynamicCordisClientRuntime>,
    engine: Arc<FakeEngine>,
    reports: Arc<Mutex<Vec<DynamicCordisRenderFailure>>>,
}

fn plugin() -> CordisDynamicPluginId {
    CordisDynamicPluginId::new("panel-1")
}

fn run(number: u64) -> CordisDynamicPluginRunId {
    CordisDynamicPluginRunId::new(format!("run-{number}"))
}

fn request(number: u64) -> ClientLoadRequest {
    ClientLoadRequest {
        plugin_id: plugin(),
        package_id: CordisDynamicPackageId::new(format!("pkg-{number}")),
        plugin_run_id: run(number),
        agent_id: SessionId::new("session-a"),
        name: format!("panel {number}"),
        code: "return { apply() {} }".to_owned(),
    }
}

fn harness(
    results: Vec<Result<MountedClientPackage, ClientMountError>>,
    gates: Vec<Option<Arc<tokio::sync::Notify>>>,
) -> Harness {
    let engine = Arc::new(FakeEngine {
        results: Mutex::new(results.into()),
        gates: Mutex::new(gates.into()),
        mounts: Arc::new(Mutex::new(Vec::new())),
        teardowns: Arc::new(Mutex::new(Vec::new())),
        unwatched: AtomicBool::new(false),
    });
    let reports = Arc::new(Mutex::new(Vec::new()));
    let observed = reports.clone();
    let runtime = DynamicCordisClientRuntime::new(
        engine.clone(),
        Arc::new(TokioSpawner),
        Arc::new(move |_, _, _, failure| observed.lock().push(failure)),
    );
    Harness {
        runtime,
        engine,
        reports,
    }
}

#[tokio::test]
async fn load_projects_unique_contributions_replays_exact_run_and_replaces_newer_run() {
    let harness = harness(
        vec![
            Ok(MountedClientPackage {
                waiting_for: vec!["slots".to_owned()],
                slots: vec!["main".to_owned(), "main".to_owned(), "aside".to_owned()],
                style_count: 2,
            }),
            Ok(MountedClientPackage {
                waiting_for: Vec::new(),
                slots: vec!["next".to_owned()],
                style_count: 1,
            }),
        ],
        Vec::new(),
    );
    let first = harness.runtime.load(request(1)).await.unwrap();
    assert!(matches!(
        first,
        ClientLoadResult::Success { ref waiting_for, .. }
            if waiting_for.as_deref() == Some(&["slots".to_owned()][..])
    ));
    let snapshot = harness.runtime.snapshot();
    assert_eq!(snapshot[0].slots, ["aside", "main"]);
    assert_eq!(snapshot[0].style_count, 2);
    assert!(Arc::ptr_eq(&snapshot, &harness.runtime.snapshot()));

    harness.runtime.load(request(1)).await.unwrap();
    assert_eq!(harness.engine.mounts.lock().len(), 1);
    harness.runtime.load(request(2)).await.unwrap();
    assert_eq!(harness.engine.mounts.lock().as_slice(), &[run(1), run(2)]);
    assert_eq!(
        harness.engine.teardowns.lock().as_slice(),
        &[(plugin(), run(1))]
    );
    assert_eq!(harness.runtime.snapshot()[0].plugin_run_id, run(2));
}

#[tokio::test]
async fn same_plugin_operations_serialize_and_a_failed_mount_does_not_wedge_the_queue() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let harness = harness(
        vec![
            Err(ClientMountError::Classified(ClientMountFailure {
                cause: ClientLoadErrorCause::Evaluate,
                message: "broken".to_owned(),
                stack: None,
            })),
            Ok(MountedClientPackage {
                waiting_for: Vec::new(),
                slots: Vec::new(),
                style_count: 0,
            }),
        ],
        vec![Some(gate.clone()), None],
    );
    let first = tokio::spawn(harness.runtime.load(request(1)));
    let second = tokio::spawn(harness.runtime.load(request(2)));
    tokio::task::yield_now().await;
    assert_eq!(harness.engine.mounts.lock().as_slice(), &[run(1)]);
    gate.notify_one();
    assert!(matches!(
        first.await.unwrap().unwrap(),
        ClientLoadResult::Failure { .. }
    ));
    assert!(matches!(
        second.await.unwrap().unwrap(),
        ClientLoadResult::Success { .. }
    ));
    assert_eq!(harness.engine.mounts.lock().as_slice(), &[run(1), run(2)]);
    assert_eq!(harness.runtime.snapshot()[0].plugin_run_id, run(2));
}

#[tokio::test]
async fn infrastructure_rejection_rejects_the_load_and_does_not_wedge_the_queue() {
    let harness = harness(
        vec![
            Err(ClientMountError::Rejected(ClientMountRejection {
                message: "cordis-client-runner: window.__ModuleLoader__ is missing (booted outside the web shell?)"
                    .to_owned(),
                stack: Some("stack".to_owned()),
            })),
            Ok(MountedClientPackage {
                waiting_for: Vec::new(),
                slots: Vec::new(),
                style_count: 0,
            }),
        ],
        Vec::new(),
    );

    let rejected = harness.runtime.load(request(1)).await.unwrap_err();
    assert!(rejected.message.contains("__ModuleLoader__ is missing"));
    assert_eq!(rejected.stack.as_deref(), Some("stack"));
    assert!(matches!(
        harness.runtime.load(request(2)).await.unwrap(),
        ClientLoadResult::Success { .. }
    ));
    assert_eq!(harness.engine.mounts.lock().as_slice(), &[run(1), run(2)]);
}

async fn eventually(mut condition: impl FnMut() -> bool, message: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

#[tokio::test]
async fn stale_retract_is_ignored_exact_retract_clears_live_state_and_dispose_unwatches() {
    let harness = harness(Vec::new(), Vec::new());
    harness.runtime.load(request(2)).await.unwrap();
    harness.runtime.retract(plugin(), run(1));
    tokio::task::yield_now().await;
    assert!(harness.runtime.is_loaded(&plugin()));
    harness.runtime.retract(plugin(), run(2));
    eventually(
        || !harness.runtime.is_loaded(&plugin()),
        "exact retraction did not converge",
    )
    .await;
    assert_eq!(
        harness.engine.teardowns.lock().as_slice(),
        &[(plugin(), run(2))]
    );

    harness.runtime.load(request(3)).await.unwrap();
    harness.runtime.dispose().await;
    assert!(!harness.runtime.is_loaded(&plugin()));
    assert!(harness.engine.unwatched.load(Ordering::Acquire));
    assert_eq!(
        harness.engine.teardowns.lock().last(),
        Some(&(plugin(), run(3)))
    );
}

#[tokio::test]
async fn retract_arriving_during_a_slow_load_runs_after_that_exact_activation_seats() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let harness = harness(Vec::new(), vec![Some(gate.clone())]);
    let load = tokio::spawn(harness.runtime.load(request(1)));
    tokio::task::yield_now().await;
    harness.runtime.retract(plugin(), run(1));
    gate.notify_one();
    assert!(matches!(
        load.await.unwrap().unwrap(),
        ClientLoadResult::Success { .. }
    ));
    eventually(
        || !harness.runtime.is_loaded(&plugin()),
        "queued retraction did not remove the slow load",
    )
    .await;
    assert_eq!(
        harness.engine.teardowns.lock().as_slice(),
        &[(plugin(), run(1))]
    );
}

#[tokio::test]
async fn render_failures_are_exact_run_last_writer_wins_and_redirect_once() {
    let harness = harness(Vec::new(), Vec::new());
    harness.runtime.load(request(1)).await.unwrap();
    let notices = Arc::new(AtomicUsize::new(0));
    let observed = notices.clone();
    let _subscription = harness.runtime.subscribe(Arc::new(move || {
        observed.fetch_add(1, Ordering::AcqRel);
    }));
    let crash = DynamicCordisRenderFailure {
        slot: "shell.overlay".to_owned(),
        message: "setInterval is not defined".to_owned(),
        stack: Some("stack".to_owned()),
        abdicated: true,
    };
    harness
        .runtime
        .report_render_failure(&SessionId::new("wrong"), &plugin(), &run(1), &crash);
    assert!(harness.runtime.render_failures().is_empty());
    harness
        .runtime
        .report_render_failure(&SessionId::new("session-a"), &plugin(), &run(1), &crash);
    let first = harness.runtime.render_failures();
    assert!(first[&plugin()].message.contains("browser timer globals"));
    assert_eq!(harness.reports.lock().len(), 1);
    harness.runtime.load(request(1)).await.unwrap();
    assert_eq!(harness.runtime.render_failures().len(), 1);

    let already_redirected = DynamicCordisRenderFailure {
        slot: "shell.overlay".to_owned(),
        message: first[&plugin()].message.clone(),
        stack: None,
        abdicated: false,
    };
    harness.runtime.report_render_failure(
        &SessionId::new("session-a"),
        &plugin(),
        &run(1),
        &already_redirected,
    );
    let latest = harness.runtime.render_failures();
    assert_eq!(
        latest[&plugin()]
            .message
            .matches("browser timer globals")
            .count(),
        1
    );
    assert_eq!(notices.load(Ordering::Acquire), 2);

    harness.runtime.load(request(2)).await.unwrap();
    assert!(harness.runtime.render_failures().is_empty());
}
