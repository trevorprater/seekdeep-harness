//! Real durable-registry-to-Workspace-API and Host-event composition.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt as _;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{SessionEvent, SessionHeader, SessionId},
    session_store::SessionStore,
};
use seekdeep_host_apiproxy::{
    WorkspaceRegistryRuntime, WorkspaceRuntime, WorkspaceRuntimeError,
    api::{events::HostFrame, workspace::WorkspaceId},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceSnapshot,
};
use seekdeep_storage::{Storage, StorageBackend};
use seekdeep_storage_domain::{DomainConfig, DomainFacility};
use seekdeep_storage_json::JsonStorageBackend;
use seekdeep_workspace::WorkspaceRegistry;
use tempfile::TempDir;

#[derive(Debug)]
struct Headers(Vec<SessionHeader>);

#[async_trait]
impl SessionPersistence for Headers {
    fn locate(&self, _meta: &SessionHeader) -> Option<SessionLocation> {
        None
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    async fn create(&self, _meta: &SessionHeader) -> anyhow::Result<()> {
        anyhow::bail!("not used")
    }

    async fn append(&self, _id: &SessionId, _events: &[SessionEvent]) -> anyhow::Result<()> {
        anyhow::bail!("not used")
    }

    async fn load(&self, _id: &SessionId) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("not used")
    }

    async fn inspect(
        &self,
        _id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("not used")
    }

    async fn read_from(
        &self,
        _id: &SessionId,
        _from_seq: u64,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("not used")
    }

    async fn list(&self, _signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        Ok(self.0.clone())
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        anyhow::bail!("not used")
    }
}

struct Harness {
    runtime: Arc<WorkspaceRegistryRuntime>,
    registry: Arc<WorkspaceRegistry>,
    _root: TempDir,
    _storage: Arc<Storage>,
    _storage_effect: seekdeep_cordis::fiber::EffectHandle,
    _backend: Arc<JsonStorageBackend>,
    _backend_registration: seekdeep_storage::BackendRegistration,
    _facility: Arc<DomainFacility>,
    _facility_effect: seekdeep_cordis::fiber::EffectHandle,
    _mount: seekdeep_storage::FormMount,
}

async fn harness(headers: Vec<SessionHeader>) -> Harness {
    let root = TempDir::new().unwrap();
    let context = Context::new();
    let storage = Storage::new();
    let storage_effect = storage.provide(&context).unwrap();
    let backend = JsonStorageBackend::new(root.path());
    let backend_registration = storage
        .backend
        .register("json", backend.clone() as Arc<dyn StorageBackend>)
        .unwrap();
    let facility = DomainFacility::new(
        context.clone(),
        storage.clone(),
        DomainConfig {
            backend: "json".to_owned(),
            routes: HashMap::default(),
        },
    );
    let (facility_effect, mount) = facility.mount(&context).unwrap();
    let registry = WorkspaceRegistry::open(
        context,
        &facility,
        Arc::new(Headers(headers)),
        None::<Arc<SessionStore>>,
    )
    .await
    .unwrap();
    let runtime = WorkspaceRegistryRuntime::new(registry.clone(), facility.clone());
    Harness {
        runtime,
        registry,
        _root: root,
        _storage: storage,
        _storage_effect: storage_effect,
        _backend: backend,
        _backend_registration: backend_registration,
        _facility: facility,
        _facility_effect: facility_effect,
        _mount: mount,
    }
}

async fn next_frame(
    stream: &mut (impl futures::Stream<Item = anyhow::Result<HostFrame>> + Unpin),
) -> HostFrame {
    tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("Host event timeout")
        .expect("Host stream ended")
        .expect("Host stream error")
}

#[tokio::test]
async fn real_registry_runtime_preserves_api_business_semantics() {
    let harness = harness(vec![]).await;
    let directories = TempDir::new().unwrap();
    let first = directories.path().join("first");
    let second = directories.path().join("second");
    tokio::fs::create_dir(&first).await.unwrap();
    tokio::fs::create_dir(&second).await.unwrap();
    let (first_view, created) = harness
        .runtime
        .create(first.to_string_lossy().into_owned())
        .await
        .unwrap();
    assert!(created);
    let (same, created) = harness
        .runtime
        .create(first.to_string_lossy().into_owned())
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(same.workspace_id, first_view.workspace_id);
    let (second_view, created) = harness
        .runtime
        .create(second.to_string_lossy().into_owned())
        .await
        .unwrap();
    assert!(created);
    let renamed = harness
        .runtime
        .rename(first_view.workspace_id.clone(), "shared".to_owned())
        .await
        .unwrap();
    assert_eq!(renamed.title, "shared");
    let conflict = harness
        .runtime
        .rename(second_view.workspace_id.clone(), "shared".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(conflict, WorkspaceRuntimeError::NameConflict(name) if name == "shared"));
    let missing = WorkspaceId::new("missing");
    assert!(matches!(
        harness.runtime.delete(missing.clone()).await.unwrap_err(),
        WorkspaceRuntimeError::NotFound(id) if id == missing
    ));
    let order = harness
        .runtime
        .insert_before(
            first_view.workspace_id.clone(),
            Some(second_view.workspace_id.clone()),
        )
        .await
        .unwrap();
    assert_eq!(
        order,
        vec![first_view.workspace_id, second_view.workspace_id]
    );
    harness.registry.close().await.unwrap();
}

#[tokio::test]
async fn real_domain_changes_project_exact_committed_host_frames() {
    let mut known = SessionHeader::new(SessionId::new("known"));
    known.created_at = 1;
    let harness = harness(vec![known]).await;
    let directories = TempDir::new().unwrap();
    let first = directories.path().join("first");
    let second = directories.path().join("second");
    tokio::fs::create_dir(&first).await.unwrap();
    tokio::fs::create_dir(&second).await.unwrap();
    let signal = AbortSignal::default();
    let mut events = harness.runtime.host_events(signal.clone());

    let (first_view, _) = harness
        .runtime
        .create(first.to_string_lossy().into_owned())
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut events).await,
        HostFrame::WorkspaceChanged { workspace } if workspace.workspace_id == first_view.workspace_id
    ));
    let (second_view, _) = harness
        .runtime
        .create(second.to_string_lossy().into_owned())
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut events).await,
        HostFrame::WorkspaceChanged { workspace } if workspace.workspace_id == second_view.workspace_id
    ));
    harness
        .runtime
        .rename(first_view.workspace_id.clone(), "renamed".to_owned())
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut events).await,
        HostFrame::WorkspaceChanged { workspace }
            if workspace.workspace_id == first_view.workspace_id && workspace.title == "renamed"
    ));
    harness
        .runtime
        .insert_before(
            first_view.workspace_id.clone(),
            Some(second_view.workspace_id.clone()),
        )
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut events).await,
        HostFrame::WorkspaceOrderChanged { workspace_ids }
            if workspace_ids == vec![first_view.workspace_id.clone(), second_view.workspace_id.clone()]
    ));
    harness
        .runtime
        .archive_session(SessionId::new("known"))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut events).await,
        HostFrame::ArchivedSessionsChanged { archived_session_ids }
            if archived_session_ids == vec![SessionId::new("known")]
    ));
    harness
        .runtime
        .delete(first_view.workspace_id.clone())
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut events).await,
        HostFrame::WorkspaceRemoved { workspace_id } if workspace_id == first_view.workspace_id
    ));
    signal.abort();
    assert!(events.next().await.is_none());
    harness.registry.close().await.unwrap();
}
