//! Resident Session open, live repair, paging, mux, projection, and operation parity.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use futures::{FutureExt, channel::oneshot, executor::LocalPool, task::LocalSpawnExt};
use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ChatConversationViewMetadata,
    ClientRpcError, ClientRpcResult, ClientSession, ComposerPhase, ConversationAssemblerError,
    ConversationContextReader, ConversationLocationEvent, ConversationMatch,
    ConversationMatchResult, ConversationMatchRole, ConversationNodeAssembler,
    ConversationNodeContext, ConversationTimelineSnapshot, ConversationViewNode,
    ConversationVisibility, NotifierScheduler, ProjectionValueStore, ProjectionsBaseline,
    PromptOperation, QueueItemInput, QueuePlacement, SessionHistoryEntry, SessionHistoryPage,
    SessionHistoryRequest, SessionMuxFrame, SessionOpenState, SessionOptions, SessionTaskSpawner,
    SessionTransport, SessionTransportRequest, SubagentAddress, SubagentMode,
};
use seekdeep_identity::{MessageId, RpcId, SessionId};
use serde_json::{Map, Value, json};

#[derive(Default)]
struct ManualScheduler {
    microtasks: RefCell<VecDeque<Box<dyn FnOnce()>>>,
    frames: RefCell<VecDeque<Box<dyn FnOnce()>>>,
}

impl NotifierScheduler for ManualScheduler {
    fn has_animation_frame(&self) -> bool {
        true
    }

    fn queue_microtask(&self, callback: Box<dyn FnOnce()>) {
        self.microtasks.borrow_mut().push_back(callback);
    }

    fn queue_animation_frame(&self, callback: Box<dyn FnOnce()>) {
        self.frames.borrow_mut().push_back(callback);
    }
}

impl ManualScheduler {
    fn flush(&self) {
        while let Some(callback) = self.microtasks.borrow_mut().pop_front() {
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

enum HistoryPlan {
    Ready(Result<ClientRpcResult<SessionHistoryPage>, String>),
    Deferred(oneshot::Receiver<Result<ClientRpcResult<SessionHistoryPage>, String>>),
}

#[derive(Default)]
struct ScriptedTransport {
    histories: RefCell<VecDeque<HistoryPlan>>,
    history_requests: RefCell<Vec<SessionHistoryRequest>>,
    calls: RefCell<Vec<SessionTransportRequest>>,
    call_results: RefCell<VecDeque<Result<ClientRpcResult<Value>, String>>>,
}

impl SessionTransport for ScriptedTransport {
    fn history(
        &self,
        request: SessionHistoryRequest,
    ) -> futures::future::LocalBoxFuture<'static, Result<ClientRpcResult<SessionHistoryPage>, String>>
    {
        self.history_requests.borrow_mut().push(request);
        match self.histories.borrow_mut().pop_front().unwrap() {
            HistoryPlan::Ready(result) => futures::future::ready(result).boxed_local(),
            HistoryPlan::Deferred(receiver) => async move {
                receiver
                    .await
                    .unwrap_or_else(|_| Err("history sender dropped".to_owned()))
            }
            .boxed_local(),
        }
    }

    fn call(
        &self,
        request: SessionTransportRequest,
    ) -> futures::future::LocalBoxFuture<'static, Result<ClientRpcResult<Value>, String>> {
        self.calls.borrow_mut().push(request);
        let result = self
            .call_results
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Ok(ClientRpcResult::Success(Some(Value::Null))));
        futures::future::ready(result).boxed_local()
    }
}

struct Events(Vec<Rc<AssemblerNodeDefinition>>);

impl AssemblerEventDefinitions for Events {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        self.0.clone()
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        None
    }
}

struct Views(Vec<Rc<AssemblerViewDefinition>>);

impl AssemblerViewDefinitions for Views {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        self.0.clone()
    }
}

struct MessageBuilder {
    nodes: IndexMap<String, Rc<ConversationViewNode>>,
    order: Vec<String>,
}

impl MessageBuilder {
    fn snapshot(&self) -> Rc<Value> {
        let nodes = self
            .nodes
            .iter()
            .map(|(key, node)| (key.clone(), node.data.as_ref().clone()))
            .collect::<Map<_, _>>();
        Rc::new(json!({"order":self.order,"nodes":nodes}))
    }
}

impl AssemblerViewBuilder for MessageBuilder {
    fn empty(&self) -> Rc<Value> {
        self.snapshot()
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        self.nodes = nodes
            .iter()
            .map(|node| (node.key.clone(), node.clone()))
            .collect();
        self.order = nodes.iter().map(|node| node.key.clone()).collect();
        Ok(self.snapshot())
    }

    fn apply(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        for node in nodes {
            if !self.nodes.contains_key(&node.key) {
                self.order.push(node.key.clone());
            }
            self.nodes.insert(node.key.clone(), node.clone());
        }
        Ok(self.snapshot())
    }
}

fn conversation() -> ConversationNodeAssembler {
    let definition = Rc::new(AssemblerNodeDefinition {
        kind: "message".to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| {
            Ok(
                (event.event_type == "message").then(|| ConversationMatchResult {
                    id: event.seq.to_string(),
                    role: ConversationMatchRole::Start,
                }),
            )
        }),
        start: Rc::new(
            |_context: &ConversationNodeContext,
             accepted: &Rc<ConversationMatch>,
             _reader: &mut dyn ConversationContextReader| {
                Ok(Some(Rc::new(json!({
                    "seq":accepted.event.seq,
                    "view":accepted.view.as_deref().cloned()
                }))))
            },
        ),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            Ok(Some(Rc::new(ConversationViewNode {
                key: context.key.clone(),
                kind: context.kind.clone(),
                id: context.id.clone(),
                target: "chat".to_owned(),
                data: Rc::new(json!({
                    "kind":"message",
                    "seq":context.state.as_ref().unwrap()["seq"],
                    "view":context.state.as_ref().unwrap()["view"]
                })),
                chat: Some(ChatConversationViewMetadata {
                    anchor_seq: context.state.as_ref().unwrap()["seq"]
                        .as_f64()
                        .unwrap_or_default(),
                    location: context.start.as_ref().unwrap().location.clone(),
                    visibility: ConversationVisibility::Visible,
                }),
            })))
        })),
    });
    let view = Rc::new(AssemblerViewDefinition {
        target: "chat".to_owned(),
        create: Rc::new(|| {
            Box::new(MessageBuilder {
                nodes: IndexMap::new(),
                order: Vec::new(),
            })
        }),
    });
    ConversationNodeAssembler::new(
        Rc::new(Events(vec![definition])),
        Rc::new(Views(vec![view])),
    )
}

fn entry(seq: u64, event_type: &str, data: Value) -> SessionHistoryEntry {
    SessionHistoryEntry {
        event: ConversationLocationEvent::with_time(
            seq,
            1_700_000_000_000 + i64::try_from(seq).unwrap(),
            event_type,
            data,
        ),
        view: None,
    }
}

fn page(seqs: &[u64], has_more: bool) -> SessionHistoryPage {
    SessionHistoryPage {
        entries: seqs
            .iter()
            .map(|seq| entry(*seq, "message", json!({})))
            .collect(),
        has_more,
        projections: None,
    }
}

fn options(scheduler: Rc<ManualScheduler>, spawner: Rc<dyn SessionTaskSpawner>) -> SessionOptions {
    SessionOptions {
        address: None,
        parent_available: false,
        projections: None,
        conversation: Some(conversation()),
        scheduler,
        spawner,
        resolve_time_zone: Rc::new(|| Ok("UTC".to_owned())),
        on_engaged: None,
        report: Rc::new(|_| {}),
    }
}

fn successful_page(page: SessionHistoryPage) -> HistoryPlan {
    HistoryPlan::Ready(Ok(ClientRpcResult::Success(Some(page))))
}

fn chat_order(session: &ClientSession) -> Vec<String> {
    session.snapshot().chat.as_ref().unwrap()["order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

fn chat_seqs(session: &ClientSession) -> Vec<u64> {
    let snapshot = session.snapshot();
    let chat = snapshot.chat.as_ref().unwrap();
    chat["order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| {
            chat["nodes"][key.as_str().unwrap()]["seq"]
                .as_u64()
                .unwrap()
        })
        .collect()
}

#[test]
fn open_installs_tail_page_is_idempotent_and_preserves_business_or_transport_errors() {
    let mut pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let transport = Rc::new(ScriptedTransport::default());
    transport
        .histories
        .borrow_mut()
        .push_back(successful_page(page(&[1, 2], true)));
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport.clone(),
        options(scheduler, Rc::new(PoolSpawner(pool.spawner()))),
    );
    assert_eq!(session.snapshot().open_state, SessionOpenState::Cold);
    let open = session.open();
    assert_eq!(session.snapshot().open_state, SessionOpenState::Loading);
    pool.run_until(open);
    assert_eq!(session.snapshot().open_state, SessionOpenState::Open);
    assert!(session.snapshot().has_more);
    assert_eq!(chat_order(&session).len(), 2);
    pool.run_until(session.open());
    assert_eq!(transport.history_requests.borrow().len(), 1);

    let failure = Rc::new(ScriptedTransport::default());
    failure
        .histories
        .borrow_mut()
        .push_back(HistoryPlan::Ready(Ok(ClientRpcResult::Failure(
            ClientRpcError {
                code: "denied".to_owned(),
                message: "no".to_owned(),
                details: Map::new(),
            },
        ))));
    let failed = ClientSession::new(
        SessionId::new("failed"),
        failure,
        options(
            Rc::new(ManualScheduler::default()),
            Rc::new(PoolSpawner(pool.spawner())),
        ),
    );
    pool.run_until(failed.open());
    assert_eq!(failed.snapshot().open_state, SessionOpenState::Error);
    assert_eq!(
        failed.snapshot().open_error.as_ref().unwrap().code,
        "denied"
    );

    let thrown = Rc::new(ScriptedTransport::default());
    thrown
        .histories
        .borrow_mut()
        .push_back(HistoryPlan::Ready(Err("offline".to_owned())));
    let thrown_session = ClientSession::new(
        SessionId::new("thrown"),
        thrown,
        options(
            Rc::new(ManualScheduler::default()),
            Rc::new(PoolSpawner(pool.spawner())),
        ),
    );
    pool.run_until(thrown_session.open());
    assert_eq!(
        thrown_session.snapshot().open_error.as_ref().unwrap().code,
        "internal"
    );
}

#[test]
fn live_events_buffer_during_open_drop_overlap_and_repair_gaps_with_one_tail_pull() {
    let mut pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let transport = Rc::new(ScriptedTransport::default());
    let (sender, receiver) = oneshot::channel();
    transport
        .histories
        .borrow_mut()
        .push_back(HistoryPlan::Deferred(receiver));
    transport
        .histories
        .borrow_mut()
        .push_back(successful_page(page(&[1, 2, 3, 4], false)));
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport.clone(),
        options(scheduler.clone(), Rc::new(PoolSpawner(pool.spawner()))),
    );
    pool.spawner().spawn_local(session.open()).unwrap();
    pool.run_until_stalled();
    assert_eq!(session.snapshot().open_state, SessionOpenState::Loading);
    session.handle_mux_envelope(
        RpcId::new("live"),
        SessionMuxFrame::Event(entry(3, "message", json!({}))),
    );
    assert!(
        sender
            .send(Ok(ClientRpcResult::Success(Some(page(&[1, 2], false)))))
            .is_ok()
    );
    pool.run_until_stalled();
    assert_eq!(chat_order(&session).len(), 3);

    session.handle_mux_envelope(
        RpcId::new("overlap"),
        SessionMuxFrame::Event(entry(3, "message", json!({}))),
    );
    assert_eq!(chat_order(&session).len(), 3);
    session.handle_mux_envelope(
        RpcId::new("gap"),
        SessionMuxFrame::Event(entry(5, "message", json!({}))),
    );
    session.handle_mux_envelope(
        RpcId::new("gap-2"),
        SessionMuxFrame::Event(entry(6, "message", json!({}))),
    );
    pool.run_until_stalled();
    scheduler.flush();
    assert_eq!(transport.history_requests.borrow().len(), 2);
    assert_eq!(chat_order(&session).len(), 6);
}

#[test]
fn paging_prepends_contiguous_rows_and_drops_discontinuous_pages_fail_soft() {
    let mut pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let transport = Rc::new(ScriptedTransport::default());
    transport.histories.borrow_mut().extend([
        successful_page(page(&[3, 4], true)),
        successful_page(page(&[1, 2], true)),
        successful_page(page(&[8, 9], true)),
    ]);
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport.clone(),
        options(scheduler, Rc::new(PoolSpawner(pool.spawner()))),
    );
    pool.run_until(session.open());
    pool.run_until(session.load_older());
    assert_eq!(chat_order(&session).len(), 4);
    assert_eq!(transport.history_requests.borrow()[1].before_seq, Some(3));
    pool.run_until(session.load_older());
    assert_eq!(chat_order(&session).len(), 4);
    assert!(!session.snapshot().has_more);
}

#[test]
fn pending_queue_and_projection_baselines_follow_authoritative_mux_and_history() {
    let mut pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let transport = Rc::new(ScriptedTransport::default());
    let projection_scheduler = scheduler.clone();
    let projections = Rc::new(ProjectionValueStore::new(projection_scheduler));
    let mut baseline = IndexMap::new();
    baseline.insert("title".to_owned(), Rc::new(json!("Baseline")));
    transport
        .histories
        .borrow_mut()
        .push_back(successful_page(SessionHistoryPage {
            entries: Vec::new(),
            has_more: false,
            projections: Some(ProjectionsBaseline {
                as_of_seq: 5,
                values: baseline,
            }),
        }));
    let mut session_options = options(scheduler, Rc::new(PoolSpawner(pool.spawner())));
    session_options.projections = Some(projections.clone());
    let session = ClientSession::new(SessionId::new("s1"), transport.clone(), session_options);
    pool.run_until(session.open());
    assert_eq!(
        projections.get("title").as_deref(),
        Some(&json!("Baseline"))
    );
    session.handle_mux_envelope(
        RpcId::new("a1"),
        SessionMuxFrame::ApprovalRequested {
            payload: json!({"approvalId":"approval-1","callId":"c"}),
        },
    );
    session.handle_mux_envelope(
        RpcId::new("q1"),
        SessionMuxFrame::QuestionRequested {
            payload: json!({"question":"Continue?"}),
        },
    );
    assert_eq!(session.snapshot().pending.len(), 2);
    let approval = session.snapshot().pending[0].clone();
    futures::executor::block_on(approval.respond(json!({"ok":true})).unwrap()).unwrap();
    assert_eq!(transport.calls.borrow().last().unwrap().method, "respond");
    assert_eq!(
        transport.calls.borrow().last().unwrap().payload["rpcId"],
        "a1"
    );
    session.handle_mux_envelope(
        RpcId::new("resolved"),
        SessionMuxFrame::ApprovalResolved {
            approval_id: "approval-1".to_owned(),
        },
    );
    session.handle_mux_envelope(
        RpcId::new("resolved-q"),
        SessionMuxFrame::QuestionResolved {
            question_rpc_id: RpcId::new("q1"),
        },
    );
    assert!(session.snapshot().pending.is_empty());
    assert!(approval.respond(Value::Null).is_err());

    session.handle_mux_envelope(
        RpcId::new("queue"),
        SessionMuxFrame::Queue(vec![QueueItemInput {
            id: MessageId::new("item"),
            message_id: MessageId::new("message"),
            placement: QueuePlacement::Queued,
            content: vec![json!({"type":"text","text":"hello"})],
        }]),
    );
    assert_eq!(session.snapshot().queue.len(), 1);
    session.handle_mux_envelope(
        RpcId::new("subscribed"),
        SessionMuxFrame::Subscribed { last_seq: 0 },
    );
    assert!(session.snapshot().queue.is_empty());
}

#[test]
fn prompt_cancel_attachment_rename_command_and_queue_operations_follow_transport_contracts() {
    let mut pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let transport = Rc::new(ScriptedTransport::default());
    transport.call_results.borrow_mut().extend([
        Ok(ClientRpcResult::Success(Some(json!({"accepted":true})))),
        Ok(ClientRpcResult::Failure(ClientRpcError {
            code: "stop-denied".to_owned(),
            message: "cannot stop".to_owned(),
            details: Map::new(),
        })),
        Ok(ClientRpcResult::Success(Some(json!({
            "attachment":{"attachmentId":"a","mediaType":"image/png"},
            "data":"AAE="
        })))),
        Ok(ClientRpcResult::Success(Some(
            json!({"title":"Renamed","seq":9}),
        ))),
        Ok(ClientRpcResult::Success(None)),
        Ok(ClientRpcResult::Success(Some(json!({"command":"ran"})))),
        Ok(ClientRpcResult::Success(Some(json!({"accepted":true})))),
    ]);
    let engaged = Rc::new(Cell::new(0));
    let observed = engaged.clone();
    let mut session_options = options(scheduler, Rc::new(PoolSpawner(pool.spawner())));
    session_options.on_engaged = Some(Rc::new(move |_| observed.set(observed.get() + 1)));
    let session = ClientSession::new(SessionId::new("s1"), transport.clone(), session_options);
    let prompt = session.prompt(vec![json!({"type":"text","text":"hi"})], "queue");
    assert_eq!(session.snapshot().composer_phase, ComposerPhase::Engaging);
    assert!(pool.run_until(prompt).is_ok());
    assert_eq!(engaged.get(), 1);
    assert!(!session.snapshot().blank);
    assert_eq!(transport.calls.borrow()[0].method, "session.prompt");
    assert_eq!(transport.calls.borrow()[0].payload["clientTimeZone"], "UTC");

    let cancel = pool.run_until(session.cancel());
    assert!(matches!(cancel, ClientRpcResult::Failure(_)));
    assert_eq!(
        session.snapshot().prompt_error.as_ref().unwrap().operation,
        PromptOperation::Stop
    );
    let attachment = pool.run_until(session.read_attachment("a"));
    let ClientRpcResult::Success(Some(attachment)) = attachment else {
        panic!("attachment did not decode")
    };
    assert_eq!(attachment.data, [0, 1]);
    assert_eq!(attachment.attachment["attachmentId"], "a");

    assert!(pool.run_until(session.rename("Renamed")).is_ok());
    assert_eq!(
        session.projections().get("title").as_deref(),
        Some(&json!("Renamed"))
    );
    assert_eq!(
        pool.run_until(session.command("/missing")),
        ClientRpcResult::Success(Some(json!({"matched":false})))
    );
    assert_eq!(
        pool.run_until(session.command("/run")),
        ClientRpcResult::Success(Some(json!({"matched":true})))
    );
    let queue_before = session.snapshot().queue.clone();
    assert!(
        pool.run_until(session.update_queue(&MessageId::new("item"), json!({"type":"remove"}),))
            .is_ok()
    );
    assert!(Rc::ptr_eq(&queue_before, &session.snapshot().queue));
}

#[test]
fn addressed_children_use_nonactivating_routes_and_enforce_one_shot_and_image_boundaries() {
    let mut pool = LocalPool::new();
    let transport = Rc::new(ScriptedTransport::default());
    transport
        .histories
        .borrow_mut()
        .push_back(successful_page(page(&[], false)));
    let mut one_shot_options = options(
        Rc::new(ManualScheduler::default()),
        Rc::new(PoolSpawner(pool.spawner())),
    );
    one_shot_options.address = Some(SubagentAddress {
        parent_session_id: SessionId::new("parent"),
        child_session_id: SessionId::new("child"),
        mode: SubagentMode::OneShot,
    });
    let one_shot = ClientSession::new(SessionId::new("child"), transport.clone(), one_shot_options);
    pool.run_until(one_shot.open());
    assert!(transport.history_requests.borrow()[0].address.is_some());
    let prompt =
        pool.run_until(one_shot.prompt(vec![json!({"type":"text","text":"again"})], "queue"));
    assert!(
        matches!(prompt, ClientRpcResult::Failure(ref error) if error.code=="subagent-not-resumable")
    );
    let cancel = pool.run_until(one_shot.cancel());
    assert!(
        matches!(cancel, ClientRpcResult::Failure(ref error) if error.code=="subagent-delivery-unavailable")
    );
    assert!(transport.calls.borrow().is_empty());

    let continuation_transport = Rc::new(ScriptedTransport::default());
    continuation_transport.call_results.borrow_mut().extend([
        Ok(ClientRpcResult::Success(Some(json!({"messageId":"m"})))),
        Ok(ClientRpcResult::Success(Some(json!({"accepted":true})))),
    ]);
    let mut continuation_options = options(
        Rc::new(ManualScheduler::default()),
        Rc::new(PoolSpawner(pool.spawner())),
    );
    continuation_options.address = Some(SubagentAddress {
        parent_session_id: SessionId::new("parent"),
        child_session_id: SessionId::new("child"),
        mode: SubagentMode::Continuable,
    });
    let continuation = ClientSession::new(
        SessionId::new("child"),
        continuation_transport.clone(),
        continuation_options,
    );
    assert!(
        pool.run_until(
            continuation.prompt(vec![json!({"type":"text","text":"continue"})], "queue",)
        )
        .is_ok()
    );
    assert_eq!(
        continuation_transport.calls.borrow()[0].method,
        "subagent.prompt"
    );
    let image = pool
        .run_until(continuation.prompt(vec![json!({"type":"image","attachmentId":"a"})], "queue"));
    assert!(matches!(image, ClientRpcResult::Failure(ref error) if error.code=="attachment-error"));
    assert!(pool.run_until(continuation.cancel()).is_ok());
    assert_eq!(
        continuation_transport.calls.borrow().last().unwrap().method,
        "subagent.interrupt"
    );
}

#[test]
fn resync_generation_guards_drop_stale_open_success_and_transport_failure() {
    let mut pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let transport = Rc::new(ScriptedTransport::default());
    let (old_sender, old_receiver) = oneshot::channel();
    transport
        .histories
        .borrow_mut()
        .push_back(HistoryPlan::Deferred(old_receiver));
    transport
        .histories
        .borrow_mut()
        .push_back(successful_page(page(&[10], false)));
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport,
        options(scheduler, Rc::new(PoolSpawner(pool.spawner()))),
    );
    pool.spawner().spawn_local(session.open()).unwrap();
    pool.run_until_stalled();
    assert_eq!(session.snapshot().open_state, SessionOpenState::Loading);
    let resync = session.resync();
    assert_eq!(session.snapshot().open_state, SessionOpenState::Loading);
    pool.run_until(resync);
    assert_eq!(chat_seqs(&session), [10]);
    assert!(
        old_sender
            .send(Ok(ClientRpcResult::Success(Some(page(&[1], false)))))
            .is_ok()
    );
    pool.run_until_stalled();
    assert_eq!(session.snapshot().open_state, SessionOpenState::Open);
    assert_eq!(chat_seqs(&session), [10]);
}

#[test]
fn resync_clears_pending_but_preserves_a_queue_baseline_that_raced_ahead() {
    let mut pool = LocalPool::new();
    let transport = Rc::new(ScriptedTransport::default());
    let (sender, receiver) = oneshot::channel();
    transport.histories.borrow_mut().extend([
        successful_page(page(&[], false)),
        HistoryPlan::Deferred(receiver),
    ]);
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport,
        options(
            Rc::new(ManualScheduler::default()),
            Rc::new(PoolSpawner(pool.spawner())),
        ),
    );
    pool.run_until(session.open());
    session.handle_mux_envelope(
        RpcId::new("approval"),
        SessionMuxFrame::ApprovalRequested {
            payload: json!({"approvalId":"a"}),
        },
    );
    let old = session.snapshot().pending[0].clone();
    session.handle_mux_envelope(
        RpcId::new("subscribed"),
        SessionMuxFrame::Subscribed { last_seq: 0 },
    );
    session.handle_mux_envelope(
        RpcId::new("queue"),
        SessionMuxFrame::Queue(vec![QueueItemInput {
            id: MessageId::new("item"),
            message_id: MessageId::new("message"),
            placement: QueuePlacement::Queued,
            content: vec![json!({"type":"text","text":"queued"})],
        }]),
    );
    let resync = session.resync();
    assert!(session.snapshot().pending.is_empty());
    assert_eq!(session.snapshot().queue.len(), 1);
    session.handle_mux_envelope(
        RpcId::new("approval"),
        SessionMuxFrame::ApprovalRequested {
            payload: json!({"approvalId":"a"}),
        },
    );
    let replayed = session.snapshot().pending[0].clone();
    assert_eq!(old.key, replayed.key);
    assert!(!Rc::ptr_eq(&old, &replayed));
    assert!(
        sender
            .send(Ok(ClientRpcResult::Success(Some(page(&[], false)))))
            .is_ok()
    );
    pool.run_until(resync);
    assert_eq!(session.snapshot().queue.len(), 1);
}

#[test]
fn snapshot_notifications_dedupe_running_flips_and_keep_unrelated_arrays_stable() {
    let mut pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let transport = Rc::new(ScriptedTransport::default());
    transport
        .histories
        .borrow_mut()
        .push_back(successful_page(page(&[], false)));
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport,
        options(scheduler.clone(), Rc::new(PoolSpawner(pool.spawner()))),
    );
    pool.run_until(session.open());
    scheduler.flush();
    let before = session.snapshot();
    let pending = before.pending.clone();
    let queue = before.queue.clone();
    let ticks = Rc::new(Cell::new(0));
    let observed = ticks.clone();
    let subscription = session.subscribe(Rc::new(move || observed.set(observed.get() + 1)));
    session.handle_running(true);
    session.handle_running(true);
    scheduler.flush();
    let after = session.snapshot();
    assert_eq!(ticks.get(), 1);
    assert!(after.running);
    assert_eq!(after.composer_phase, ComposerPhase::Active);
    assert!(Rc::ptr_eq(&pending, &after.pending));
    assert!(Rc::ptr_eq(&queue, &after.queue));
    subscription.dispose();
    session.handle_running(false);
    scheduler.flush();
    assert_eq!(ticks.get(), 1);
    session.bind_scope().unwrap();
    assert_eq!(
        session.bind_scope().unwrap_err(),
        "session s1 already has a bound scope"
    );
    session.unbind_scope();
    session.bind_scope().unwrap();
}

#[test]
fn subscribed_tail_baseline_triggers_one_second_stitch_and_failure_keeps_first_window_open() {
    let mut pool = LocalPool::new();
    let transport = Rc::new(ScriptedTransport::default());
    transport.histories.borrow_mut().extend([
        successful_page(page(&[1], false)),
        successful_page(page(&[1, 2, 3], false)),
    ]);
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport.clone(),
        options(
            Rc::new(ManualScheduler::default()),
            Rc::new(PoolSpawner(pool.spawner())),
        ),
    );
    session.handle_mux_envelope(
        RpcId::new("subscribed"),
        SessionMuxFrame::Subscribed { last_seq: 3 },
    );
    pool.run_until(session.open());
    assert_eq!(transport.history_requests.borrow().len(), 2);
    assert_eq!(chat_seqs(&session), [1, 2, 3]);

    let failed_transport = Rc::new(ScriptedTransport::default());
    failed_transport.histories.borrow_mut().extend([
        successful_page(page(&[1], false)),
        HistoryPlan::Ready(Ok(ClientRpcResult::Failure(ClientRpcError {
            code: "stitch-failed".to_owned(),
            message: "later".to_owned(),
            details: Map::new(),
        }))),
    ]);
    let failed = ClientSession::new(
        SessionId::new("failed"),
        failed_transport,
        options(
            Rc::new(ManualScheduler::default()),
            Rc::new(PoolSpawner(pool.spawner())),
        ),
    );
    failed.handle_mux_envelope(
        RpcId::new("subscribed"),
        SessionMuxFrame::Subscribed { last_seq: 3 },
    );
    pool.run_until(failed.open());
    assert_eq!(failed.snapshot().open_state, SessionOpenState::Open);
    assert_eq!(chat_seqs(&failed), [1]);
}

#[test]
fn stale_or_absent_projection_baselines_never_regress_live_values() {
    let mut pool = LocalPool::new();
    let scheduler = Rc::new(ManualScheduler::default());
    let projections = Rc::new(ProjectionValueStore::new(scheduler.clone()));
    projections.apply("title", Rc::new(json!("Live")), 9);
    let transport = Rc::new(ScriptedTransport::default());
    transport
        .histories
        .borrow_mut()
        .push_back(successful_page(SessionHistoryPage {
            entries: Vec::new(),
            has_more: false,
            projections: Some(ProjectionsBaseline {
                as_of_seq: 5,
                values: IndexMap::from([("title".to_owned(), Rc::new(json!("Stale")))]),
            }),
        }));
    let mut session_options = options(scheduler, Rc::new(PoolSpawner(pool.spawner())));
    session_options.projections = Some(projections.clone());
    let session = ClientSession::new(SessionId::new("s1"), transport, session_options);
    pool.run_until(session.open());
    assert_eq!(projections.get("title").as_deref(), Some(&json!("Live")));
}

#[test]
fn failed_first_prompt_stays_blank_and_engaging_while_successful_cancel_stays_clear() {
    let mut pool = LocalPool::new();
    let transport = Rc::new(ScriptedTransport::default());
    transport.call_results.borrow_mut().extend([
        Ok(ClientRpcResult::Failure(ClientRpcError {
            code: "prompt-denied".to_owned(),
            message: "retry".to_owned(),
            details: Map::new(),
        })),
        Ok(ClientRpcResult::Success(Some(json!({"accepted":true})))),
    ]);
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport,
        options(
            Rc::new(ManualScheduler::default()),
            Rc::new(PoolSpawner(pool.spawner())),
        ),
    );
    let prompt = session.prompt(vec![json!({"type":"text","text":"hi"})], "queue");
    assert_eq!(session.snapshot().composer_phase, ComposerPhase::Engaging);
    assert!(matches!(
        pool.run_until(prompt),
        ClientRpcResult::Failure(_)
    ));
    let snapshot = session.snapshot();
    assert!(snapshot.blank);
    assert_eq!(snapshot.composer_phase, ComposerPhase::Engaging);
    assert_eq!(
        snapshot.prompt_error.as_ref().unwrap().operation,
        PromptOperation::Send
    );
    assert!(pool.run_until(session.cancel()).is_ok());
    assert_eq!(
        session.snapshot().prompt_error.as_ref().unwrap().operation,
        PromptOperation::Send
    );
}

#[test]
fn cold_live_events_are_ignored_and_history_and_mux_views_reach_definitions() {
    let mut pool = LocalPool::new();
    let transport = Rc::new(ScriptedTransport::default());
    let mut history_entry = entry(1, "message", json!({}));
    history_entry.view = Some(Rc::new(json!({"source":"history"})));
    transport
        .histories
        .borrow_mut()
        .push_back(successful_page(SessionHistoryPage {
            entries: vec![history_entry],
            has_more: false,
            projections: None,
        }));
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport,
        options(
            Rc::new(ManualScheduler::default()),
            Rc::new(PoolSpawner(pool.spawner())),
        ),
    );
    session.handle_mux_envelope(
        RpcId::new("cold"),
        SessionMuxFrame::Event(entry(99, "message", json!({}))),
    );
    pool.run_until(session.open());
    assert_eq!(chat_seqs(&session), [1]);
    let first = session.snapshot();
    let chat = first.chat.as_ref().unwrap();
    let key = chat["order"][0].as_str().unwrap();
    assert_eq!(chat["nodes"][key]["view"]["source"], "history");
    let mut live = entry(2, "message", json!({}));
    live.view = Some(Rc::new(json!({"source":"mux"})));
    session.handle_mux_envelope(RpcId::new("live"), SessionMuxFrame::Event(live));
    let snapshot = session.snapshot();
    let chat = snapshot.chat.as_ref().unwrap();
    let key = chat["order"][1].as_str().unwrap();
    assert_eq!(chat["nodes"][key]["view"]["source"], "mux");
}

#[test]
fn failed_gap_repair_releases_the_guard_and_the_next_gap_retries_once() {
    let mut pool = LocalPool::new();
    let transport = Rc::new(ScriptedTransport::default());
    transport.histories.borrow_mut().extend([
        successful_page(page(&[1], false)),
        HistoryPlan::Ready(Err("repair offline".to_owned())),
        successful_page(page(&[1, 2, 3, 4, 5], false)),
    ]);
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport.clone(),
        options(
            Rc::new(ManualScheduler::default()),
            Rc::new(PoolSpawner(pool.spawner())),
        ),
    );
    pool.run_until(session.open());
    session.handle_mux_envelope(
        RpcId::new("gap-3"),
        SessionMuxFrame::Event(entry(3, "message", json!({}))),
    );
    session.handle_mux_envelope(
        RpcId::new("gap-4"),
        SessionMuxFrame::Event(entry(4, "message", json!({}))),
    );
    pool.run_until_stalled();
    assert_eq!(transport.history_requests.borrow().len(), 2);
    assert_eq!(chat_seqs(&session), [1]);
    session.handle_mux_envelope(
        RpcId::new("gap-5"),
        SessionMuxFrame::Event(entry(5, "message", json!({}))),
    );
    pool.run_until_stalled();
    assert_eq!(transport.history_requests.borrow().len(), 3);
    assert_eq!(chat_seqs(&session), [1, 2, 3, 4, 5]);
}

#[test]
fn concurrent_load_older_calls_share_the_inflight_guard() {
    let mut pool = LocalPool::new();
    let transport = Rc::new(ScriptedTransport::default());
    let (sender, receiver) = oneshot::channel();
    transport.histories.borrow_mut().extend([
        successful_page(page(&[3, 4], true)),
        HistoryPlan::Deferred(receiver),
    ]);
    let session = ClientSession::new(
        SessionId::new("s1"),
        transport.clone(),
        options(
            Rc::new(ManualScheduler::default()),
            Rc::new(PoolSpawner(pool.spawner())),
        ),
    );
    pool.run_until(session.open());
    let first = session.clone();
    pool.spawner()
        .spawn_local(async move { first.load_older().await })
        .unwrap();
    let second = session.clone();
    pool.spawner()
        .spawn_local(async move { second.load_older().await })
        .unwrap();
    pool.run_until_stalled();
    assert_eq!(transport.history_requests.borrow().len(), 2);
    assert!(session.snapshot().loading_older);
    assert!(
        sender
            .send(Ok(ClientRpcResult::Success(Some(page(&[1, 2], false)))))
            .is_ok()
    );
    pool.run_until_stalled();
    assert!(!session.snapshot().loading_older);
    let mut seqs = chat_seqs(&session);
    seqs.sort_unstable();
    assert_eq!(seqs, [1, 2, 3, 4]);
}
