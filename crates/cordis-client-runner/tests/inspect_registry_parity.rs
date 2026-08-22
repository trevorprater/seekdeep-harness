//! Client inspect registration, publication, query, cancellation, and routing parity.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis_client_runner::*;
use seekdeep_cordis_dynamic_types::*;
use seekdeep_identity::SessionId;
use serde_json::{Value, json};

#[derive(Debug)]
struct TokioSpawner;

impl ClientTaskSpawner for TokioSpawner {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        tokio::spawn(future);
    }
}

#[derive(Default)]
struct ManualMicrotasks {
    queued: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl ClientMicrotaskScheduler for ManualMicrotasks {
    fn queue(&self, callback: Box<dyn FnOnce() + Send>) {
        self.queued.lock().push(callback);
    }
}

impl ManualMicrotasks {
    fn run(&self) {
        for callback in std::mem::take(&mut *self.queued.lock()) {
            callback();
        }
    }
}

#[derive(Default)]
struct FakeHost {
    syncs: Mutex<Vec<Vec<CordisInspectProviderManifest>>>,
    resolutions: Mutex<
        Vec<(
            SessionId,
            CordisInspectRequestId,
            CordisInspectQueryResolution,
        )>,
    >,
}

impl ClientCordisInspectHost for FakeHost {
    fn sync(
        &self,
        providers: Vec<CordisInspectProviderManifest>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.syncs.lock().push(providers);
        Box::pin(async { Ok(()) })
    }

    fn resolve(
        &self,
        session_id: SessionId,
        request_id: CordisInspectRequestId,
        resolution: CordisInspectQueryResolution,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.resolutions
            .lock()
            .push((session_id, request_id, resolution));
        Box::pin(async { Ok(()) })
    }
}

fn manifest(id: &str, methods: &[&str]) -> CordisInspectProviderManifest {
    CordisInspectProviderManifest {
        id: id.to_owned(),
        description: format!("inspect {id}"),
        methods: methods
            .iter()
            .map(|name| CordisInspectMethodManifest {
                name: (*name).to_owned(),
                description: format!("query {name}"),
                input_schema: json!({}),
                output_schema: json!({}),
            })
            .collect(),
    }
}

fn registration(id: &str, methods: &[&str]) -> ClientCordisInspectProviderRegistration {
    ClientCordisInspectProviderRegistration {
        manifest: manifest(id, methods),
        query: Arc::new(|method, input, context| {
            Box::pin(async move {
                Ok(json!({
                    "method": method,
                    "input": input,
                    "session": context.session_id,
                }))
            })
        }),
    }
}

fn registry() -> (
    Arc<ClientCordisInspectRegistry>,
    Arc<FakeHost>,
    Arc<ManualMicrotasks>,
) {
    let host = Arc::new(FakeHost::default());
    let microtasks = Arc::new(ManualMicrotasks::default());
    let registry =
        ClientCordisInspectRegistry::new(host.clone(), microtasks.clone(), Arc::new(TokioSpawner));
    (registry, host, microtasks)
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
async fn registration_validates_coalesces_complete_manifests_and_disposes_idempotently() {
    let (registry, host, microtasks) = registry();
    let first = registry.register(registration("alpha", &["one"])).unwrap();
    registry.register(registration("beta", &["two"])).unwrap();
    assert_eq!(microtasks.queued.lock().len(), 1);
    assert_eq!(
        registry
            .manifests()
            .iter()
            .map(|manifest| manifest.id.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    assert!(registry.register(registration("alpha", &["x"])).is_err());
    assert!(registry.register(registration(" ", &["x"])).is_err());
    assert!(
        registry
            .register(registration("dupe", &["x", "x"]))
            .is_err()
    );

    microtasks.run();
    eventually(|| host.syncs.lock().len() == 1, "manifest did not publish").await;
    assert_eq!(host.syncs.lock()[0].len(), 2);
    first.dispose();
    first.dispose();
    microtasks.run();
    eventually(|| host.syncs.lock().len() == 2, "disposal did not publish").await;
    assert_eq!(host.syncs.lock()[1][0].id, "beta");
}

fn request(id: &str, provider: &str, method: &str) -> CordisInspectQueryRequest {
    CordisInspectQueryRequest {
        request_id: CordisInspectRequestId::new(id),
        agent_id: SessionId::new("session-a"),
        provider: provider.to_owned(),
        method: method.to_owned(),
        input: Some(json!({"value": 1})),
    }
}

#[tokio::test]
async fn query_routes_session_method_input_and_structures_missing_and_provider_failures() {
    let (registry, host, _) = registry();
    registry.register(registration("alpha", &["read"])).unwrap();
    registry.query(request("inspect-1", "alpha", "read")).await;
    registry
        .query(request("inspect-2", "missing", "read"))
        .await;
    registry
        .query(request("inspect-3", "alpha", "missing"))
        .await;
    let failing = ClientCordisInspectProviderRegistration {
        manifest: manifest("broken", &["read"]),
        query: Arc::new(|_, _, _| Box::pin(async { anyhow::bail!("provider broke") })),
    };
    registry.register(failing).unwrap();
    registry.query(request("inspect-4", "broken", "read")).await;
    let resolutions = host.resolutions.lock();
    assert_eq!(resolutions[0].0, SessionId::new("session-a"));
    assert!(matches!(
        &resolutions[0].2,
        CordisInspectQueryResolution::Success { data }
            if data["method"] == "read" && data["session"] == "session-a"
    ));
    assert!(matches!(
        resolutions[1].2,
        CordisInspectQueryResolution::Failure {
            reason: CordisInspectFailureReason::ProviderMissing,
            ..
        }
    ));
    assert!(matches!(
        resolutions[2].2,
        CordisInspectQueryResolution::Failure {
            reason: CordisInspectFailureReason::MethodMissing,
            ..
        }
    ));
    assert!(matches!(
        resolutions[3].2,
        CordisInspectQueryResolution::Failure {
            reason: CordisInspectFailureReason::ProviderError,
            ..
        }
    ));
}

#[tokio::test]
async fn duplicate_query_is_ignored_and_close_cancels_without_answering() {
    let (registry, host, _) = registry();
    let gate = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let query_gate = gate.clone();
    let query_calls = calls.clone();
    registry
        .register(ClientCordisInspectProviderRegistration {
            manifest: manifest("slow", &["read"]),
            query: Arc::new(move |_, _, context| {
                let gate = query_gate.clone();
                let calls = query_calls.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::AcqRel);
                    gate.notified().await;
                    if context.signal.is_aborted() {
                        anyhow::bail!("cancelled")
                    }
                    Ok(Value::Null)
                })
            }),
        })
        .unwrap();
    let query = request("inspect-1", "slow", "read");
    let first_registry = registry.clone();
    let first_query = query.clone();
    let first = tokio::spawn(async move { first_registry.query(first_query).await });
    let duplicate_registry = registry.clone();
    let duplicate_query = query.clone();
    let duplicate = tokio::spawn(async move { duplicate_registry.query(duplicate_query).await });
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::Acquire), 1);
    registry.close(&query.request_id);
    gate.notify_one();
    first.await.unwrap();
    duplicate.await.unwrap();
    assert!(host.resolutions.lock().is_empty());
}
