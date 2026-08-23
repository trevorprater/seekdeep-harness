//! Session manager instance, list, buffering, projection, job, and reminder parity.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use futures::{FutureExt, channel::oneshot, executor::LocalPool, task::LocalSpawnExt};
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerViewDefinitions, ClientRpcError, ClientRpcResult,
    ConversationNodeAssembler, ManagerHostFrame, ManagerMuxEnvelope, ManagerMuxFrame,
    ManagerSessionSummary, NotifierScheduler, QueueItemInput, QueuePlacement, SessionHistoryEntry,
    SessionHistoryPage, SessionHistoryRequest, SessionListPhase, SessionListState, SessionManager,
    SessionManagerOptions, SessionMuxFrame, SessionTaskSpawner, SessionTransport,
    SessionTransportRequest,
};
use seekdeep_identity::{MessageId, RpcId, SessionId};
use serde_json::{Map, Value, json};

#[derive(Default)]
struct ManualScheduler {
    tasks: RefCell<VecDeque<Box<dyn FnOnce()>>>,
}

impl NotifierScheduler for ManualScheduler {
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

impl ManualScheduler {
    fn flush(&self) {
        while let Some(callback) = self.tasks.borrow_mut().pop_front() {
            callback();
        }
    }
}

struct PoolSpawner(futures::executor::LocalSpawner);

impl SessionTaskSpawner for PoolSpawner {
    fn spawn(&self, task: futures::future::LocalBoxFuture<'static, ()>) {
        self.0.spawn_local(task).unwrap();
    }
}

enum CallPlan {
    Ready(Result<ClientRpcResult<Value>, String>),
    Deferred(oneshot::Receiver<Result<ClientRpcResult<Value>, String>>),
}

#[derive(Default)]
struct Transport {
    calls: RefCell<Vec<SessionTransportRequest>>,
    plans: RefCell<VecDeque<CallPlan>>,
    history_calls: RefCell<u64>,
}

impl SessionTransport for Transport {
    fn history(
        &self,
        _request: SessionHistoryRequest,
    ) -> futures::future::LocalBoxFuture<'static, Result<ClientRpcResult<SessionHistoryPage>, String>>
    {
        *self.history_calls.borrow_mut() += 1;
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
        self.calls.borrow_mut().push(request);
        match self.plans.borrow_mut().pop_front().unwrap() {
            CallPlan::Ready(result) => futures::future::ready(result).boxed_local(),
            CallPlan::Deferred(receiver) => async move {
                receiver
                    .await
                    .unwrap_or_else(|_| Err("call sender dropped".to_owned()))
            }
            .boxed_local(),
        }
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

fn manager(
    transport: Rc<Transport>,
    scheduler: Rc<ManualScheduler>,
    pool: &LocalPool,
) -> Rc<SessionManager> {
    SessionManager::new(
        transport,
        None,
        SessionManagerOptions {
            scheduler,
            spawner: Rc::new(PoolSpawner(pool.spawner())),
            resolve_time_zone: Rc::new(|| Ok("UTC".to_owned())),
            create_conversation: Rc::new(|| {
                ConversationNodeAssembler::new(Rc::new(EmptyEvents), Rc::new(EmptyViews))
            }),
            clock: Rc::new(|| 1_700_000_000_000),
            report: Rc::new(|_| {}),
        },
    )
}

fn summary(id: &str, running: bool, blank: bool) -> ManagerSessionSummary {
    ManagerSessionSummary {
        session_id: SessionId::new(id),
        updated_at: 1,
        running,
        blank,
        parent_session_id: None,
        origin: None,
        cwd: None,
        agent_preset: None,
        projections: None,
    }
}

fn list_value(items: &[ManagerSessionSummary]) -> Value {
    json!({"items":items.iter().map(|item| json!({
        "sessionId":item.session_id.as_str(),
        "updatedAt":item.updated_at,
        "running":item.running,
        "blank":item.blank,
        "parentSessionId":item.parent_session_id.as_ref().map(SessionId::as_str),
        "origin":item.origin,
        "cwd":item.cwd,
        "agentPreset":item.agent_preset
    })).collect::<Vec<_>>()})
}

fn approval(id: &str) -> SessionMuxFrame {
    SessionMuxFrame::ApprovalRequested {
        payload: json!({"approvalId":id}),
    }
}

fn queue(text: &str) -> SessionMuxFrame {
    SessionMuxFrame::Queue(vec![QueueItemInput {
        id: MessageId::new(format!("item-{text}")),
        message_id: MessageId::new(format!("message-{text}")),
        placement: QueuePlacement::Queued,
        content: vec![json!({"type":"text","text":text})],
    }])
}

#[test]
fn lazy_instances_replay_compacted_answerable_and_latest_queue_frames_then_sync_summary_bits() {
    let pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let manager = manager(Rc::new(Transport::default()), scheduler, &pool);
    manager.handle_host_frame(ManagerHostFrame::Added(summary("s1", true, false)));
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("approval"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(approval("a1")),
    });
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("queue-1"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(queue("old")),
    });
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("queue-2"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(queue("new")),
    });
    let first = manager.get(&SessionId::new("s1"));
    let second = manager.get(&SessionId::new("s1"));
    assert!(Rc::ptr_eq(&first, &second));
    let snapshot = first.snapshot();
    assert!(snapshot.running);
    assert!(!snapshot.blank);
    assert_eq!(snapshot.pending.len(), 1);
    assert_eq!(snapshot.queue.len(), 1);
    assert_eq!(snapshot.queue[0].preview, "new");
}

#[test]
fn resolutions_compact_uninstantiated_buffers_and_disconnect_drops_generation_waits_only() {
    let pool = LocalPool::new();
    let manager = manager(
        Rc::new(Transport::default()),
        Rc::new(ManualScheduler::default()),
        &pool,
    );
    manager.handle_host_frame(ManagerHostFrame::Added(summary("s1", false, false)));
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("approval"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(approval("a1")),
    });
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("queue"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(queue("kept")),
    });
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("resolved"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(SessionMuxFrame::ApprovalResolved {
            approval_id: "a1".to_owned(),
        }),
    });
    manager.handle_disconnected();
    let session = manager.get(&SessionId::new("s1"));
    assert!(session.snapshot().pending.is_empty());
    assert_eq!(session.snapshot().queue.len(), 1);
}

#[test]
fn list_refresh_single_flights_replays_inflight_mutations_and_preserves_established_order() {
    let mut pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let transport = Rc::new(Transport::default());
    let (sender, receiver) = oneshot::channel();
    transport
        .plans
        .borrow_mut()
        .push_back(CallPlan::Deferred(receiver));
    transport
        .plans
        .borrow_mut()
        .push_back(CallPlan::Ready(Ok(ClientRpcResult::Success(Some(
            list_value(&[
                summary("s2", false, false),
                summary("s1", false, false),
                summary("s4", false, false),
            ]),
        )))));
    let manager = manager(transport.clone(), scheduler, &pool);
    let first = manager.refresh_list();
    let second = manager.refresh_list();
    assert_eq!(manager.snapshot().state, SessionListState::Loading);
    pool.spawner().spawn_local(first).unwrap();
    pool.spawner().spawn_local(second).unwrap();
    pool.run_until_stalled();
    manager.handle_host_frame(ManagerHostFrame::Added(summary("s3", false, false)));
    manager.handle_host_frame(ManagerHostFrame::Status {
        session_id: SessionId::new("s1"),
        running: true,
    });
    assert!(
        sender
            .send(Ok(ClientRpcResult::Success(Some(list_value(&[
                summary("s1", false, false),
                summary("s2", false, false),
            ])))))
            .is_ok()
    );
    pool.run_until_stalled();
    assert_eq!(transport.calls.borrow().len(), 1);
    assert_eq!(manager.snapshot().phase, SessionListPhase::Ready);
    assert_eq!(
        manager
            .snapshot()
            .items
            .iter()
            .map(|entry| entry.summary.session_id.as_str())
            .collect::<Vec<_>>(),
        ["s3", "s1", "s2"]
    );
    assert!(manager.snapshot().items[1].summary.running);

    pool.run_until(manager.refresh_list());
    assert_eq!(
        manager
            .snapshot()
            .items
            .iter()
            .map(|entry| entry.summary.session_id.as_str())
            .collect::<Vec<_>>(),
        ["s1", "s2", "s4"]
    );
}

#[test]
fn list_failure_keeps_ready_phase_monotonic_and_pushes_bits_to_existing_sessions() {
    let mut pool = LocalPool::new();
    let transport = Rc::new(Transport::default());
    transport.plans.borrow_mut().extend([
        CallPlan::Ready(Ok(ClientRpcResult::Success(Some(list_value(&[summary(
            "s1", false, true,
        )]))))),
        CallPlan::Ready(Ok(ClientRpcResult::Failure(ClientRpcError {
            code: "denied".to_owned(),
            message: "no".to_owned(),
            details: Map::new(),
        }))),
    ]);
    let manager = manager(transport, Rc::new(ManualScheduler::default()), &pool);
    let session = manager.get(&SessionId::new("s1"));
    pool.run_until(manager.refresh_list());
    assert!(session.snapshot().blank);
    manager.handle_host_frame(ManagerHostFrame::Status {
        session_id: SessionId::new("s1"),
        running: true,
    });
    assert!(!session.snapshot().blank);
    pool.run_until(manager.refresh_list());
    assert_eq!(manager.snapshot().phase, SessionListPhase::Ready);
    assert_eq!(manager.snapshot().state, SessionListState::Error);
    assert_eq!(manager.snapshot().error.as_ref().unwrap().code, "denied");
}

#[test]
fn projection_and_job_mirrors_outlive_instantiation_truncate_on_subscribe_and_drop_on_remove() {
    let pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let manager = manager(Rc::new(Transport::default()), scheduler, &pool);
    manager.handle_host_frame(ManagerHostFrame::Added(summary("s1", false, false)));
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("projection"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Projection {
            key: "title".to_owned(),
            value: Rc::new(json!("Live")),
            seq: 9,
        },
    });
    let before = manager.snapshot().items[0].clone();
    assert_eq!(before.summary.title.as_deref(), Some("Live"));
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("stale"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Projection {
            key: "title".to_owned(),
            value: Rc::new(json!("Stale")),
            seq: 5,
        },
    });
    assert!(Rc::ptr_eq(&before, &manager.snapshot().items[0]));
    let session = manager.get(&SessionId::new("s1"));
    assert_eq!(
        session.projections().get("title").as_deref(),
        Some(&json!("Live"))
    );

    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("jobs"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Jobs(vec![json!({"id":"job"})]),
    });
    assert_eq!(
        manager.snapshot().jobs_by_session[&SessionId::new("s1")].len(),
        1
    );
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("subscribed"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(SessionMuxFrame::Subscribed { last_seq: 2 }),
    });
    assert!(session.projections().get("title").is_none());
    assert!(
        !manager
            .snapshot()
            .jobs_by_session
            .contains_key(&SessionId::new("s1"))
    );
    manager.handle_host_frame(ManagerHostFrame::Removed {
        session_id: SessionId::new("s1"),
    });
    assert!(manager.snapshot().items.is_empty());
    assert!(session.snapshot().removed);
}

#[test]
fn question_priority_plan_review_classification_and_generation_cleanup_match_answer_order() {
    let pool = LocalPool::new();
    let manager = manager(
        Rc::new(Transport::default()),
        Rc::new(ManualScheduler::default()),
        &pool,
    );
    manager.handle_host_frame(ManagerHostFrame::Added(summary("s1", false, false)));
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("approval"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(approval("a1")),
    });
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("question"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(SessionMuxFrame::QuestionRequested {
            payload: json!({"questions":[{
                "question":"Ship?",
                "detail":"Review",
                "intent":{"kind":"plan-review","approve":"Approve"},
                "options":[{"label":"Approve"},{"label":"Reject"}]
            }]}),
        }),
    });
    assert_eq!(
        manager.snapshot().items[0].pending_interaction,
        Some(json!("plan-review"))
    );
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("resolved"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(SessionMuxFrame::QuestionResolved {
            question_rpc_id: RpcId::new("question"),
        }),
    });
    assert_eq!(
        manager.snapshot().items[0].pending_interaction,
        Some(json!("approval"))
    );
    manager.handle_disconnected();
    assert_eq!(manager.snapshot().items[0].pending_interaction, None);
    assert!(
        manager
            .get(&SessionId::new("s1"))
            .snapshot()
            .pending
            .is_empty()
    );
}

#[test]
fn completion_reminders_arm_only_on_unwatched_running_to_idle_edges() {
    let pool = LocalPool::new();
    let manager = manager(
        Rc::new(Transport::default()),
        Rc::new(ManualScheduler::default()),
        &pool,
    );
    manager.handle_host_frame(ManagerHostFrame::Added(summary("s1", false, false)));
    manager.handle_host_frame(ManagerHostFrame::Added(summary("s2", false, false)));
    manager.handle_host_frame(ManagerHostFrame::Status {
        session_id: SessionId::new("s2"),
        running: true,
    });
    manager.handle_host_frame(ManagerHostFrame::Status {
        session_id: SessionId::new("s2"),
        running: false,
    });
    let completed_snapshot = manager.snapshot();
    let s2 = completed_snapshot
        .items
        .iter()
        .find(|entry| entry.summary.session_id.as_str() == "s2")
        .unwrap();
    assert!(s2.completed);
    manager.select(&SessionId::new("s2")).unwrap();
    assert!(
        !manager
            .snapshot()
            .items
            .iter()
            .find(|entry| entry.summary.session_id.as_str() == "s2")
            .unwrap()
            .completed
    );
    manager.handle_host_frame(ManagerHostFrame::Status {
        session_id: SessionId::new("s2"),
        running: true,
    });
    manager.handle_host_frame(ManagerHostFrame::Status {
        session_id: SessionId::new("s2"),
        running: false,
    });
    assert!(
        !manager
            .snapshot()
            .items
            .iter()
            .find(|entry| entry.summary.session_id.as_str() == "s2")
            .unwrap()
            .completed
    );
    manager.clear_selection();
    manager.handle_host_frame(ManagerHostFrame::Status {
        session_id: SessionId::new("s2"),
        running: true,
    });
    manager.handle_host_frame(ManagerHostFrame::Status {
        session_id: SessionId::new("s2"),
        running: false,
    });
    assert!(
        manager
            .snapshot()
            .items
            .iter()
            .find(|entry| entry.summary.session_id.as_str() == "s2")
            .unwrap()
            .completed
    );
}

#[test]
fn only_direct_user_messages_advance_list_activity_and_subscribers_unsubscribe() {
    let pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let manager = manager(Rc::new(Transport::default()), scheduler.clone(), &pool);
    manager.handle_host_frame(ManagerHostFrame::Added(summary("s1", false, false)));
    let ticks = Rc::new(std::cell::Cell::new(0));
    let observed = ticks.clone();
    let subscription = manager.subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("user"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(SessionMuxFrame::Event(SessionHistoryEntry {
            event: seekdeep_client_runtime::ConversationLocationEvent::with_time(
                1,
                100,
                "user/message",
                json!({"source":{"kind":"user"}}),
            ),
            view: None,
        })),
    });
    manager.handle_mux_envelope(ManagerMuxEnvelope {
        rpc_id: RpcId::new("system"),
        session_id: SessionId::new("s1"),
        frame: ManagerMuxFrame::Session(SessionMuxFrame::Event(SessionHistoryEntry {
            event: seekdeep_client_runtime::ConversationLocationEvent::with_time(
                2,
                200,
                "user/message",
                json!({"source":{"kind":"system"}}),
            ),
            view: None,
        })),
    });
    scheduler.flush();
    assert_eq!(manager.snapshot().items[0].summary.updated_at, 100);
    assert_eq!(ticks.get(), 1);
    subscription.dispose();
    manager.handle_host_frame(ManagerHostFrame::Status {
        session_id: SessionId::new("s1"),
        running: true,
    });
    scheduler.flush();
    assert_eq!(ticks.get(), 1);
}

#[test]
fn connected_generation_refreshes_list_and_resyncs_only_opened_instances() {
    let mut pool = LocalPool::new();
    let transport = Rc::new(Transport::default());
    transport
        .plans
        .borrow_mut()
        .push_back(CallPlan::Ready(Ok(ClientRpcResult::Success(Some(
            list_value(&[summary("cold", false, false), summary("open", false, false)]),
        )))));
    let manager = manager(
        transport.clone(),
        Rc::new(ManualScheduler::default()),
        &pool,
    );
    manager.handle_host_frame(ManagerHostFrame::Added(summary("cold", false, false)));
    manager.handle_host_frame(ManagerHostFrame::Added(summary("open", false, false)));
    let _cold = manager.get(&SessionId::new("cold"));
    let opened = manager.get(&SessionId::new("open"));
    pool.run_until(opened.open());
    assert_eq!(*transport.history_calls.borrow(), 1);
    manager.handle_connected();
    pool.run_until_stalled();
    assert_eq!(transport.calls.borrow().len(), 1);
    assert_eq!(transport.calls.borrow()[0].method, "session.list");
    assert_eq!(*transport.history_calls.borrow(), 2);
}
