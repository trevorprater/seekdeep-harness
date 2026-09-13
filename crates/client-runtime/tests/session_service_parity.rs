//! Root Sessions service list, selection, scope, provide, create, and fork parity.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use futures::{FutureExt, executor::LocalPool, task::LocalSpawnExt};
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerViewDefinitions, ClientRpcError, ClientRpcResult,
    ConversationNodeAssembler, ManagerHostFrame, ManagerSessionSummary, NotifierScheduler,
    RuntimeDisposer, RuntimeSessionScope, SessionHistoryPage, SessionHistoryRequest,
    SessionManager, SessionManagerOptions, SessionManagerTimer, SessionProvideContribution,
    SessionProvideDescriptor, SessionProvideError, SessionRuntime, SessionRuntimeOptions,
    SessionScopeFactory, SessionSelection, SessionSelectionStorage, SessionTaskSpawner,
    SessionTransport, SessionTransportRequest, SubagentAddress, SubagentMode, workspace_title_of,
};
use seekdeep_identity::SessionId;
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
        loop {
            let callback = self.tasks.borrow_mut().pop_front();
            let Some(callback) = callback else {
                break;
            };
            callback();
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

#[derive(Default)]
struct Transport {
    calls: RefCell<Vec<SessionTransportRequest>>,
    results: RefCell<VecDeque<Result<ClientRpcResult<Value>, String>>>,
    history_calls: Cell<u64>,
}

impl SessionTransport for Transport {
    fn history(
        &self,
        _request: SessionHistoryRequest,
    ) -> futures::future::LocalBoxFuture<'static, Result<ClientRpcResult<SessionHistoryPage>, String>>
    {
        self.history_calls.set(self.history_calls.get() + 1);
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
    ) -> futures::future::LocalBoxFuture<'static, Result<ClientRpcResult<Value>, String>> {
        let method = request.method.clone();
        self.calls.borrow_mut().push(request);
        let result = self.results.borrow_mut().pop_front().unwrap_or_else(|| {
            Ok(ClientRpcResult::Success(Some(
                if method == "subagent.list" {
                    json!({"entries":[],"parentAvailable":false})
                } else {
                    Value::Null
                },
            )))
        });
        futures::future::ready(result).boxed_local()
    }
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
struct Storage {
    value: RefCell<SessionSelection>,
    stores: Cell<u64>,
    clears: Cell<u64>,
}

impl SessionSelectionStorage for Storage {
    fn load(&self) -> SessionSelection {
        self.value.borrow().clone()
    }

    fn store(&self, selection: &SessionSelection) {
        *self.value.borrow_mut() = selection.clone();
        self.stores.set(self.stores.get() + 1);
    }

    fn clear(&self) {
        *self.value.borrow_mut() = SessionSelection::default();
        self.clears.set(self.clears.get() + 1);
    }
}

#[derive(Default)]
struct Scopes {
    created: Rc<RefCell<Vec<SessionId>>>,
    disposed: Rc<RefCell<Vec<SessionId>>>,
}

impl SessionScopeFactory for Scopes {
    fn create(&self, session_id: &SessionId) -> Rc<RuntimeSessionScope> {
        self.created.borrow_mut().push(session_id.clone());
        let disposed = self.disposed.clone();
        let id = session_id.clone();
        RuntimeSessionScope::new(
            session_id.clone(),
            json!({"sessionId":session_id.as_str()}),
            RuntimeDisposer::new(move || disposed.borrow_mut().push(id)),
        )
    }
}

fn summary(id: &str, cwd: Option<&str>) -> ManagerSessionSummary {
    ManagerSessionSummary {
        session_id: SessionId::new(id),
        updated_at: 1,
        running: false,
        blank: false,
        parent_session_id: None,
        origin: None,
        cwd: cwd.map(ToOwned::to_owned),
        agent_preset: None,
        projections: None,
    }
}

struct Bench {
    pool: LocalPool,
    scheduler: Rc<Scheduler>,
    transport: Rc<Transport>,
    manager: Rc<SessionManager>,
    runtime: Rc<SessionRuntime>,
    storage: Rc<Storage>,
    scopes: Rc<Scopes>,
    pruned: Rc<RefCell<Vec<SessionId>>>,
}

#[allow(clippy::needless_pass_by_value)]
fn bench(restored: SessionSelection) -> Bench {
    let pool = LocalPool::new();
    let scheduler = Rc::new(Scheduler::default());
    let transport = Rc::new(Transport::default());
    let storage = Rc::new(Storage::default());
    *storage.value.borrow_mut() = restored.clone();
    let manager = SessionManager::new(
        transport.clone(),
        restored.session_id.clone(),
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
    let scopes = Rc::new(Scopes::default());
    let pruned = Rc::new(RefCell::new(Vec::new()));
    let observed = pruned.clone();
    let runtime = SessionRuntime::new(
        &manager,
        SessionRuntimeOptions {
            selection: storage.clone(),
            scopes: scopes.clone(),
            spawner: Rc::new(Spawner(pool.spawner())),
            prune_store_scope: Rc::new(move |id| observed.borrow_mut().push(id.clone())),
        },
    );
    Bench {
        pool,
        scheduler,
        transport,
        manager,
        runtime,
        storage,
        scopes,
        pruned,
    }
}

fn flush(bench: &mut Bench) {
    bench.scheduler.flush();
    bench.pool.run_until_stalled();
    bench.scheduler.flush();
}

#[test]
fn projects_titles_fallbacks_parent_links_and_preset_only_changes() {
    let mut bench = bench(SessionSelection::default());
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary(
            "path",
            Some("/work/project/"),
        )));
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(ManagerSessionSummary {
            parent_session_id: Some(SessionId::new("path")),
            ..summary("child", Some("////"))
        }));
    flush(&mut bench);
    let list = bench.runtime.list_snapshot();
    assert_eq!(list.by_id[&SessionId::new("path")].display_title, "project");
    assert_eq!(list.by_id[&SessionId::new("child")].display_title, "child");
    assert_eq!(
        list.by_id[&SessionId::new("child")]
            .parent_id
            .as_ref()
            .map(SessionId::as_str),
        Some("path")
    );
    bench
        .manager
        .note_agent_preset(&SessionId::new("path"), "preset");
    flush(&mut bench);
    assert_eq!(
        bench.runtime.list_snapshot().by_id[&SessionId::new("path")]
            .agent_preset
            .as_deref(),
        Some("preset")
    );
    assert_eq!(workspace_title_of("C:\\work\\name\\"), "name");
}

#[test]
fn binding_is_pure_and_stable_while_staging_alone_opens_history() {
    let mut bench = bench(SessionSelection::default());
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary("s1", None)));
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary("s2", None)));
    flush(&mut bench);
    let first = bench.runtime.binding(&SessionId::new("s1")).unwrap();
    let second = bench.runtime.binding(&SessionId::new("s1")).unwrap();
    assert!(Rc::ptr_eq(&first, &second));
    assert_eq!(bench.transport.history_calls.get(), 0);
    bench.runtime.open(&SessionId::new("s1")).unwrap();
    flush(&mut bench);
    assert_eq!(bench.transport.history_calls.get(), 1);
    bench.runtime.binding(&SessionId::new("s2")).unwrap();
    assert_eq!(bench.transport.history_calls.get(), 1);
    assert_eq!(bench.scopes.created.borrow().len(), 2);
}

#[test]
fn off_stage_removal_prunes_immediately_but_staged_removal_defers_until_stage_moves() {
    let mut bench = bench(SessionSelection::default());
    for id in ["s1", "s2"] {
        bench
            .manager
            .handle_host_frame(ManagerHostFrame::Added(summary(id, None)));
    }
    flush(&mut bench);
    bench.runtime.binding(&SessionId::new("s1")).unwrap();
    bench.runtime.binding(&SessionId::new("s2")).unwrap();
    bench.runtime.open(&SessionId::new("s1")).unwrap();
    flush(&mut bench);
    bench.manager.handle_host_frame(ManagerHostFrame::Removed {
        session_id: SessionId::new("s2"),
    });
    flush(&mut bench);
    assert!(bench.runtime.binding(&SessionId::new("s2")).is_none());
    assert!(
        bench
            .scopes
            .disposed
            .borrow()
            .iter()
            .any(|id| id.as_str() == "s2")
    );
    bench.manager.handle_host_frame(ManagerHostFrame::Removed {
        session_id: SessionId::new("s1"),
    });
    flush(&mut bench);
    assert!(bench.runtime.scope(&SessionId::new("s1")).is_some());
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary("s3", None)));
    flush(&mut bench);
    bench.runtime.open(&SessionId::new("s3")).unwrap();
    flush(&mut bench);
    assert!(bench.runtime.scope(&SessionId::new("s1")).is_none());
    assert!(bench.pruned.borrow().iter().any(|id| id.as_str() == "s1"));
}

#[test]
fn persisted_selection_rehydrates_after_list_projection_and_clear_wipes_it() {
    let mut bench = bench(SessionSelection {
        session_id: Some(SessionId::new("s1")),
        subagent_address: None,
    });
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary("s1", None)));
    flush(&mut bench);
    assert_eq!(
        bench
            .runtime
            .list_snapshot()
            .current
            .as_ref()
            .map(SessionId::as_str),
        Some("s1")
    );
    assert_eq!(bench.transport.history_calls.get(), 1);
    assert_eq!(
        bench
            .storage
            .value
            .borrow()
            .session_id
            .as_ref()
            .map(SessionId::as_str),
        Some("s1")
    );
    bench.runtime.clear();
    flush(&mut bench);
    assert!(bench.runtime.list_snapshot().current.is_none());
    assert!(bench.storage.value.borrow().session_id.is_none());
    assert!(bench.storage.clears.get() > 0);
}

#[test]
fn provider_roster_rebuilds_live_binding_and_republishes_stable_current() {
    let mut bench = bench(SessionSelection::default());
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary("s1", None)));
    flush(&mut bench);
    bench.runtime.open(&SessionId::new("s1")).unwrap();
    flush(&mut bench);
    let before = bench.runtime.current_provide_info();
    let ticks = Rc::new(Cell::new(0));
    let observed = ticks.clone();
    let _subscription = bench.runtime.subscribe_current_provide(Rc::new(move || {
        observed.set(observed.get() + 1);
        Ok(())
    }));
    let hook = bench
        .runtime
        .binding(&SessionId::new("s1"))
        .unwrap()
        .session
        .clone();
    let registration = bench
        .runtime
        .provide(SessionProvideDescriptor {
            hooks: vec!["extra".to_owned()],
            props: vec!["marker".to_owned()],
            resolve: Rc::new(move |_binding| {
                Ok(SessionProvideContribution {
                    hooks: indexmap::IndexMap::from([("extra".to_owned(), Some(hook.clone()))]),
                    props: indexmap::IndexMap::from([("marker".to_owned(), Some(json!(7)))]),
                })
            }),
        })
        .unwrap();
    let added = bench.runtime.current_provide_info();
    assert!(!Rc::ptr_eq(&before, &added));
    assert_eq!(added.props["marker"], Some(json!(7)));
    assert_eq!(ticks.get(), 1);
    registration.dispose().unwrap();
    assert_eq!(ticks.get(), 2);
}

#[test]
fn create_and_fork_resolve_with_rows_and_bindings_already_addressable() {
    let mut bench = bench(SessionSelection::default());
    bench.transport.results.borrow_mut().extend([
        Ok(ClientRpcResult::Success(Some(
            json!({"sessionId":"created"}),
        ))),
        Ok(ClientRpcResult::Success(Some(
            json!({"sessionId":"forked"}),
        ))),
    ]);
    let created = bench
        .pool
        .run_until(bench.runtime.create(json!({
            "sessionId":"created","cwd":"/created"
        })))
        .unwrap();
    assert_eq!(created.as_str(), "created");
    assert!(bench.runtime.binding(&created).is_some());
    let forked = bench
        .pool
        .run_until(bench.runtime.fork(&created, Some(4.9), false))
        .unwrap();
    assert_eq!(forked.as_str(), "forked");
    assert!(bench.runtime.binding(&forked).is_some());
    assert_eq!(bench.transport.calls.borrow()[1].payload["atSeq"], 4);

    let invalid_anchor = bench
        .pool
        .run_until(bench.runtime.fork(&created, Some(-1.0), false))
        .unwrap_err();
    assert_eq!(invalid_anchor.error.code, "bad-request");
    assert_eq!(
        invalid_anchor.kind,
        seekdeep_client_runtime::SessionForkFailureKind::Fork
    );
    assert_eq!(bench.transport.calls.borrow().len(), 2);

    bench
        .transport
        .results
        .borrow_mut()
        .push_back(Ok(ClientRpcResult::Failure(ClientRpcError {
            code: "denied".to_owned(),
            message: "no".to_owned(),
            details: serde_json::Map::new(),
        })));
    let error = bench
        .pool
        .run_until(bench.runtime.create(json!({"sessionId":"reserved"})))
        .unwrap_err();
    assert_eq!(
        error.requested_session_id.as_ref().map(SessionId::as_str),
        Some("reserved")
    );
}

#[test]
fn selection_storage_writes_only_when_the_persisted_route_changes() {
    let mut bench = bench(SessionSelection::default());
    assert_eq!(bench.storage.clears.get(), 0);
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary("s1", None)));
    flush(&mut bench);
    assert_eq!(bench.storage.clears.get(), 0);
    assert_eq!(bench.storage.stores.get(), 0);
    bench.runtime.open(&SessionId::new("s1")).unwrap();
    flush(&mut bench);
    assert_eq!(bench.storage.stores.get(), 1);
    bench
        .runtime
        .note_agent_preset(&SessionId::new("s1"), "minimal");
    flush(&mut bench);
    assert_eq!(bench.storage.stores.get(), 1);
    bench.runtime.clear();
    flush(&mut bench);
    assert_eq!(bench.storage.clears.get(), 1);
}

#[test]
fn catalog_methods_retain_addresses_and_override_listed_breadcrumb_titles() {
    let mut bench = bench(SessionSelection::default());
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary("root", None)));
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(ManagerSessionSummary {
            parent_session_id: Some(SessionId::new("root")),
            origin: Some("subagent".to_owned()),
            ..summary("child", Some("/listed-title"))
        }));
    flush(&mut bench);
    bench
        .transport
        .results
        .borrow_mut()
        .push_back(Ok(ClientRpcResult::Success(Some(json!({
            "entries":[{
                "kind":"child","id":"child","mode":"continuable",
                "label":"Catalog title","activity":"inactive","hasChildren":false
            }],
            "parentAvailable":true
        })))));
    bench
        .pool
        .run_until(bench.runtime.refresh_subagents(&SessionId::new("root")));
    flush(&mut bench);
    let address = SubagentAddress {
        parent_session_id: SessionId::new("root"),
        child_session_id: SessionId::new("child"),
        mode: SubagentMode::Continuable,
    };
    bench.runtime.open_subagent(address.clone()).unwrap();
    flush(&mut bench);
    assert_eq!(
        bench.runtime.list_snapshot().by_id[&SessionId::new("child")].display_title,
        "Catalog title"
    );
    assert_eq!(
        bench.runtime.subagent_address(&SessionId::new("child")),
        Some(address)
    );
    bench
        .runtime
        .set_subagent_catalog_open(&SessionId::new("root"), false);
}

#[test]
fn failed_lazy_provider_materialization_rolls_back_scope_and_session_binding() {
    let mut bench = bench(SessionSelection::default());
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary("s1", None)));
    flush(&mut bench);
    let registration = bench
        .runtime
        .provide(SessionProvideDescriptor {
            hooks: vec!["broken".to_owned()],
            props: Vec::new(),
            resolve: Rc::new(|_| Err(SessionProvideError::new("resolver failed"))),
        })
        .unwrap();
    assert!(bench.runtime.binding(&SessionId::new("s1")).is_none());
    assert_eq!(bench.scopes.created.borrow().len(), 1);
    assert_eq!(bench.scopes.disposed.borrow().len(), 1);
    registration.dispose().unwrap();
    assert!(bench.runtime.binding(&SessionId::new("s1")).is_some());
}

#[test]
fn list_listener_observes_the_matching_current_provide_bundle() {
    let mut bench = bench(SessionSelection::default());
    bench
        .manager
        .handle_host_frame(ManagerHostFrame::Added(summary("s1", None)));
    flush(&mut bench);
    let runtime = bench.runtime.clone();
    let coherent = Rc::new(Cell::new(false));
    let observed = coherent.clone();
    let _subscription = bench.runtime.subscribe(Rc::new(move || {
        let current = runtime.list_snapshot().current.clone();
        let provided = runtime.current_provide_info().session_id.clone();
        observed.set(current == provided);
    }));
    bench.runtime.open(&SessionId::new("s1")).unwrap();
    flush(&mut bench);
    assert!(coherent.get());
}
