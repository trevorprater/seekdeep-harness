//! Executable parity cases ported from the pinned Workspace source suite.

mod support;

use std::{collections::HashMap, sync::Arc, time::Duration};

use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, FiberState};
use seekdeep_core::session::SessionId;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_session_persistence::SessionPersistenceService;
use seekdeep_storage::{BackendRegistration, FormMount, Storage, StorageBackend};
use seekdeep_storage_domain::{DomainChanged, DomainConfig, DomainFacility};
use seekdeep_workspace::{
    PendingMutation, PendingOperation, WorkspaceAggregateError, WorkspaceDomainState, WorkspaceId,
    WorkspaceMoveInvalidError, WorkspaceOrderInvalidError, WorkspaceRecord, WorkspaceRegistry,
    WorkspaceStatus,
};
use serde_json::json;
use tempfile::TempDir;

use support::{FailAt, Headers, MemoryBackend, Pool, header, stored_workspace};

struct Harness {
    context: Context,
    registry: Arc<WorkspaceRegistry>,
    pool: Arc<Pool>,
    headers: Arc<Headers>,
    _storage: Arc<Storage>,
    _storage_effect: seekdeep_cordis::fiber::EffectHandle,
    _registration: BackendRegistration,
    _facility: Arc<DomainFacility>,
    _facility_effect: seekdeep_cordis::fiber::EffectHandle,
    _mount: FormMount,
    _registry_effect: seekdeep_cordis::fiber::EffectHandle,
}

async fn boot(
    pool: Arc<Pool>,
    backend: Arc<dyn StorageBackend>,
    headers: Arc<Headers>,
) -> anyhow::Result<Harness> {
    let context = Context::new();
    let storage = Storage::new();
    let storage_effect = storage.provide(&context)?;
    let registration = storage.backend.register("memory", backend)?;
    let facility = DomainFacility::new(
        context.clone(),
        storage.clone(),
        DomainConfig {
            backend: "memory".to_owned(),
            routes: HashMap::default(),
        },
    );
    let (facility_effect, mount) = facility.mount(&context)?;
    let registry =
        WorkspaceRegistry::open(context.clone(), &facility, headers.clone(), None).await?;
    let registry_effect = registry.provide(&context)?;
    Ok(Harness {
        context,
        registry,
        pool,
        headers,
        _storage: storage,
        _storage_effect: storage_effect,
        _registration: registration,
        _facility: facility,
        _facility_effect: facility_effect,
        _mount: mount,
        _registry_effect: registry_effect,
    })
}

async fn fresh(headers: Arc<Headers>) -> anyhow::Result<Harness> {
    let pool = Arc::new(Pool::default());
    boot(pool.clone(), MemoryBackend::new(pool), headers).await
}

fn ids(workspaces: &[Arc<seekdeep_workspace::Workspace>]) -> Vec<String> {
    workspaces
        .iter()
        .map(|workspace| workspace.id().to_string())
        .collect()
}

fn record(path: &str, session_ids: &[&str], created_at: &str) -> WorkspaceRecord {
    WorkspaceRecord {
        path: path.to_owned(),
        title: std::path::Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        session_ids: session_ids.iter().map(|id| SessionId::new(*id)).collect(),
        created_at: created_at.to_owned(),
        updated_at: created_at.to_owned(),
    }
}

#[tokio::test]
async fn bootstraps_once_by_workspace_and_session_created_at_order() {
    let temp = TempDir::new().unwrap();
    let alpha = temp.path().join("alpha");
    let beta = temp.path().join("beta");
    tokio::fs::create_dir_all(&alpha).await.unwrap();
    tokio::fs::create_dir_all(&beta).await.unwrap();
    let headers = Arc::new(Headers::default());
    *headers.values.lock() = vec![
        header("a-old", alpha.to_str(), 10),
        header("b", beta.to_str(), 30),
        header("a-new", alpha.to_str(), 20),
        header("no-cwd", None, 40),
    ];
    let harness = fresh(headers.clone()).await.unwrap();
    let listed = harness.registry.list().unwrap();
    let alpha = tokio::fs::canonicalize(alpha).await.unwrap();
    let beta = tokio::fs::canonicalize(beta).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].path(), beta.to_string_lossy());
    assert_eq!(listed[1].path(), alpha.to_string_lossy());
    assert_eq!(
        listed[1].session_ids(),
        vec![SessionId::new("a-new"), SessionId::new("a-old")]
    );
    assert_eq!(
        headers
            .list_calls
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );

    harness.registry.close().await.unwrap();
    headers.values.lock().clear();
    let restarted = boot(
        harness.pool.clone(),
        MemoryBackend::new(harness.pool.clone()),
        headers.clone(),
    )
    .await
    .unwrap();
    assert_eq!(ids(&restarted.registry.list().unwrap()), ids(&listed));
    assert_eq!(
        headers
            .list_calls
            .load(std::sync::atomic::Ordering::Acquire),
        2
    );
}

#[tokio::test]
async fn initialized_empty_registry_does_not_rerun_bootstrap() {
    let pool = Arc::new(Pool::default());
    stored_workspace(
        &pool,
        vec![],
        json!({ "initialized": true, "workspaceIds": [] }),
    );
    let headers = Arc::new(Headers::default());
    headers.values.lock().push(header("ignored", Some("/"), 1));
    let harness = boot(pool.clone(), MemoryBackend::new(pool), headers.clone())
        .await
        .unwrap();
    assert!(harness.registry.list().unwrap().is_empty());
    assert_eq!(
        headers
            .list_calls
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
}

#[tokio::test]
async fn creates_newest_first_reuses_canonical_path_and_allows_duplicate_titles() {
    let temp = TempDir::new().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    tokio::fs::create_dir_all(&first).await.unwrap();
    tokio::fs::create_dir_all(&second).await.unwrap();
    #[cfg(unix)]
    let alias = {
        let alias = temp.path().join("alias");
        tokio::fs::symlink(&first, &alias).await.unwrap();
        alias
    };
    let harness = fresh(Arc::new(Headers::default())).await.unwrap();
    let left = harness.registry.create(
        first.to_string_lossy().into_owned(),
        Some("same".to_owned()),
    );
    let right = harness.registry.create(
        first.join(".").to_string_lossy().into_owned(),
        Some("lost".to_owned()),
    );
    let (left, right) = tokio::join!(left, right);
    let left = left.unwrap();
    assert!(Arc::ptr_eq(&left, &right.unwrap()));
    assert_eq!(left.title(), "same");
    #[cfg(unix)]
    assert!(Arc::ptr_eq(
        &left,
        &harness
            .registry
            .create(alias.to_string_lossy().into_owned(), None)
            .await
            .unwrap()
    ));
    let other = harness
        .registry
        .create(
            second.to_string_lossy().into_owned(),
            Some("same".to_owned()),
        )
        .await
        .unwrap();
    assert_eq!(
        ids(&harness.registry.list().unwrap()),
        vec![other.id().to_string(), left.id().to_string()]
    );
    assert_eq!(
        harness
            .registry
            .resolve_by_path(first.to_str().unwrap())
            .await
            .unwrap()
            .unwrap()
            .id(),
        left.id()
    );
}

#[tokio::test]
async fn rejects_missing_and_non_directory_paths_without_mutation() {
    let temp = TempDir::new().unwrap();
    let file = temp.path().join("file");
    tokio::fs::write(&file, b"x").await.unwrap();
    let harness = fresh(Arc::new(Headers::default())).await.unwrap();
    assert!(
        harness
            .registry
            .create(
                temp.path().join("missing").to_string_lossy().into_owned(),
                None
            )
            .await
            .is_err()
    );
    assert!(
        harness
            .registry
            .create(file.to_string_lossy().into_owned(), None)
            .await
            .unwrap_err()
            .to_string()
            .contains("path is not a directory")
    );
    assert!(harness.registry.list().unwrap().is_empty());
}

#[tokio::test]
async fn workspace_order_is_dom_insert_before_and_durable() {
    let temp = TempDir::new().unwrap();
    let harness = fresh(Arc::new(Headers::default())).await.unwrap();
    let mut made = Vec::new();
    for name in ["a", "b", "c"] {
        let path = temp.path().join(name);
        tokio::fs::create_dir(&path).await.unwrap();
        made.push(
            harness
                .registry
                .create(path.to_string_lossy().into_owned(), None)
                .await
                .unwrap(),
        );
    }
    let order = harness
        .registry
        .insert_before(made[0].id().clone(), Some(made[2].id().clone()))
        .await
        .unwrap();
    assert_eq!(
        order,
        vec![
            made[0].id().clone(),
            made[2].id().clone(),
            made[1].id().clone()
        ]
    );
    assert_eq!(
        harness
            .registry
            .insert_before(made[0].id().clone(), Some(made[0].id().clone()))
            .await
            .unwrap(),
        order
    );
    let unknown = WorkspaceId::new("unknown");
    assert_eq!(
        harness
            .registry
            .insert_before(unknown.clone(), None)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceOrderInvalidError>()
            .unwrap()
            .workspace_id,
        unknown
    );
}

#[tokio::test]
async fn attach_move_detach_filter_and_status_match_entity_contract() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("owned");
    let other = temp.path().join("other");
    tokio::fs::create_dir(&dir).await.unwrap();
    tokio::fs::create_dir(&other).await.unwrap();
    let headers = Arc::new(Headers::default());
    *headers.values.lock() = vec![
        header("one", dir.to_str(), 1),
        header("two", dir.to_str(), 2),
        header("wrong", other.to_str(), 3),
        header("none", None, 4),
        header("missing", Some("/definitely/missing"), 5),
    ];
    let harness = fresh(headers).await.unwrap();
    let workspace = harness
        .registry
        .resolve_by_path(dir.to_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    workspace
        .attach_session(SessionId::new("one"))
        .await
        .unwrap();
    workspace
        .attach_session(SessionId::new("two"))
        .await
        .unwrap();
    assert_eq!(
        workspace.session_ids(),
        vec![SessionId::new("two"), SessionId::new("one")]
    );
    workspace
        .insert_session_before(SessionId::new("one"), Some(SessionId::new("two")))
        .await
        .unwrap();
    assert_eq!(
        workspace.session_ids(),
        vec![SessionId::new("one"), SessionId::new("two")]
    );
    workspace
        .insert_session_before(SessionId::new("one"), None)
        .await
        .unwrap();
    assert_eq!(
        workspace.session_ids(),
        vec![SessionId::new("two"), SessionId::new("one")]
    );
    workspace
        .detach_session(SessionId::new("two"))
        .await
        .unwrap();
    workspace
        .detach_session(SessionId::new("two"))
        .await
        .unwrap();
    assert_eq!(workspace.session_ids(), vec![SessionId::new("one")]);
    let move_error = workspace
        .insert_session_before(SessionId::new("ghost"), None)
        .await
        .unwrap_err();
    assert!(
        move_error
            .downcast_ref::<WorkspaceMoveInvalidError>()
            .is_some()
    );
    for id in ["wrong", "none", "missing", "ghost"] {
        assert!(
            workspace.attach_session(SessionId::new(id)).await.is_err(),
            "{id}"
        );
    }
    assert_eq!(workspace.status().await, WorkspaceStatus::Ok);
    tokio::fs::remove_dir(&dir).await.unwrap();
    assert_eq!(workspace.status().await, WorkspaceStatus::MissingDir);
}

#[tokio::test]
async fn archive_is_ordered_idempotent_durable_and_propagates_listing_failure() {
    let headers = Arc::new(Headers::default());
    headers
        .values
        .lock()
        .extend([header("a", None, 1), header("b", None, 2)]);
    let harness = fresh(headers.clone()).await.unwrap();
    harness
        .registry
        .archive_session(SessionId::new("b"))
        .await
        .unwrap();
    harness
        .registry
        .archive_session(SessionId::new("a"))
        .await
        .unwrap();
    harness
        .registry
        .archive_session(SessionId::new("b"))
        .await
        .unwrap();
    assert_eq!(
        harness.registry.archived_session_ids(),
        vec![SessionId::new("b"), SessionId::new("a")]
    );
    *headers.fail_list.lock() = Some("listing exploded".to_owned());
    let error = harness
        .registry
        .archive_session(SessionId::new("ghost"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("listing exploded"));
}

#[tokio::test]
async fn delete_retains_directory_and_recovers_order_across_restart() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("owned");
    tokio::fs::create_dir(&dir).await.unwrap();
    let harness = fresh(Arc::new(Headers::default())).await.unwrap();
    let workspace = harness
        .registry
        .create(dir.to_string_lossy().into_owned(), None)
        .await
        .unwrap();
    assert!(
        harness
            .registry
            .delete(workspace.id().clone())
            .await
            .unwrap()
    );
    assert!(
        !harness
            .registry
            .delete(workspace.id().clone())
            .await
            .unwrap()
    );
    assert!(tokio::fs::metadata(&dir).await.unwrap().is_dir());
    assert!(harness.registry.list().unwrap().is_empty());
    harness.registry.close().await.unwrap();
    let restarted = boot(
        harness.pool.clone(),
        MemoryBackend::new(harness.pool.clone()),
        harness.headers.clone(),
    )
    .await
    .unwrap();
    assert!(restarted.registry.list().unwrap().is_empty());
}

#[tokio::test]
async fn recovers_only_explicit_pending_markers_and_rejects_unexplained_drift() {
    let pool = Arc::new(Pool::default());
    stored_workspace(
        &pool,
        vec![(
            "pending".to_owned(),
            serde_json::to_value(record("/tmp/pending", &[], "2026-01-01T00:00:00.000Z")).unwrap(),
        )],
        serde_json::to_value(WorkspaceDomainState {
            initialized: true,
            workspace_ids: vec![],
            archived_session_ids: vec![],
            pending_mutation: Some(PendingMutation {
                operation: PendingOperation::Create,
                workspace_id: WorkspaceId::new("pending"),
            }),
        })
        .unwrap(),
    );
    let harness = boot(
        pool.clone(),
        MemoryBackend::new(pool.clone()),
        Arc::new(Headers::default()),
    )
    .await
    .unwrap();
    assert!(harness.registry.list().unwrap().is_empty());
    harness.registry.close().await.unwrap();

    stored_workspace(
        &pool,
        vec![(
            "orphan".to_owned(),
            serde_json::to_value(record("/tmp/orphan", &[], "2026-01-01T00:00:00.000Z")).unwrap(),
        )],
        json!({ "initialized": true, "workspaceIds": [] }),
    );
    let error = boot(
        pool.clone(),
        MemoryBackend::new(pool),
        Arc::new(Headers::default()),
    )
    .await
    .err()
    .expect("unexplained orphan must fail startup");
    assert!(error.to_string().contains("absent from registry order"));
}

#[tokio::test]
async fn create_rollbacks_preserve_cache_table_and_aggregate_both_failures() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("owned");
    tokio::fs::create_dir(&dir).await.unwrap();

    let pool = Arc::new(Pool::default());
    let backend = MemoryBackend::failing(
        pool.clone(),
        FailAt {
            put: Some(1),
            ..FailAt::default()
        },
    );
    let harness = boot(pool, backend, Arc::new(Headers::default()))
        .await
        .unwrap();
    assert!(
        harness
            .registry
            .create(dir.to_string_lossy().into_owned(), None)
            .await
            .is_err()
    );
    assert!(harness.registry.list().unwrap().is_empty());

    let pool = Arc::new(Pool::default());
    let backend = MemoryBackend::failing(
        pool.clone(),
        FailAt {
            put: Some(1),
            global: vec![3],
            ..FailAt::default()
        },
    );
    let harness = boot(pool, backend, Arc::new(Headers::default()))
        .await
        .unwrap();
    let error = harness
        .registry
        .create(dir.to_string_lossy().into_owned(), None)
        .await
        .unwrap_err();
    let aggregate = error.downcast_ref::<WorkspaceAggregateError>().unwrap();
    assert_eq!(aggregate.errors.len(), 2);
    assert!(
        aggregate
            .message
            .contains("record write and pending-marker rollback")
    );
}

#[tokio::test]
async fn accepted_operations_are_eager_and_serial_even_when_result_is_dropped() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("owned");
    tokio::fs::create_dir(&dir).await.unwrap();
    let harness = fresh(Arc::new(Headers::default())).await.unwrap();
    drop(
        harness
            .registry
            .create(dir.to_string_lossy().into_owned(), None),
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if !harness.registry.list().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn bootstrap_reuses_partial_records_and_durable_preinitialized_order() {
    let temp = TempDir::new().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    tokio::fs::create_dir(&first).await.unwrap();
    tokio::fs::create_dir(&second).await.unwrap();
    let headers = Arc::new(Headers::default());
    *headers.values.lock() = vec![
        header("first", first.to_str(), 200),
        header("second", second.to_str(), 100),
    ];
    let pool = Arc::new(Pool::default());
    let failed = boot(
        pool.clone(),
        MemoryBackend::failing(
            pool.clone(),
            FailAt {
                put: Some(2),
                ..FailAt::default()
            },
        ),
        headers.clone(),
    )
    .await;
    assert!(failed.is_err());
    assert_eq!(pool.media.lock()["workspace"].tables["workspaces"].len(), 1);
    assert!(pool.media.lock()["workspace"].global.is_null());
    let retried = boot(pool.clone(), MemoryBackend::new(pool.clone()), headers)
        .await
        .unwrap();
    assert_eq!(retried.registry.list().unwrap().len(), 2);
    assert_eq!(pool.media.lock()["workspace"].tables["workspaces"].len(), 2);

    let one = temp.path().join("marker");
    tokio::fs::create_dir(&one).await.unwrap();
    let headers = Arc::new(Headers::default());
    headers
        .values
        .lock()
        .push(header("marker", one.to_str(), 100));
    let pool = Arc::new(Pool::default());
    assert!(
        boot(
            pool.clone(),
            MemoryBackend::failing(
                pool.clone(),
                FailAt {
                    global: vec![2],
                    ..FailAt::default()
                },
            ),
            headers.clone(),
        )
        .await
        .is_err()
    );
    let state: WorkspaceDomainState =
        serde_json::from_value(pool.media.lock()["workspace"].global.clone()).unwrap();
    assert!(!state.initialized);
    assert_eq!(state.workspace_ids.len(), 1);
    let retried = boot(pool.clone(), MemoryBackend::new(pool), headers)
        .await
        .unwrap();
    assert_eq!(retried.registry.list().unwrap().len(), 1);
}

#[tokio::test]
async fn bootstrap_merges_partial_records_and_leaves_accounted_drift_ungrouped() {
    let temp = TempDir::new().unwrap();
    let owned = tokio::fs::canonicalize({
        let path = temp.path().join("owned");
        tokio::fs::create_dir(&path).await.unwrap();
        path
    })
    .await
    .unwrap();
    let prior = tokio::fs::canonicalize({
        let path = temp.path().join("prior");
        tokio::fs::create_dir(&path).await.unwrap();
        path
    })
    .await
    .unwrap();
    let drifted = temp.path().join("drifted");
    tokio::fs::create_dir(&drifted).await.unwrap();
    let owned_id = WorkspaceId::new("00000000-0000-4000-8000-000000000010");
    let prior_id = WorkspaceId::new("00000000-0000-4000-8000-000000000011");
    let pool = Arc::new(Pool::default());
    stored_workspace(
        &pool,
        vec![
            (
                owned_id.to_string(),
                serde_json::to_value(record(
                    owned.to_str().unwrap(),
                    &["old"],
                    "2026-07-24T00:00:00.000Z",
                ))
                .unwrap(),
            ),
            (
                prior_id.to_string(),
                serde_json::to_value(record(
                    prior.to_str().unwrap(),
                    &["drift"],
                    "2026-07-23T00:00:00.000Z",
                ))
                .unwrap(),
            ),
        ],
        json!({ "initialized": false, "workspaceIds": [] }),
    );
    let headers = Arc::new(Headers::default());
    *headers.values.lock() = vec![
        header("new", owned.to_str(), 200),
        header("old", owned.to_str(), 100),
        header("drift", drifted.to_str(), 300),
    ];
    let harness = boot(pool.clone(), MemoryBackend::new(pool), headers)
        .await
        .unwrap();
    assert_eq!(
        harness.registry.get(&owned_id).unwrap().session_ids(),
        vec![SessionId::new("new"), SessionId::new("old")]
    );
    assert!(
        harness
            .registry
            .list()
            .unwrap()
            .iter()
            .all(|workspace| workspace.path() != drifted.to_string_lossy())
    );
}

#[tokio::test]
async fn bootstrap_stably_orders_headerless_rows_by_prior_order_then_id() {
    let temp = TempDir::new().unwrap();
    let owned = temp.path().join("owned");
    let prior = temp.path().join("prior");
    tokio::fs::create_dir(&owned).await.unwrap();
    tokio::fs::create_dir(&prior).await.unwrap();
    let owned = tokio::fs::canonicalize(owned).await.unwrap();
    let prior = tokio::fs::canonicalize(prior).await.unwrap();
    let first_id = WorkspaceId::new("00000000-0000-4000-8000-000000000020");
    let second_id = WorkspaceId::new("00000000-0000-4000-8000-000000000021");
    let entries = vec![
        (
            second_id.to_string(),
            serde_json::to_value(record(
                prior.to_str().unwrap(),
                &[],
                "2026-07-24T00:00:00.000Z",
            ))
            .unwrap(),
        ),
        (
            first_id.to_string(),
            serde_json::to_value(record(
                owned.to_str().unwrap(),
                &[],
                "2026-07-24T00:00:00.000Z",
            ))
            .unwrap(),
        ),
    ];
    let pool = Arc::new(Pool::default());
    stored_workspace(
        &pool,
        entries.clone(),
        serde_json::to_value(WorkspaceDomainState {
            initialized: false,
            workspace_ids: vec![second_id.clone(), first_id.clone()],
            archived_session_ids: vec![],
            pending_mutation: None,
        })
        .unwrap(),
    );
    let prior_order = boot(
        pool.clone(),
        MemoryBackend::new(pool),
        Arc::new(Headers::default()),
    )
    .await
    .unwrap();
    assert_eq!(
        ids(&prior_order.registry.list().unwrap()),
        vec![second_id.to_string(), first_id.to_string()]
    );
    prior_order.registry.close().await.unwrap();
    let pool = Arc::new(Pool::default());
    stored_workspace(
        &pool,
        entries,
        json!({ "initialized": false, "workspaceIds": [] }),
    );
    let by_id = boot(
        pool.clone(),
        MemoryBackend::new(pool),
        Arc::new(Headers::default()),
    )
    .await
    .unwrap();
    assert_eq!(
        ids(&by_id.registry.list().unwrap()),
        vec![first_id.to_string(), second_id.to_string()]
    );
}

#[tokio::test]
async fn delete_failure_rolls_back_or_keeps_recoverable_unpublished_direction() {
    let temp = TempDir::new().unwrap();
    let first_dir = temp.path().join("first");
    let second_dir = temp.path().join("second");
    tokio::fs::create_dir(&first_dir).await.unwrap();
    tokio::fs::create_dir(&second_dir).await.unwrap();

    let pool = Arc::new(Pool::default());
    let harness = boot(
        pool.clone(),
        MemoryBackend::failing(
            pool,
            FailAt {
                delete: Some(1),
                ..FailAt::default()
            },
        ),
        Arc::new(Headers::default()),
    )
    .await
    .unwrap();
    let workspace = harness
        .registry
        .create(first_dir.to_string_lossy().into_owned(), None)
        .await
        .unwrap();
    assert!(
        harness
            .registry
            .delete(workspace.id().clone())
            .await
            .is_err()
    );
    assert!(Arc::ptr_eq(
        &workspace,
        &harness.registry.get(workspace.id()).unwrap()
    ));
    assert_eq!(harness.registry.list().unwrap().len(), 1);

    let pool = Arc::new(Pool::default());
    let harness = boot(
        pool.clone(),
        MemoryBackend::failing(
            pool,
            FailAt {
                delete: Some(1),
                global: vec![5],
                ..FailAt::default()
            },
        ),
        Arc::new(Headers::default()),
    )
    .await
    .unwrap();
    let workspace = harness
        .registry
        .create(second_dir.to_string_lossy().into_owned(), None)
        .await
        .unwrap();
    let error = harness
        .registry
        .delete(workspace.id().clone())
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<WorkspaceAggregateError>().is_some());
    assert!(harness.registry.get(workspace.id()).is_none());
    let state: WorkspaceDomainState =
        serde_json::from_value(harness.pool.media.lock()["workspace"].global.clone()).unwrap();
    assert_eq!(
        state.pending_mutation,
        Some(PendingMutation {
            operation: PendingOperation::Delete,
            workspace_id: workspace.id().clone()
        })
    );
}

#[tokio::test]
async fn delete_cleanup_failure_commits_and_next_operation_recovers_marker() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("owned");
    tokio::fs::create_dir(&dir).await.unwrap();
    let pool = Arc::new(Pool::default());
    let harness = boot(
        pool.clone(),
        MemoryBackend::failing(
            pool,
            FailAt {
                global: vec![5],
                ..FailAt::default()
            },
        ),
        Arc::new(Headers::default()),
    )
    .await
    .unwrap();
    let deleted = harness
        .registry
        .create(dir.to_string_lossy().into_owned(), None)
        .await
        .unwrap();
    assert!(harness.registry.delete(deleted.id().clone()).await.unwrap());
    let state: WorkspaceDomainState =
        serde_json::from_value(harness.pool.media.lock()["workspace"].global.clone()).unwrap();
    assert!(state.pending_mutation.is_some());
    let recreated = harness
        .registry
        .create(dir.to_string_lossy().into_owned(), None)
        .await
        .unwrap();
    assert_ne!(recreated.id(), deleted.id());
    let state: WorkspaceDomainState =
        serde_json::from_value(harness.pool.media.lock()["workspace"].global.clone()).unwrap();
    assert!(state.pending_mutation.is_none());
    assert_eq!(state.workspace_ids, vec![recreated.id().clone()]);
}

#[tokio::test]
async fn projection_prunes_only_on_mutation_and_membership_race_is_chain_ordered() {
    let temp = TempDir::new().unwrap();
    let owned = tokio::fs::canonicalize({
        let path = temp.path().join("owned");
        tokio::fs::create_dir(&path).await.unwrap();
        path
    })
    .await
    .unwrap();
    let elsewhere = temp.path().join("elsewhere");
    tokio::fs::create_dir(&elsewhere).await.unwrap();
    let id = WorkspaceId::new("00000000-0000-4000-8000-000000000001");
    let pool = Arc::new(Pool::default());
    stored_workspace(
        &pool,
        vec![(
            id.to_string(),
            serde_json::to_value(record(
                owned.to_str().unwrap(),
                &["good", "mismatch", "missing"],
                "2026-07-24T00:00:00.000Z",
            ))
            .unwrap(),
        )],
        json!({ "initialized": true, "workspaceIds": [id] }),
    );
    let headers = Arc::new(Headers::default());
    *headers.values.lock() = vec![
        header("good", owned.to_str(), 1),
        header("mismatch", elsewhere.to_str(), 2),
        header("cwd-only", owned.to_str(), 3),
    ];
    let harness = boot(
        pool.clone(),
        MemoryBackend::new(pool.clone()),
        headers.clone(),
    )
    .await
    .unwrap();
    let workspace = harness.registry.get(&id).unwrap();
    assert_eq!(workspace.session_ids(), vec![SessionId::new("good")]);
    assert_eq!(
        serde_json::from_value::<WorkspaceRecord>(
            pool.media.lock()["workspace"].tables["workspaces"][id.as_str()].clone()
        )
        .unwrap()
        .session_ids
        .len(),
        3
    );
    workspace.set_title("pruned".to_owned()).await.unwrap();
    assert_eq!(
        serde_json::from_value::<WorkspaceRecord>(
            pool.media.lock()["workspace"].tables["workspaces"][id.as_str()].clone()
        )
        .unwrap()
        .session_ids,
        vec![SessionId::new("good")]
    );
    assert_eq!(
        headers
            .list_calls
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );

    let detached = workspace.detach_session(SessionId::new("good"));
    let attached = workspace.attach_session(SessionId::new("good"));
    let (detached, attached) = tokio::join!(detached, attached);
    detached.unwrap();
    attached.unwrap();
    assert_eq!(workspace.session_ids(), vec![SessionId::new("good")]);
}

#[tokio::test]
async fn startup_rejects_all_unexplained_order_path_and_accounting_corruption() {
    let temp = TempDir::new().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    tokio::fs::create_dir(&first).await.unwrap();
    tokio::fs::create_dir(&second).await.unwrap();
    let first = tokio::fs::canonicalize(first).await.unwrap();
    let second = tokio::fs::canonicalize(second).await.unwrap();
    let first_id = WorkspaceId::new("first");
    let second_id = WorkspaceId::new("second");
    let cases = vec![
        (
            vec![
                (
                    first_id.to_string(),
                    serde_json::to_value(record(
                        first.to_str().unwrap(),
                        &["dup"],
                        "2026-01-01T00:00:00.000Z",
                    ))
                    .unwrap(),
                ),
                (
                    second_id.to_string(),
                    serde_json::to_value(record(
                        second.to_str().unwrap(),
                        &["dup"],
                        "2026-01-01T00:00:00.000Z",
                    ))
                    .unwrap(),
                ),
            ],
            json!({ "initialized": true, "workspaceIds": [first_id, second_id] }),
            "accounted",
        ),
        (
            vec![
                (
                    first_id.to_string(),
                    serde_json::to_value(record(
                        first.to_str().unwrap(),
                        &[],
                        "2026-01-01T00:00:00.000Z",
                    ))
                    .unwrap(),
                ),
                (
                    second_id.to_string(),
                    serde_json::to_value(record(
                        first.to_str().unwrap(),
                        &[],
                        "2026-01-01T00:00:00.000Z",
                    ))
                    .unwrap(),
                ),
            ],
            json!({ "initialized": true, "workspaceIds": [first_id, second_id] }),
            "claimed",
        ),
        (
            vec![(
                first_id.to_string(),
                serde_json::to_value(record(
                    first.to_str().unwrap(),
                    &[],
                    "2026-01-01T00:00:00.000Z",
                ))
                .unwrap(),
            )],
            json!({ "initialized": true, "workspaceIds": [first_id, first_id] }),
            "repeats workspace",
        ),
        (
            vec![],
            json!({ "initialized": true, "workspaceIds": [first_id] }),
            "references missing workspace",
        ),
    ];
    for (entries, state, expected) in cases {
        let pool = Arc::new(Pool::default());
        stored_workspace(&pool, entries, state);
        let error = boot(
            pool.clone(),
            MemoryBackend::new(pool),
            Arc::new(Headers::default()),
        )
        .await
        .err()
        .expect("corrupt registry must fail");
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}

#[tokio::test]
async fn timestamps_snapshot_failure_and_archive_restart_are_durable() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("owned");
    tokio::fs::create_dir(&dir).await.unwrap();
    let pool = Arc::new(Pool::default());
    let headers = Arc::new(Headers::default());
    headers
        .values
        .lock()
        .push(header("archived", dir.to_str(), 1));
    let harness = boot(
        pool.clone(),
        MemoryBackend::failing(
            pool.clone(),
            FailAt {
                put: Some(3),
                ..FailAt::default()
            },
        ),
        headers.clone(),
    )
    .await
    .unwrap();
    let workspace = harness
        .registry
        .resolve_by_path(dir.to_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let created = workspace.created_at();
    workspace.set_title("kept".to_owned()).await.unwrap();
    assert_eq!(workspace.created_at(), created);
    assert!(workspace.set_title("lost".to_owned()).await.is_err());
    assert_eq!(workspace.title(), "kept");
    harness
        .registry
        .archive_session(SessionId::new("archived"))
        .await
        .unwrap();
    harness.registry.close().await.unwrap();
    let restarted = boot(pool.clone(), MemoryBackend::new(pool.clone()), headers)
        .await
        .unwrap();
    assert_eq!(
        restarted.registry.archived_session_ids(),
        vec![SessionId::new("archived")]
    );

    restarted.registry.close().await.unwrap();
    let legacy_id = WorkspaceId::new("legacy");
    stored_workspace(
        &pool,
        vec![(
            legacy_id.to_string(),
            serde_json::to_value(record(
                dir.to_str().unwrap(),
                &[],
                "2026-01-01T00:00:00.000Z",
            ))
            .unwrap(),
        )],
        json!({ "initialized": true, "workspaceIds": [legacy_id] }),
    );
    let legacy = boot(
        pool.clone(),
        MemoryBackend::new(pool),
        Arc::new(Headers::default()),
    )
    .await
    .unwrap();
    assert!(legacy.registry.archived_session_ids().is_empty());
}

#[tokio::test]
async fn invariant_enforces_cache_ownership_and_ignores_foreign_domain_events() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("owned");
    tokio::fs::create_dir(&dir).await.unwrap();
    let harness = fresh(Arc::new(Headers::default())).await.unwrap();
    let invariants =
        InvariantRegistry::install(&harness.context, &InvariantConfig::default()).unwrap();
    let registration = seekdeep_workspace::register_invariant(&invariants).unwrap();
    registration.await_ready().await.unwrap();
    let workspace = harness
        .registry
        .create(dir.to_string_lossy().into_owned(), None)
        .await
        .unwrap();
    let value =
        serde_json::to_value(record(&workspace.path(), &[], "2026-01-01T00:00:00.000Z")).unwrap();

    harness
        .context
        .events()
        .emit(
            &harness.context,
            "domain/changed",
            &EventArgs::one(DomainChanged::Put {
                domain: "foreign".to_owned(),
                table: "workspaces".to_owned(),
                key: "missing".to_owned(),
                value: value.clone(),
            }),
        )
        .unwrap();
    harness
        .context
        .events()
        .emit(
            &harness.context,
            "domain/changed",
            &EventArgs::one(DomainChanged::Put {
                domain: "workspace".to_owned(),
                table: "workspaces".to_owned(),
                key: workspace.id().to_string(),
                value: value.clone(),
            }),
        )
        .unwrap();

    let deletion = harness.context.events().emit(
        &harness.context,
        "domain/changed",
        &EventArgs::one(DomainChanged::Deleted {
            domain: "workspace".to_owned(),
            table: "workspaces".to_owned(),
            key: workspace.id().to_string(),
        }),
    );
    assert!(
        deletion
            .unwrap_err()
            .to_string()
            .contains("deleted while the registry cache still publishes")
    );
    let unknown_put = harness.context.events().emit(
        &harness.context,
        "domain/changed",
        &EventArgs::one(DomainChanged::Put {
            domain: "workspace".to_owned(),
            table: "workspaces".to_owned(),
            key: "unknown".to_owned(),
            value,
        }),
    );
    assert!(
        unknown_put
            .unwrap_err()
            .to_string()
            .contains("cache holds no entity")
    );

    harness
        .registry
        .delete(workspace.id().clone())
        .await
        .unwrap();
    harness
        .context
        .events()
        .emit(
            &harness.context,
            "domain/changed",
            &EventArgs::one(DomainChanged::Deleted {
                domain: "workspace".to_owned(),
                table: "workspaces".to_owned(),
                key: workspace.id().to_string(),
            }),
        )
        .unwrap();
}

#[tokio::test]
async fn synchronous_domain_observers_see_a_coherent_committed_registry() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("observed");
    tokio::fs::create_dir(&dir).await.unwrap();
    let harness = fresh(Arc::new(Headers::default())).await.unwrap();
    let observed = Arc::new(parking_lot::Mutex::new(Vec::<Vec<WorkspaceId>>::new()));
    let registry = harness.registry.clone();
    let listener_observed = observed.clone();
    let _listener = harness
        .context
        .events()
        .on_sync(
            &harness.context,
            "domain/changed",
            move |_context, args| {
                let change = args
                    .get::<DomainChanged>(0)
                    .ok_or_else(|| anyhow::anyhow!("missing change"))?;
                if change.domain() == "workspace" {
                    listener_observed.lock().push(
                        registry
                            .list()?
                            .into_iter()
                            .map(|workspace| workspace.id().clone())
                            .collect(),
                    );
                }
                Ok(EventReply::default())
            },
            EventOptions::default(),
        )
        .unwrap();

    let workspace = tokio::time::timeout(
        Duration::from_secs(1),
        harness
            .registry
            .create(dir.to_string_lossy().into_owned(), None),
    )
    .await
    .expect("a synchronous observer must not deadlock the commit lock")
    .unwrap();

    let snapshots = observed.lock();
    assert!(!snapshots.is_empty());
    assert!(snapshots.iter().all(|snapshot| {
        snapshot.is_empty() || snapshot.as_slice() == [workspace.id().clone()]
    }));
    assert_eq!(snapshots.last().unwrap(), &[workspace.id().clone()]);
}

#[tokio::test]
async fn plugin_waits_for_persistence_and_reopens_per_dependency_epoch() {
    let context = Context::new();
    let storage = Storage::new();
    let _storage_effect = storage.provide(&context).unwrap();
    let pool = Arc::new(Pool::default());
    let backend = MemoryBackend::new(pool.clone());
    let _registration = storage
        .backend
        .register("memory", backend as Arc<dyn StorageBackend>)
        .unwrap();
    let facility = DomainFacility::new(
        context.clone(),
        storage.clone(),
        DomainConfig {
            backend: "memory".to_owned(),
            routes: HashMap::new(),
        },
    );
    let (_facility_effect, _mount) = facility.mount(&context).unwrap();
    let mounted = context
        .plugin(seekdeep_workspace::plugin(), serde_json::Value::Null)
        .unwrap();
    mounted.await_settled().await.unwrap();
    assert_eq!(mounted.fiber().state(), FiberState::Pending);
    assert!(
        context
            .get(seekdeep_workspace::WORKSPACE_REGISTRY)
            .is_none()
    );
    assert!(!pool.media.lock().contains_key("workspace"));

    let persistence = SessionPersistenceService::new(Arc::new(Headers::default()));
    let first_epoch = persistence.provide(&context).unwrap();
    mounted.await_settled().await.unwrap();
    assert_eq!(mounted.fiber().state(), FiberState::Active);
    assert!(
        context
            .get(seekdeep_workspace::WORKSPACE_REGISTRY)
            .is_some()
    );
    assert_eq!(
        pool.media.lock()["workspace"].global,
        json!({
            "initialized": true,
            "workspaceIds": [],
            "archivedSessionIds": [],
        })
    );

    first_epoch.dispose().await.unwrap();
    mounted.await_settled().await.unwrap();
    assert_eq!(mounted.fiber().state(), FiberState::Pending);
    assert!(
        context
            .get(seekdeep_workspace::WORKSPACE_REGISTRY)
            .is_none()
    );
    assert!(facility.get("workspace").is_none());

    let _second_epoch = persistence.provide(&context).unwrap();
    mounted.await_settled().await.unwrap();
    assert_eq!(mounted.fiber().state(), FiberState::Active);
    assert!(
        context
            .get(seekdeep_workspace::WORKSPACE_REGISTRY)
            .is_some()
    );
    mounted.dispose().await.unwrap();
    assert!(
        context
            .get(seekdeep_workspace::WORKSPACE_REGISTRY)
            .is_none()
    );
    assert!(facility.get("workspace").is_none());
}

#[tokio::test]
async fn complete_storage_plugin_stack_activates_and_unwinds_transitively() {
    let storage_root = TempDir::new().unwrap();
    let owned_root = TempDir::new().unwrap();
    let directory = owned_root.path().join("owned");
    tokio::fs::create_dir(&directory).await.unwrap();
    let context = Context::new();

    let workspace_plugin = context
        .plugin(seekdeep_workspace::plugin(), serde_json::Value::Null)
        .unwrap();
    let domain_plugin = context
        .plugin(
            seekdeep_storage_domain::plugin(),
            json!({ "backend": "json" }),
        )
        .unwrap();
    let json_plugin = context
        .plugin(
            seekdeep_storage_json::plugin(),
            json!({ "root": storage_root.path().to_string_lossy() }),
        )
        .unwrap();
    let storage_plugin = context
        .plugin(seekdeep_storage::plugin(), serde_json::Value::Null)
        .unwrap();
    storage_plugin.await_settled().await.unwrap();
    json_plugin.await_settled().await.unwrap();
    domain_plugin.await_settled().await.unwrap();
    workspace_plugin.await_settled().await.unwrap();
    assert_eq!(workspace_plugin.fiber().state(), FiberState::Pending);
    assert!(
        context
            .get(seekdeep_workspace::WORKSPACE_REGISTRY)
            .is_none()
    );

    let persistence = SessionPersistenceService::new(Arc::new(Headers::default()));
    let _persistence = persistence.provide(&context).unwrap();
    workspace_plugin.await_settled().await.unwrap();
    let registry = context.get(seekdeep_workspace::WORKSPACE_REGISTRY).unwrap();
    let workspace = registry
        .create(directory.to_string_lossy().into_owned(), None)
        .await
        .unwrap();
    assert_eq!(registry.list().unwrap()[0].id(), workspace.id());
    assert!(storage_root.path().join("workspace.json").is_file());

    json_plugin.dispose().await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if context
                .get(seekdeep_workspace::WORKSPACE_REGISTRY)
                .is_none()
                && context
                    .get(seekdeep_storage_domain::STORAGE_DOMAIN)
                    .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("backend withdrawal must transitively unwind domain and workspace services");
    workspace_plugin.await_settled().await.unwrap();
    assert_eq!(workspace_plugin.fiber().state(), FiberState::Pending);
    domain_plugin.dispose().await.unwrap();
    workspace_plugin.dispose().await.unwrap();
    storage_plugin.dispose().await.unwrap();
}
