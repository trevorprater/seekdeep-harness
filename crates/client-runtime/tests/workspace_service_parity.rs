//! Workspace entity, manager, Sessions-coupled service, and action parity.

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
};

use futures::{FutureExt, channel::oneshot, executor::LocalPool, task::LocalSpawnExt};
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerViewDefinitions, ClientRpcError, ClientRpcResult,
    ClientWorkspace, ClientWorkspaceView, ConversationNodeAssembler, DirectoryBrowseCallFailure,
    ManagerHostFrame, ManagerSessionSummary, NotifierScheduler, RuntimeDisposer,
    RuntimeSessionScope, SessionHistoryPage, SessionHistoryRequest, SessionManager,
    SessionManagerOptions, SessionManagerTimer, SessionRuntime, SessionRuntimeOptions,
    SessionRuntimeWorkspacePort, SessionScopeFactory, SessionSelection, SessionSelectionStorage,
    SessionTaskSpawner, SessionTransport, SessionTransportRequest, WorkspaceCreateInput,
    WorkspaceHostFrame, WorkspaceIntentPhase, WorkspaceListPhase, WorkspaceListState,
    WorkspaceManager, WorkspaceManagerOptions, WorkspaceRuntime, WorkspaceRuntimeOptions,
    WorkspaceSessionsPort,
};
use seekdeep_identity::{SessionId, WorkspaceId};
use serde_json::{Value, json};

#[derive(Default)]
struct Scheduler {
    tasks: RefCell<VecDeque<Box<dyn FnOnce()>>>,
}

impl NotifierScheduler for Scheduler {
    fn has_animation_frame(&self) -> bool {
        false
    }

    fn queue_microtask(&self, callback: Box<dyn FnOnce()>) {
        self.tasks.borrow_mut().push_back(callback);
    }

    fn queue_animation_frame(&self, callback: Box<dyn FnOnce()>) {
        self.queue_microtask(callback);
    }
}

impl Scheduler {
    fn flush(&self) {
        while let Some(task) = self.tasks.borrow_mut().pop_front() {
            task();
        }
    }
}

struct Spawner(futures::executor::LocalSpawner);

impl SessionTaskSpawner for Spawner {
    fn spawn(&self, task: futures::future::LocalBoxFuture<'static, ()>) {
        self.0.spawn_local(task).unwrap();
    }
}

struct Timer;

impl SessionManagerTimer for Timer {
    fn schedule(&self, _delay_ms: u64, callback: Box<dyn FnOnce()>) -> RuntimeDisposer {
        callback();
        RuntimeDisposer::new(|| {})
    }
}

type TransportResult = Result<ClientRpcResult<Value>, String>;

enum Reply {
    Ready(TransportResult),
    Pending(oneshot::Receiver<TransportResult>),
}

#[derive(Default)]
struct Transport {
    calls: RefCell<Vec<SessionTransportRequest>>,
    replies: RefCell<HashMap<String, VecDeque<Reply>>>,
}

impl Transport {
    fn push(&self, method: &str, result: ClientRpcResult<Value>) {
        self.replies
            .borrow_mut()
            .entry(method.to_owned())
            .or_default()
            .push_back(Reply::Ready(Ok(result)));
    }

    fn reject(&self, method: &str, message: &str) {
        self.replies
            .borrow_mut()
            .entry(method.to_owned())
            .or_default()
            .push_back(Reply::Ready(Err(message.to_owned())));
    }

    fn gate(&self, method: &str) -> oneshot::Sender<TransportResult> {
        let (sender, receiver) = oneshot::channel();
        self.replies
            .borrow_mut()
            .entry(method.to_owned())
            .or_default()
            .push_back(Reply::Pending(receiver));
        sender
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.calls
            .borrow()
            .iter()
            .filter(|call| call.method == method)
            .map(|call| call.payload.clone())
            .collect()
    }
}

impl SessionTransport for Transport {
    fn history(
        &self,
        _request: SessionHistoryRequest,
    ) -> futures::future::LocalBoxFuture<'static, Result<ClientRpcResult<SessionHistoryPage>, String>>
    {
        futures::future::ready(Ok(ClientRpcResult::Success(Some(SessionHistoryPage {
            entries: Vec::new(),
            has_more: false,
            projections: None,
        }))))
        .boxed_local()
    }

    fn call(
        &self,
        request: SessionTransportRequest,
    ) -> futures::future::LocalBoxFuture<'static, TransportResult> {
        let method = request.method.clone();
        self.calls.borrow_mut().push(request.clone());
        let reply = self
            .replies
            .borrow_mut()
            .get_mut(&method)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| Reply::Ready(Ok(default_result(&method, &request.payload))));
        async move {
            match reply {
                Reply::Ready(result) => result,
                Reply::Pending(receiver) => receiver
                    .await
                    .unwrap_or_else(|_| Err("test gate dropped".to_owned())),
            }
        }
        .boxed_local()
    }
}

fn default_result(method: &str, payload: &Value) -> ClientRpcResult<Value> {
    let value = match method {
        "workspace.list" => json!({"items":[],"archivedSessionIds":[]}),
        "session.list" => json!({"items":[]}),
        "subagent.list" => json!({"entries":[],"parentAvailable":false}),
        "workspace.delete" => json!({"deleted":true}),
        "workspace.insertBefore" => json!({"workspaceIds":[]}),
        "workspace.archiveSession" => {
            json!({"archivedSessionIds":[payload["sessionId"].clone()]})
        }
        "host.pickDirectory" => json!({"path":null}),
        "host.listDirectory" => {
            json!({"path":"/","home":"/","crumbs":[],"entries":[],"truncated":false})
        }
        "host.createDirectory" => json!({"path":"/created"}),
        "host.openPath" => json!({"opened":true}),
        _ => Value::Null,
    };
    ClientRpcResult::Success(Some(value))
}

struct EmptyEvents;

impl AssemblerEventDefinitions for EmptyEvents {
    fn entries(&self) -> Vec<Rc<seekdeep_client_runtime::AssemblerNodeDefinition>> {
        Vec::new()
    }

    fn fallback_entry(&self) -> Option<Rc<seekdeep_client_runtime::AssemblerNodeDefinition>> {
        None
    }
}

struct EmptyViews;

impl AssemblerViewDefinitions for EmptyViews {
    fn entries(&self) -> Vec<Rc<seekdeep_client_runtime::AssemblerViewDefinition>> {
        Vec::new()
    }
}

#[derive(Default)]
struct Storage(RefCell<SessionSelection>);

impl SessionSelectionStorage for Storage {
    fn load(&self) -> SessionSelection {
        self.0.borrow().clone()
    }

    fn store(&self, selection: &SessionSelection) {
        *self.0.borrow_mut() = selection.clone();
    }

    fn clear(&self) {
        *self.0.borrow_mut() = SessionSelection::default();
    }
}

struct Scopes;

impl SessionScopeFactory for Scopes {
    fn create(&self, session_id: &SessionId) -> Rc<RuntimeSessionScope> {
        RuntimeSessionScope::new(session_id.clone(), Value::Null, RuntimeDisposer::new(|| {}))
    }
}

fn workspace(id: &str, sessions: &[&str], created_at: &str) -> Rc<ClientWorkspaceView> {
    Rc::new(ClientWorkspaceView {
        workspace_id: WorkspaceId::new(id),
        path: format!("/w/{id}"),
        title: id.to_owned(),
        session_ids: sessions.iter().map(|id| SessionId::new(*id)).collect(),
        created_at: created_at.to_owned(),
        updated_at: created_at.to_owned(),
    })
}

fn workspace_json(id: &str, sessions: &[&str], created_at: &str) -> Value {
    serde_json::to_value(workspace(id, sessions, created_at).as_ref()).unwrap()
}

fn summary(id: &str, cwd: Option<&str>, updated_at: i64, blank: bool) -> ManagerSessionSummary {
    ManagerSessionSummary {
        session_id: SessionId::new(id),
        updated_at,
        running: false,
        blank,
        parent_session_id: None,
        origin: None,
        cwd: cwd.map(ToOwned::to_owned),
        agent_preset: None,
        projections: None,
    }
}

fn failure(code: &str, message: &str) -> ClientRpcResult<Value> {
    ClientRpcResult::Failure(ClientRpcError {
        code: code.to_owned(),
        message: message.to_owned(),
        details: serde_json::Map::new(),
    })
}

struct ManagerBench {
    pool: LocalPool,
    scheduler: Rc<Scheduler>,
    transport: Rc<Transport>,
    manager: Rc<WorkspaceManager>,
}

fn manager_bench() -> ManagerBench {
    let pool = LocalPool::new();
    let scheduler = Rc::new(Scheduler::default());
    let transport = Rc::new(Transport::default());
    let manager = WorkspaceManager::new(
        transport.clone(),
        WorkspaceManagerOptions {
            scheduler: scheduler.clone(),
            spawner: Rc::new(Spawner(pool.spawner())),
            parse_date: Rc::new(|value| value.parse().unwrap_or(f64::NAN)),
        },
    );
    ManagerBench {
        pool,
        scheduler,
        transport,
        manager,
    }
}

struct RuntimeBench {
    pool: LocalPool,
    scheduler: Rc<Scheduler>,
    transport: Rc<Transport>,
    session_manager: Rc<SessionManager>,
    sessions: Rc<SessionRuntime>,
    workspaces: Rc<WorkspaceRuntime>,
    reports: Rc<RefCell<Vec<String>>>,
}

fn runtime_bench() -> RuntimeBench {
    let pool = LocalPool::new();
    let scheduler = Rc::new(Scheduler::default());
    let transport = Rc::new(Transport::default());
    let session_manager = SessionManager::new(
        transport.clone(),
        None,
        SessionManagerOptions {
            scheduler: scheduler.clone(),
            spawner: Rc::new(Spawner(pool.spawner())),
            timer: Rc::new(Timer),
            resolve_time_zone: Rc::new(|| Ok("UTC".to_owned())),
            create_conversation: Rc::new(|| {
                ConversationNodeAssembler::new(Rc::new(EmptyEvents), Rc::new(EmptyViews))
            }),
            clock: Rc::new(|| 10),
            report: Rc::new(|_| {}),
        },
    );
    let sessions = SessionRuntime::new(
        &session_manager,
        SessionRuntimeOptions {
            selection: Rc::new(Storage::default()),
            scopes: Rc::new(Scopes),
            spawner: Rc::new(Spawner(pool.spawner())),
            prune_store_scope: Rc::new(|_| {}),
        },
    );
    let reports = Rc::new(RefCell::new(Vec::new()));
    let observed = reports.clone();
    let sessions_port: Rc<dyn WorkspaceSessionsPort> =
        SessionRuntimeWorkspacePort::new(sessions.clone());
    let workspaces = WorkspaceRuntime::new(
        transport.clone(),
        &sessions_port,
        WorkspaceRuntimeOptions {
            manager: WorkspaceManagerOptions {
                scheduler: scheduler.clone(),
                spawner: Rc::new(Spawner(pool.spawner())),
                parse_date: Rc::new(|value| value.parse().unwrap_or(f64::NAN)),
            },
            spawner: Rc::new(Spawner(pool.spawner())),
            report: Rc::new(move |message| observed.borrow_mut().push(message.to_owned())),
        },
    );
    RuntimeBench {
        pool,
        scheduler,
        transport,
        session_manager,
        sessions,
        workspaces,
        reports,
    }
}

fn flush_manager(bench: &mut ManagerBench) {
    bench.pool.run_until_stalled();
    bench.scheduler.flush();
    bench.pool.run_until_stalled();
    bench.scheduler.flush();
}

fn flush_runtime(bench: &mut RuntimeBench) {
    bench.pool.run_until_stalled();
    bench.scheduler.flush();
    bench.pool.run_until_stalled();
    bench.scheduler.flush();
}

#[test]
fn workspace_entity_materialization_is_singleflight_retryable_and_identity_stable() {
    let mut bench = manager_bench();
    let local = ClientWorkspace::local(
        bench.transport.clone(),
        bench.scheduler.clone(),
        WorkspaceCreateInput {
            path: "/w/local/".to_owned(),
        },
    );
    assert_eq!(local.snapshot().intent.as_ref().unwrap().name, "local");
    let gate = bench.transport.gate("workspace.create");
    let first = local.materialize().unwrap();
    let second = local.materialize().unwrap();
    assert_eq!(
        local.snapshot().intent.as_ref().unwrap().phase,
        WorkspaceIntentPhase::Creating
    );
    gate.send(Ok(ClientRpcResult::Success(Some(json!({
        "workspace":workspace_json("local", &[], "1"),"created":true
    })))))
    .unwrap();
    let (left, right) = bench.pool.run_until(futures::future::join(first, second));
    assert_eq!(left, right);
    bench.scheduler.flush();
    assert_eq!(
        local
            .snapshot()
            .view
            .as_ref()
            .unwrap()
            .workspace_id
            .as_str(),
        "local"
    );
    assert!(local.materialize().is_none());
    assert!(local.adopt(workspace("other", &[], "2")).is_err());

    let replacement = ClientWorkspace::local(
        bench.transport.clone(),
        bench.scheduler.clone(),
        WorkspaceCreateInput {
            path: "/w/replacement".to_owned(),
        },
    );
    let gate = bench.transport.gate("workspace.create");
    let pending = replacement.materialize().unwrap();
    replacement.adopt(workspace("external", &[], "3")).unwrap();
    gate.send(Ok(ClientRpcResult::Success(Some(json!({
        "workspace":workspace_json("late-create", &[], "4"),"created":true
    })))))
    .unwrap();
    bench.pool.run_until(pending);
    bench.scheduler.flush();
    assert_eq!(
        replacement
            .snapshot()
            .view
            .as_ref()
            .unwrap()
            .workspace_id
            .as_str(),
        "external"
    );
}

#[test]
fn manager_replays_frames_over_refresh_and_reestablishes_host_order() {
    let mut bench = manager_bench();
    let gate = bench.transport.gate("workspace.list");
    let refresh = bench.manager.refresh();
    bench
        .manager
        .handle_host_frame(WorkspaceHostFrame::Changed(workspace("new", &[], "2")));
    gate.send(Ok(ClientRpcResult::Success(Some(json!({
        "items":[workspace_json("old", &[], "1")],"archivedSessionIds":[]
    })))))
    .unwrap();
    bench.pool.run_until(refresh);
    flush_manager(&mut bench);
    assert_eq!(bench.manager.snapshot().phase, WorkspaceListPhase::Ready);
    assert_eq!(
        bench
            .manager
            .snapshot()
            .items
            .iter()
            .map(|item| item.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["new", "old"]
    );
    bench.transport.push(
        "workspace.list",
        ClientRpcResult::Success(Some(json!({
            "items":[workspace_json("old", &[], "1"),workspace_json("new", &[], "2")],
            "archivedSessionIds":[]
        }))),
    );
    let refresh = bench.manager.refresh();
    bench.pool.run_until(refresh);
    flush_manager(&mut bench);
    assert_eq!(
        bench
            .manager
            .snapshot()
            .items
            .iter()
            .map(|item| item.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["old", "new"]
    );
}

#[test]
fn manager_singleflights_and_keeps_readiness_across_business_and_transport_failures() {
    let mut bench = manager_bench();
    let gate = bench.transport.gate("workspace.list");
    let first = bench.manager.refresh();
    let second = bench.manager.refresh();
    assert_eq!(bench.manager.snapshot().state, WorkspaceListState::Loading);
    gate.send(Ok(ClientRpcResult::Success(Some(json!({
        "items":[],"archivedSessionIds":[]
    })))))
    .unwrap();
    bench.pool.run_until(futures::future::join(first, second));
    assert_eq!(bench.transport.calls("workspace.list").len(), 1);
    bench
        .transport
        .push("workspace.list", failure("internal", "down"));
    let refresh = bench.manager.refresh();
    bench.pool.run_until(refresh);
    bench.scheduler.flush();
    assert_eq!(bench.manager.snapshot().phase, WorkspaceListPhase::Ready);
    assert_eq!(bench.manager.snapshot().state, WorkspaceListState::Error);
    bench.transport.reject("workspace.list", "wire down");
    let refresh = bench.manager.refresh();
    bench.pool.run_until(refresh);
    bench.scheduler.flush();
    assert_eq!(
        bench.manager.snapshot().error.as_ref().unwrap().message,
        "wire down"
    );
}

#[test]
fn manager_create_reorder_arbitration_and_rollbacks_match_host_authority() {
    let mut bench = manager_bench();
    bench.transport.push(
        "workspace.create",
        ClientRpcResult::Success(Some(json!({
            "workspace":workspace_json("created", &[], "4"),"created":true
        }))),
    );
    let created = bench
        .pool
        .run_until(bench.manager.create(WorkspaceCreateInput {
            path: "/w/created".to_owned(),
        }));
    assert!(created.is_ok());
    bench.transport.push(
        "workspace.list",
        ClientRpcResult::Success(Some(json!({
            "items":[
                workspace_json("one", &[], "1"),workspace_json("two", &[], "2"),
                workspace_json("three", &[], "3")
            ],"archivedSessionIds":[]
        }))),
    );
    let refresh = bench.manager.refresh();
    bench.pool.run_until(refresh);
    let gate = bench.transport.gate("workspace.insertBefore");
    let three = WorkspaceId::new("three");
    let one = WorkspaceId::new("one");
    let pending = bench.manager.insert_before(&three, Some(&one));
    bench.pool.run_until_stalled();
    bench
        .manager
        .handle_host_frame(WorkspaceHostFrame::OrderChanged(Rc::new(vec![
            WorkspaceId::new("one"),
            WorkspaceId::new("three"),
            WorkspaceId::new("two"),
        ])));
    gate.send(Ok(ClientRpcResult::Success(Some(json!({
        "workspaceIds":["three","one","two"]
    })))))
    .unwrap();
    bench.pool.run_until(pending).unwrap();
    bench.scheduler.flush();
    assert_eq!(
        bench
            .manager
            .snapshot()
            .items
            .iter()
            .map(|item| item.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "three", "two"]
    );
    bench.transport.push(
        "workspace.insertBefore",
        failure("workspace-not-found", "gone"),
    );
    let rejected = bench.manager.insert_before(&three, None);
    bench.pool.run_until_stalled();
    assert_eq!(
        bench
            .manager
            .snapshot()
            .items
            .iter()
            .map(|item| item.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two", "three"]
    );
    assert!(matches!(
        bench.pool.run_until(rejected),
        Ok(ClientRpcResult::Failure(_))
    ));
    bench.scheduler.flush();
    assert_eq!(
        bench
            .manager
            .snapshot()
            .items
            .iter()
            .map(|item| item.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "three", "two"]
    );
}

#[test]
fn manager_tombstones_removed_rows_and_replays_delete_over_stale_refresh() {
    let mut bench = manager_bench();
    bench.transport.push(
        "workspace.list",
        ClientRpcResult::Success(Some(json!({
            "items":[workspace_json("gone", &[], "1")],"archivedSessionIds":[]
        }))),
    );
    let refresh = bench.manager.refresh();
    bench.pool.run_until(refresh);
    let gate = bench.transport.gate("workspace.list");
    let refresh = bench.manager.refresh();
    let gone = WorkspaceId::new("gone");
    let deletion = bench.manager.delete(&gone);
    bench.pool.run_until(deletion).unwrap();
    assert!(bench.manager.snapshot().items.is_empty());
    gate.send(Ok(ClientRpcResult::Success(Some(json!({
        "items":[workspace_json("gone", &[], "2")],"archivedSessionIds":[]
    })))))
    .unwrap();
    bench.pool.run_until(refresh);
    bench
        .manager
        .handle_host_frame(WorkspaceHostFrame::Changed(workspace("gone", &[], "3")));
    flush_manager(&mut bench);
    assert!(bench.manager.snapshot().items.is_empty());
}

#[test]
fn overlapping_rejected_reorders_roll_back_only_the_latest_to_committed_order() {
    let mut bench = manager_bench();
    bench.transport.push(
        "workspace.list",
        ClientRpcResult::Success(Some(json!({
            "items":[
                workspace_json("one", &[], "1"),workspace_json("two", &[], "2"),
                workspace_json("three", &[], "3")
            ],"archivedSessionIds":[]
        }))),
    );
    let refresh = bench.manager.refresh();
    bench.pool.run_until(refresh);
    let first_gate = bench.transport.gate("workspace.insertBefore");
    let second_gate = bench.transport.gate("workspace.insertBefore");
    let one = WorkspaceId::new("one");
    let two = WorkspaceId::new("two");
    let three = WorkspaceId::new("three");
    let first = bench.manager.insert_before(&three, Some(&one));
    let second = bench.manager.insert_before(&two, Some(&three));
    assert_eq!(
        bench
            .manager
            .snapshot()
            .items
            .iter()
            .map(|item| item.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["two", "three", "one"]
    );
    first_gate
        .send(Ok(failure("workspace-not-found", "first")))
        .unwrap();
    assert!(matches!(
        bench.pool.run_until(first),
        Ok(ClientRpcResult::Failure(_))
    ));
    assert_eq!(
        bench
            .manager
            .snapshot()
            .items
            .iter()
            .map(|item| item.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["two", "three", "one"]
    );
    second_gate
        .send(Ok(failure("workspace-not-found", "second")))
        .unwrap();
    assert!(matches!(
        bench.pool.run_until(second),
        Ok(ClientRpcResult::Failure(_))
    ));
    assert_eq!(
        bench
            .manager
            .snapshot()
            .items
            .iter()
            .map(|item| item.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two", "three"]
    );
}

#[test]
fn runtime_projects_readiness_recency_and_reuses_only_visible_member_blanks() {
    let mut bench = runtime_bench();
    bench.transport.push(
        "workspace.list",
        ClientRpcResult::Success(Some(json!({
            "items":[workspace_json("first", &[], "3"),workspace_json("active", &["s"], "1")],
            "archivedSessionIds":[]
        }))),
    );
    bench.pool.run_until(bench.workspaces.refresh());
    flush_runtime(&mut bench);
    assert!(!bench.workspaces.list_snapshot().baselines_ready);
    bench
        .session_manager
        .handle_host_frame(ManagerHostFrame::Added(summary(
            "s",
            Some("/w/active"),
            9,
            true,
        )));
    bench
        .session_manager
        .handle_host_frame(ManagerHostFrame::Added(summary(
            "stray",
            Some("/w/active"),
            10,
            true,
        )));
    bench.scheduler.flush();
    bench.transport.push(
        "session.list",
        ClientRpcResult::Success(Some(json!({"items":[
            {"sessionId":"s","updatedAt":9,"running":false,"blank":true,"cwd":"/w/active"},
            {"sessionId":"stray","updatedAt":10,"running":false,"blank":true,"cwd":"/w/active"}
        ]}))),
    );
    bench.pool.run_until(bench.sessions.refresh());
    flush_runtime(&mut bench);
    assert!(bench.workspaces.list_snapshot().baselines_ready);
    assert_eq!(
        bench
            .workspaces
            .list_snapshot()
            .recent_workspace_id
            .as_ref()
            .map(WorkspaceId::as_str),
        Some("active")
    );
    assert_eq!(
        bench
            .pool
            .run_until(
                bench
                    .workspaces
                    .connect_workspace(&WorkspaceId::new("active"))
            )
            .unwrap()
            .as_str(),
        "s"
    );
    assert!(bench.transport.calls("session.create").is_empty());
}

#[test]
fn runtime_connect_coalesces_creation_rejects_unknown_and_skips_archived_blanks() {
    let mut bench = runtime_bench();
    bench.transport.push(
        "workspace.list",
        ClientRpcResult::Success(Some(json!({
            "items":[workspace_json("alpha", &["blank"], "1")],
            "archivedSessionIds":["blank"]
        }))),
    );
    bench.pool.run_until(bench.workspaces.refresh());
    bench
        .session_manager
        .handle_host_frame(ManagerHostFrame::Added(summary(
            "blank",
            Some("/w/alpha"),
            1,
            true,
        )));
    flush_runtime(&mut bench);
    let gate = bench.transport.gate("session.create");
    let first = bench
        .workspaces
        .connect_workspace(&WorkspaceId::new("alpha"));
    let second = bench
        .workspaces
        .connect_workspace(&WorkspaceId::new("alpha"));
    gate.send(Ok(ClientRpcResult::Success(Some(
        json!({"sessionId":"fresh"}),
    ))))
    .unwrap();
    let (left, right) = bench.pool.run_until(futures::future::join(first, second));
    assert_eq!(left.unwrap().as_str(), "fresh");
    assert_eq!(right.unwrap().as_str(), "fresh");
    assert_eq!(bench.transport.calls("session.create").len(), 1);
    assert!(
        bench
            .pool
            .run_until(
                bench
                    .workspaces
                    .connect_workspace(&WorkspaceId::new("ghost"))
            )
            .is_err()
    );
}

#[test]
fn runtime_structured_create_picker_and_browse_failures_preserve_business_codes() {
    let mut bench = runtime_bench();
    bench.transport.push(
        "workspace.create",
        ClientRpcResult::Success(Some(json!({
            "workspace":workspace_json("picked", &[], "1"),"created":true
        }))),
    );
    let created = bench
        .pool
        .run_until(bench.workspaces.create(WorkspaceCreateInput {
            path: "/w/picked".to_owned(),
        }))
        .unwrap();
    assert_eq!(created.workspace_id.as_str(), "picked");
    bench.transport.push(
        "workspace.create",
        failure("workspace-invalid-path", "missing"),
    );
    let error = bench
        .pool
        .run_until(bench.workspaces.create(WorkspaceCreateInput {
            path: "/missing".to_owned(),
        }))
        .unwrap_err();
    assert_eq!(error.rpc_error.code, "workspace-invalid-path");
    bench.transport.push(
        "host.pickDirectory",
        ClientRpcResult::Success(Some(json!({"path":"/w/picked"}))),
    );
    assert_eq!(
        bench
            .pool
            .run_until(bench.workspaces.pick_directory())
            .unwrap(),
        Some("/w/picked".to_owned())
    );
    bench.transport.push(
        "host.listDirectory",
        failure("directory-unreadable", "denied"),
    );
    let error = bench
        .pool
        .run_until(bench.workspaces.list_directory(Some("/x")))
        .unwrap_err();
    assert!(matches!(
        error,
        DirectoryBrowseCallFailure::Business(error) if error.rpc_error.code == "directory-unreadable"
    ));
}

#[test]
fn runtime_actions_install_unary_echoes_and_archive_clears_only_current() {
    let mut bench = runtime_bench();
    bench.transport.push(
        "workspace.list",
        ClientRpcResult::Success(Some(json!({
            "items":[workspace_json("one", &["current","idle"], "1")],
            "archivedSessionIds":[]
        }))),
    );
    bench.pool.run_until(bench.workspaces.refresh());
    for id in ["current", "idle"] {
        bench
            .session_manager
            .handle_host_frame(ManagerHostFrame::Added(summary(id, None, 1, false)));
    }
    flush_runtime(&mut bench);
    bench.sessions.open(&SessionId::new("current")).unwrap();
    bench.scheduler.flush();
    let mut renamed_view = workspace_json("one", &["current", "idle"], "2");
    renamed_view["title"] = Value::String("renamed".to_owned());
    bench.transport.push(
        "workspace.rename",
        ClientRpcResult::Success(Some(json!({"workspace":renamed_view}))),
    );
    let renamed = bench
        .pool
        .run_until(bench.workspaces.rename(&WorkspaceId::new("one"), "renamed"))
        .unwrap();
    assert_eq!(renamed.title, "renamed");
    bench
        .pool
        .run_until(bench.workspaces.archive_session(&SessionId::new("idle")))
        .unwrap();
    flush_runtime(&mut bench);
    assert_eq!(
        bench
            .sessions
            .list_snapshot()
            .current
            .as_ref()
            .map(SessionId::as_str),
        Some("current")
    );
    bench.transport.push(
        "workspace.archiveSession",
        ClientRpcResult::Success(Some(json!({"archivedSessionIds":["idle","current"]}))),
    );
    bench
        .pool
        .run_until(bench.workspaces.archive_session(&SessionId::new("current")))
        .unwrap();
    flush_runtime(&mut bench);
    assert!(bench.sessions.list_snapshot().current.is_none());
}

#[test]
fn initial_selection_waits_for_both_baselines_opens_once_and_retries_after_failure() {
    let mut bench = runtime_bench();
    let _stop = bench.workspaces.start_initial_selection().unwrap();
    assert!(bench.workspaces.start_initial_selection().is_err());
    bench.transport.push(
        "workspace.list",
        ClientRpcResult::Success(Some(json!({
            "items":[workspace_json("recent", &[], "2")],"archivedSessionIds":[]
        }))),
    );
    bench
        .transport
        .push("session.create", failure("internal", "attach exploded"));
    bench.pool.run_until(bench.workspaces.refresh());
    bench.pool.run_until(bench.sessions.refresh());
    flush_runtime(&mut bench);
    assert!(bench.sessions.list_snapshot().current.is_none());
    assert_eq!(bench.transport.calls("session.create").len(), 1);
    assert_eq!(bench.reports.borrow().len(), 1);
    bench.transport.push(
        "workspace.list",
        ClientRpcResult::Success(Some(json!({
            "items":[workspace_json("recent", &[], "2")],"archivedSessionIds":[]
        }))),
    );
    bench.transport.push(
        "session.create",
        ClientRpcResult::Success(Some(json!({"sessionId":"retry"}))),
    );
    bench.pool.run_until(bench.workspaces.refresh());
    flush_runtime(&mut bench);
    assert_eq!(
        bench
            .sessions
            .list_snapshot()
            .current
            .as_ref()
            .map(SessionId::as_str),
        Some("retry")
    );
}

#[test]
fn remote_archive_frame_survives_stale_baseline_and_new_session_without_workspaces_clears() {
    let mut bench = runtime_bench();
    bench
        .session_manager
        .handle_host_frame(ManagerHostFrame::Added(summary("open", None, 1, false)));
    flush_runtime(&mut bench);
    bench.sessions.open(&SessionId::new("open")).unwrap();
    let gate = bench.transport.gate("workspace.list");
    let refresh = bench.workspaces.refresh();
    bench
        .workspaces
        .handle_host_frame(WorkspaceHostFrame::ArchivedSessionsChanged(Rc::new(vec![
            SessionId::new("open"),
        ])));
    flush_runtime(&mut bench);
    assert!(bench.sessions.list_snapshot().current.is_none());
    gate.send(Ok(ClientRpcResult::Success(Some(json!({
        "items":[],"archivedSessionIds":[]
    })))))
    .unwrap();
    bench.pool.run_until(refresh);
    flush_runtime(&mut bench);
    assert_eq!(
        bench
            .workspaces
            .list_snapshot()
            .archived_session_ids
            .as_ref(),
        &[SessionId::new("open")]
    );
    bench.workspaces.start_session(None);
    assert!(bench.sessions.list_snapshot().current.is_none());
}
