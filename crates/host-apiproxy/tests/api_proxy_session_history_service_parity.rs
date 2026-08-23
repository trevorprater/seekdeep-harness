//! Production `session.history` pagination, projection, and presenter cases.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_agent_presets::{
    AgentPresetConfig, AgentPresetRegistry, AgentPresetRegistryConfig, COMPOSITION_FILE,
    PresetRoot, PresetTrust,
};
use seekdeep_attachment::{
    AttachmentBackend, AttachmentStore, ImageAttachmentLimits, ImageAttachmentRef, ImageMediaType,
    SaveImageAttachment, StoredImageAttachment,
};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionHeader, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, RpcId, RpcMethod, RpcReceipt,
    RpcReceiptReason, RpcRequest, RpcResponse, SessionApiProxyOptions, SessionApiProxyRuntime,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, Message, MessageSource, UserMessage};
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::ScopeKey;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceRevision,
    SessionPersistenceService, SessionPersistenceSnapshot,
};
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SessionProjectionRegistry,
};
use seekdeep_tools::{
    ContentToolFixtureOptions, TerminalCallView, TerminalResultView, ToolCallView,
    ToolPresentationMode, ToolResultView, ToolRuntime, ToolRuntimeConfig,
    define_content_tool_fixture,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug)]
struct TerminalDomains;

impl ApiProxyRuntime for TerminalDomains {
    fn unary(
        &self,
        _method: RpcMethod,
        request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        async move {
            Ok(RpcResponse::new(
                request.rpc_id,
                RpcResult::Success { value: None },
            ))
        }
        .boxed()
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        async {
            Ok(RpcReceipt::Rejected {
                reason: RpcReceiptReason::NotPending,
            })
        }
        .boxed()
    }

    fn mux(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<MuxFrame> {
        futures::stream::empty().boxed()
    }

    fn host(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        futures::stream::empty().boxed()
    }

    fn session_log(
        &self,
        _query: SessionLogQuery,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        async { Ok(HttpResponse::text(501, "not used")) }.boxed()
    }
}

#[derive(Default)]
struct MemoryPersistence {
    headers: Mutex<Vec<SessionHeader>>,
    inspections: Mutex<HashMap<SessionId, SessionInspection>>,
}

struct AttachmentBackendFixture {
    limits: ImageAttachmentLimits,
}

#[async_trait]
impl AttachmentBackend for AttachmentBackendFixture {
    fn image_limits(&self) -> &ImageAttachmentLimits {
        &self.limits
    }

    async fn validate_image(&self, _input: &SaveImageAttachment) -> anyhow::Result<()> {
        anyhow::bail!("not used")
    }

    async fn save_image(&self, _input: SaveImageAttachment) -> anyhow::Result<ImageAttachmentRef> {
        anyhow::bail!("not used")
    }

    async fn read_image(
        &self,
        _reference: &ImageAttachmentRef,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<StoredImageAttachment> {
        anyhow::bail!("not used")
    }
}

#[async_trait]
impl SessionPersistence for MemoryPersistence {
    fn locate(&self, _meta: &SessionHeader) -> Option<SessionLocation> {
        None
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    async fn create(&self, _meta: &SessionHeader) -> anyhow::Result<()> {
        Ok(())
    }

    async fn append(&self, _id: &SessionId, _events: &[SessionEvent]) -> anyhow::Result<()> {
        Ok(())
    }

    async fn load(&self, id: &SessionId) -> anyhow::Result<SessionInspection> {
        self.inspect(id, None).await
    }

    async fn inspect(
        &self,
        id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.inspections
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("inspection failed"))
    }

    async fn read_from(
        &self,
        id: &SessionId,
        _from_seq: u64,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.inspect(id, signal).await
    }

    async fn list(&self, _signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        Ok(self.headers.lock().clone())
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        Ok(self
            .headers
            .lock()
            .iter()
            .cloned()
            .map(|header| SessionPersistenceSnapshot {
                revision: SessionPersistenceRevision::new(format!("test:{}", header.id)),
                header,
            })
            .collect())
    }
}

struct Harness {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    projections: Arc<SessionProjectionRegistry>,
    tools: Arc<ToolRuntime>,
}

impl Harness {
    fn new() -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let projections = SessionProjectionRegistry::install(&context).unwrap();
        let tools = ToolRuntime::new(
            context.clone(),
            ToolRuntimeConfig {
                mode: ToolPresentationMode::Native,
                max_parallel_sub_calls: 4,
            },
        )
        .unwrap();
        tools.provide(&context).unwrap();
        Self {
            context,
            sessions,
            agents,
            projections,
            tools,
        }
    }

    fn runtime(&self) -> Arc<SessionApiProxyRuntime> {
        SessionApiProxyRuntime::from_context(
            &self.context,
            SessionApiProxyOptions::default(),
            Arc::new(TerminalDomains),
        )
        .unwrap()
    }

    fn session(&self, id: &str) -> Arc<Session> {
        let session = self
            .sessions
            .create(
                &self.context,
                Some(SessionId::new(id)),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session.clone(),
            inbox,
            self.context.clone(),
            ScopeKey::new(),
        ));
        self.agents.register(&self.context, &agent, None).unwrap();
        session
    }
}

async fn history(runtime: &SessionApiProxyRuntime, payload: Value) -> RpcResult<Value> {
    runtime
        .unary(
            RpcMethod::SessionHistory,
            RpcRequest::new(RpcId::new("history-test"), payload),
            AbortSignal::default(),
        )
        .await
        .unwrap()
        .result
}

fn success(result: RpcResult<Value>) -> Value {
    match result {
        RpcResult::Success { value: Some(value) } => value,
        other => panic!("expected history success, got {other:?}"),
    }
}

fn append_user(session: &Session, text: &str) -> SessionEvent {
    session
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: text.to_owned(),
                }],
                MessageSource::user(),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap()
}

fn append_assistant(session: &Session, text: &str, step: u64) -> SessionEvent {
    session
        .append(
            "assistant/message",
            json!({
                "turn": 1,
                "step": step,
                "message": Message::assistant(
                    vec![ContentBlock::Text { text: text.to_owned() }],
                    "provider",
                    "model",
                ),
            }),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap()
}

#[tokio::test]
async fn tail_history_carries_the_exact_projection_cut_and_older_pages_do_not() {
    let harness = Harness::new();
    let count_registration = harness
        .projections
        .register(
            &harness.context,
            ProjectionDefinition::new(
                "test/count",
                1,
                || Ok(json!(0)),
                |state, event| {
                    if event.event_type == "user/message" {
                        ProjectionTransition::changed(state.as_u64().unwrap_or(0) + 1)
                    } else {
                        Ok(ProjectionTransition::Unchanged)
                    }
                },
                |state| Ok(state.clone()),
            ),
        )
        .unwrap();
    let runtime = harness.runtime();
    let session = harness.session("history-projections");
    for index in 0..5 {
        append_user(&session, &format!("message {index}"));
    }
    let tail = success(history(&runtime, json!({ "sessionId": session.id() })).await);
    assert_eq!(tail["events"].as_array().unwrap().len(), 5);
    assert_eq!(tail["projections"]["asOfSeq"], 4);
    assert_eq!(tail["projections"]["values"]["test/count"], 5);
    assert!(tail["projections"]["values"].get("imageLimits").is_none());
    assert_eq!(
        tail["events"].as_array().unwrap().last().unwrap()["event"]["seq"],
        tail["projections"]["asOfSeq"]
    );
    let older = success(
        history(
            &runtime,
            json!({ "sessionId": session.id(), "beforeSeq": 4, "maxMessages": 2 }),
        )
        .await,
    );
    assert!(older.get("projections").is_none());
    assert_eq!(
        older["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["event"]["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(older["hasMore"], true);
    count_registration.dispose().await.unwrap();
    let after = success(history(&runtime, json!({ "sessionId": session.id() })).await);
    assert!(after["projections"]["values"].get("test/count").is_none());
    assert!(
        after["projections"]["values"]
            .get("sessionListMetadata")
            .is_some()
    );
}

#[derive(Deserialize)]
struct CommandArgs {
    cmd: String,
}

#[tokio::test]
async fn history_attaches_replay_safe_call_and_result_views_with_backscan_pairing() {
    let harness = Harness::new();
    let definition = ContentToolFixtureOptions::new(
        "term",
        "terminal fixture",
        json!({ "cmd": { "type": "string", "required": true } }),
        Arc::new(|_: CommandArgs, _| Box::pin(async { Ok(Vec::<ContentBlock>::new()) })),
    )
    .present_call(Arc::new(|args| {
        Some(ToolCallView::Terminal(TerminalCallView {
            title: args.cmd.clone(),
            description: None,
            cwd: None,
        }))
    }))
    .present_result(Arc::new(|args, _| {
        Some(ToolResultView::Terminal(TerminalResultView {
            title: Some(args.cmd.clone()),
            output: Some("done".to_owned()),
            exit_code: Some(0),
            signal: None,
        }))
    }));
    harness
        .tools
        .register(
            &harness.context,
            define_content_tool_fixture(definition).unwrap(),
        )
        .unwrap();
    let runtime = harness.runtime();
    let session = harness.session("history-views");
    let signal = AbortSignal::default();
    let mut mux = runtime.mux(
        RpcRequest::new(RpcId::new("view-mux"), json!({})),
        signal.clone(),
    );
    let _baseline = mux.next().await.unwrap().unwrap();
    let call_id = CallId::new("call-1");
    session
        .append(
            "tool/call",
            json!({
                "turn": 1, "step": 1, "callId": call_id,
                "name": "term", "arguments": "{\"cmd\":\"cargo test\"}"
            }),
            AppendOptions::default(),
        )
        .unwrap();
    let live_call = mux.next().await.unwrap().unwrap();
    assert_eq!(
        match live_call.payload {
            MuxFrame::SessionEvent { view, .. } => view.unwrap().view["card"].clone(),
            other => panic!("expected live call, got {other:?}"),
        },
        "terminal"
    );
    session
        .append(
            "turn/end",
            json!({ "turn": 1, "reason": { "kind": "completed" } }),
            AppendOptions::default(),
        )
        .unwrap();
    let _turn_end = mux.next().await.unwrap().unwrap();
    let result = Message::tool_result(
        &call_id,
        vec![ContentBlock::Text {
            text: "raw output".to_owned(),
        }],
        false,
    );
    session
        .append(
            "tool/result",
            json!({ "turn": 1, "step": 1, "message": result }),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let live_result = mux.next().await.unwrap().unwrap();
    assert_eq!(
        match live_result.payload {
            MuxFrame::SessionEvent { view, .. } => view.unwrap().view["output"].clone(),
            other => panic!("expected live result, got {other:?}"),
        },
        "done"
    );
    let value = success(history(&runtime, json!({ "sessionId": session.id() })).await);
    assert_eq!(value["events"][0]["view"]["for"], "call");
    assert_eq!(value["events"][0]["view"]["view"]["card"], "terminal");
    assert_eq!(value["events"][2]["view"]["for"], "result");
    assert_eq!(value["events"][2]["view"]["view"]["output"], "done");
    signal.abort();
}

#[tokio::test]
async fn presenterless_and_panicking_tools_soft_fall_without_dropping_events() {
    let harness = Harness::new();
    for definition in [
        ContentToolFixtureOptions::new(
            "plain",
            "plain fixture",
            json!({ "cmd": { "type": "string", "required": true } }),
            Arc::new(|_: CommandArgs, _| Box::pin(async { Ok(Vec::<ContentBlock>::new()) })),
        ),
        ContentToolFixtureOptions::new(
            "broken",
            "broken fixture",
            json!({ "cmd": { "type": "string", "required": true } }),
            Arc::new(|_: CommandArgs, _| Box::pin(async { Ok(Vec::<ContentBlock>::new()) })),
        )
        .present_call(Arc::new(|_| panic!("presenter exploded"))),
    ] {
        harness
            .tools
            .register(
                &harness.context,
                define_content_tool_fixture(definition).unwrap(),
            )
            .unwrap();
    }
    let runtime = harness.runtime();
    let session = harness.session("history-soft-view");
    let signal = AbortSignal::default();
    let mut mux = runtime.mux(
        RpcRequest::new(RpcId::new("soft-view-mux"), json!({})),
        signal.clone(),
    );
    let _baseline = mux.next().await.unwrap().unwrap();
    for (index, name) in ["plain", "broken"].into_iter().enumerate() {
        session
            .append(
                "tool/call",
                json!({
                    "turn": 1, "step": index + 1, "callId": format!("call-{index}"),
                    "name": name, "arguments": "{\"cmd\":\"x\"}"
                }),
                AppendOptions::default(),
            )
            .unwrap();
        let envelope = mux.next().await.unwrap().unwrap();
        assert!(matches!(
            envelope.payload,
            MuxFrame::SessionEvent { view: None, .. }
        ));
    }
    let value = success(history(&runtime, json!({ "sessionId": session.id() })).await);
    assert!(
        value["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry.get("view").is_none())
    );
    signal.abort();
}

#[tokio::test]
async fn pagination_counts_only_append_messages_and_keeps_compaction_transaction_contiguous() {
    let harness = Harness::new();
    let runtime = harness.runtime();
    let session = harness.session("history-compaction");
    session
        .append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    let first = append_user(&session, "first prompt");
    append_assistant(&session, "first reply", 1);
    let third = append_user(&session, "second prompt");
    append_assistant(&session, "second reply", 2);
    let shadowed = session.surface_nodes();
    let summary = session
        .append(
            "compaction/summary",
            json!({
                "summary": [{ "type": "text", "text": "summary" }],
                "shadowedRange": {
                    "start": shadowed.first().unwrap(),
                    "end": shadowed.last().unwrap()
                },
                "shadowedSeqs": shadowed.clone(),
                "shadowedTokenCount": 0,
                "provider": "provider",
                "model": "model"
            }),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: "<context_checkpoint>summary</context_checkpoint>".to_owned(),
                }],
                MessageSource::plugin("compact"),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(
                    *shadowed.first().unwrap(),
                    *shadowed.last().unwrap(),
                )),
                source_event_seqs: Some(shadowed.iter().copied().chain([summary.seq]).collect()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let value = success(
        history(
            &runtime,
            json!({ "sessionId": session.id(), "maxMessages": 2 }),
        )
        .await,
    );
    let seqs = value["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["event"]["seq"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(seqs, (third.seq..=summary.seq + 1).collect::<Vec<_>>());
    assert!(!seqs.contains(&first.seq));
    assert_eq!(value["hasMore"], true);
}

#[tokio::test]
async fn cold_history_inspects_without_attaching_and_distinguishes_missing_composition() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let missing = SessionApiProxyRuntime::from_context(
        &context,
        SessionApiProxyOptions::default(),
        Arc::new(TerminalDomains),
    )
    .unwrap();
    let no_persistence = history(&missing, json!({ "sessionId": "cold" })).await;
    assert!(matches!(
        no_persistence,
        RpcResult::Failure { ref error } if error.code == "internal"
    ));

    let persistence = Arc::new(MemoryPersistence::default());
    let mut meta = SessionHeader::new(SessionId::new("cold"));
    meta.cwd = Some("/project".to_owned());
    persistence.headers.lock().push(meta.clone());
    persistence.inspections.lock().insert(
        meta.id.clone(),
        SessionInspection {
            meta: meta.clone(),
            events: vec![SessionEvent {
                event_type: "session/title".to_owned(),
                seq: 0,
                time: 1,
                data: json!({ "title": "Cold", "messageSeqs": [], "source": { "kind": "fallback" } }),
                source_event_seqs: None,
                surface_op: None,
                ignorable: None,
            }],
        },
    );
    SessionPersistenceService::new(persistence.clone())
        .provide(&context)
        .unwrap();
    let runtime = SessionApiProxyRuntime::from_context(
        &context,
        SessionApiProxyOptions::default(),
        Arc::new(TerminalDomains),
    )
    .unwrap();
    let cold = success(history(&runtime, json!({ "sessionId": "cold" })).await);
    assert_eq!(cold["events"][0]["event"]["type"], "session/title");
    assert!(sessions.get(&meta.id).is_none());
    assert!(agents.get(&meta.id).is_none());
    let absent = history(&runtime, json!({ "sessionId": "absent" })).await;
    assert!(matches!(
        absent,
        RpcResult::Failure { ref error } if error.code == "session-not-found"
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One cold transcript distinguishes two complete preset generations.
async fn cold_history_uses_the_logged_preset_standing_presenter_without_resuming() {
    let context = Context::new();
    SessionStore::install(&context).unwrap();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    SessionProjectionRegistry::install(&context).unwrap();
    let tools = ToolRuntime::new(
        context.clone(),
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            max_parallel_sub_calls: 4,
        },
    )
    .unwrap();
    tools.provide(&context).unwrap();

    let catalog = PluginCatalog::new();
    for (plugin_name, title) in [
        ("preset:standard", "standard presenter"),
        ("preset:minimal", "minimal presenter"),
    ] {
        let tools = tools.clone();
        catalog
            .register_named(
                plugin_name,
                Plugin::new(
                    plugin_name,
                    std::iter::empty::<&str>(),
                    move |plugin_ctx, _| {
                        let tools = tools.clone();
                        Box::pin(async move {
                            let definition = ContentToolFixtureOptions::new(
                                "term",
                                "terminal fixture",
                                json!({ "cmd": { "type": "string", "required": true } }),
                                Arc::new(|_: CommandArgs, _| {
                                    Box::pin(async { Ok(Vec::<ContentBlock>::new()) })
                                }),
                            )
                            .present_call(Arc::new(move |_| {
                                Some(ToolCallView::Terminal(TerminalCallView {
                                    title: title.to_owned(),
                                    description: None,
                                    cwd: None,
                                }))
                            }));
                            tools
                                .register(&plugin_ctx, define_content_tool_fixture(definition)?)?;
                            Ok(())
                        })
                    },
                ),
            )
            .unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    for (id, plugin) in [
        ("standard", "preset:standard"),
        ("minimal", "preset:minimal"),
    ] {
        let directory = root.path().join(id);
        tokio::fs::create_dir_all(&directory).await.unwrap();
        tokio::fs::write(
            directory.join(COMPOSITION_FILE),
            format!("- id: tool\n  name: {plugin}\n"),
        )
        .await
        .unwrap();
    }
    let roster = AgentPresetRegistry::new(
        &context,
        catalog,
        AgentPresetRegistryConfig {
            roster: AgentPresetConfig {
                default: "standard".to_owned(),
                roots: vec![PresetRoot {
                    path: root.path().to_string_lossy().into_owned(),
                    trust: PresetTrust::System,
                }],
                include_user_root: false,
            },
            user_root: None,
        },
    )
    .unwrap();
    roster.provide(&context).unwrap();

    let persistence = Arc::new(MemoryPersistence::default());
    let mut meta = SessionHeader::new(SessionId::new("cold-preset"));
    meta.cwd = Some("/project".to_owned());
    meta.agent_preset = Some("standard".to_owned());
    let events = vec![
        SessionEvent {
            event_type: "agent-preset/selected".to_owned(),
            seq: 0,
            time: 1,
            data: json!({ "agentPreset": "minimal" }),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
        SessionEvent {
            event_type: "tool/call".to_owned(),
            seq: 1,
            time: 2,
            data: json!({
                "callId": "call-1",
                "name": "term",
                "arguments": "{\"cmd\":\"echo\"}"
            }),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
    ];
    persistence.headers.lock().push(meta.clone());
    persistence.inspections.lock().insert(
        meta.id.clone(),
        SessionInspection {
            meta: meta.clone(),
            events,
        },
    );
    SessionPersistenceService::new(persistence)
        .provide(&context)
        .unwrap();
    let runtime = SessionApiProxyRuntime::from_context(
        &context,
        SessionApiProxyOptions::default(),
        Arc::new(TerminalDomains),
    )
    .unwrap();

    let value = success(history(&runtime, json!({ "sessionId": meta.id })).await);
    assert_eq!(
        value["events"][1]["view"]["view"]["title"],
        "minimal presenter"
    );
    assert!(agents.get(&meta.id).is_none());
}

#[tokio::test]
async fn attachment_limits_are_a_constant_history_projection_without_change_frames() {
    let harness = Harness::new();
    let limits = ImageAttachmentLimits {
        max_image_bytes: 5 * 1024 * 1024,
        max_images_per_message: 20,
        max_message_image_bytes: 100 * 1024 * 1024,
        max_image_pixels: 40_000_000,
        media_types: vec![ImageMediaType::Png],
    };
    Arc::new(AttachmentStore::new(Arc::new(AttachmentBackendFixture {
        limits: limits.clone(),
    })))
    .provide(&harness.context)
    .unwrap();
    let runtime = harness.runtime();
    let session = harness.session("image-limits");
    append_user(&session, "first");
    let tail = success(history(&runtime, json!({ "sessionId": session.id() })).await);
    assert_eq!(
        tail["projections"]["values"]["imageLimits"],
        serde_json::to_value(limits).unwrap()
    );
    let signal = AbortSignal::default();
    let mut mux = runtime.mux(
        RpcRequest::new(RpcId::new("image-mux"), json!({})),
        signal.clone(),
    );
    let _baseline = mux.next().await.unwrap().unwrap();
    append_user(&session, "second");
    let mut saw_event = false;
    let mut saw_image_change = false;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !saw_event {
            let envelope = mux.next().await.unwrap().unwrap();
            match envelope.payload {
                MuxFrame::SessionEvent { .. } => saw_event = true,
                MuxFrame::SessionProjection { key, .. } if key == "imageLimits" => {
                    saw_image_change = true;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(!saw_image_change);
    signal.abort();
}

#[tokio::test]
async fn mux_orders_session_baselines_before_live_events_and_projection_changes() {
    let harness = Harness::new();
    harness
        .projections
        .register(
            &harness.context,
            ProjectionDefinition::new(
                "test/last-user",
                1,
                || Ok(Value::Null),
                |_state, event| {
                    if event.event_type == "user/message" {
                        ProjectionTransition::changed(event.data.clone())
                    } else {
                        Ok(ProjectionTransition::Unchanged)
                    }
                },
                |state| Ok(state.clone()),
            ),
        )
        .unwrap();
    let runtime = harness.runtime();
    let session = harness.session("mux-session");
    let signal = AbortSignal::default();
    let mut mux = runtime.mux(
        RpcRequest::new(RpcId::new("mux-history"), json!({})),
        signal.clone(),
    );
    let baseline = mux.next().await.unwrap().unwrap();
    assert!(matches!(
        baseline.payload,
        MuxFrame::SessionSubscribed {
            ref session_id,
            last_seq: -1,
        } if session_id == session.id()
    ));
    append_user(&session, "live");
    let mut saw_event = false;
    let mut projection_keys = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !saw_event || projection_keys.len() < 2 {
            let envelope = mux.next().await.unwrap().unwrap();
            match envelope.payload {
                MuxFrame::SessionEvent { event, .. } => {
                    saw_event = event.kind == "user/message";
                }
                MuxFrame::SessionProjection { key, .. } => projection_keys.push(key),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_event);
    assert!(projection_keys.contains(&"sessionListMetadata".to_owned()));
    assert!(projection_keys.contains(&"test/last-user".to_owned()));
    signal.abort();
}

#[tokio::test]
async fn composition_without_projection_registry_serves_history_and_emits_no_projection_frames() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let runtime = SessionApiProxyRuntime::from_context(
        &context,
        SessionApiProxyOptions::default(),
        Arc::new(TerminalDomains),
    )
    .unwrap();
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("no-projections")),
            CreateSessionOptions {
                cwd: Some("/project".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    append_user(&session, "first");
    let value = success(history(&runtime, json!({ "sessionId": session.id() })).await);
    assert!(value.get("projections").is_none());
    let signal = AbortSignal::default();
    let mut mux = runtime.mux(
        RpcRequest::new(RpcId::new("no-projection-mux"), json!({})),
        signal.clone(),
    );
    let _baseline = mux.next().await.unwrap().unwrap();
    append_user(&session, "second");
    let event = mux.next().await.unwrap().unwrap();
    assert!(matches!(event.payload, MuxFrame::SessionEvent { .. }));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), mux.next())
            .await
            .is_err()
    );
    signal.abort();
}
