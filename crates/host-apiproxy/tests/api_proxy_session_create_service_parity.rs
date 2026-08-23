//! Production `session.create` identity, composition, rollback, and adoption cases.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use seekdeep_agent::{AgentEvents, AgentRegistry, AgentStatus, assemble_context_for};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices};
use seekdeep_agent_presets::{
    AgentPresetConfig, AgentPresetRegistry, AgentPresetRegistryConfig, COMPOSITION_FILE,
    PresetRoot, PresetTrust, resolve_session_preset,
};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionHeader, SessionId, SurfaceOp},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, ModelSelection, PathOpenerInternals,
    PresetApiProxyOptions, PresetApiProxyRuntime, RpcId, RpcMethod, RpcReceipt, RpcReceiptReason,
    RpcRequest, RpcResponse, SessionApiProxyOptions, SessionApiProxyRuntime,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::{
    AbortSignal, ContentBlock, LlmCallConfig, LlmRuntime, MessageSource, ModelId, ProviderId,
    UserMessage,
};
use seekdeep_loader::PluginCatalog;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceSnapshot,
};
use seekdeep_storage::{Storage, StorageBackend};
use seekdeep_storage_domain::{DomainConfig, DomainFacility};
use seekdeep_storage_json::JsonStorageBackend;
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tools::{ToolRuntime, ToolRuntimeConfig};
use seekdeep_workspace::WorkspaceRegistry;
use serde_json::{Value, json};

struct EmptyPersistence;

#[async_trait]
impl SessionPersistence for EmptyPersistence {
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
        Ok(Vec::new())
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        Ok(Vec::new())
    }
}

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

struct Harness {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    runtime: Arc<SessionApiProxyRuntime>,
    _project: tempfile::TempDir,
    _presets: tempfile::TempDir,
}

impl Harness {
    async fn new(with_roster: bool) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let llm = LlmRuntime::install(&context).unwrap();
        let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
        prompt.provide(&context).unwrap();
        let tools =
            ToolRuntime::new_with_system_prompt(&context, &prompt, ToolRuntimeConfig::default())
                .unwrap();
        let factory = AgentLoop::new(
            context.clone(),
            sessions.clone(),
            (*agents).clone(),
            AgentLoopServices {
                llm,
                system_prompt: prompt.clone(),
                tools,
                max_parallel_tool_calls: 4,
            },
        )
        .unwrap();
        agents.set_factory(Arc::new(factory)).unwrap();

        let presets = tempfile::tempdir().unwrap();
        for id in ["standard", "minimal"] {
            let directory = presets.path().join(id);
            tokio::fs::create_dir_all(&directory).await.unwrap();
            tokio::fs::write(directory.join(COMPOSITION_FILE), "[]\n")
                .await
                .unwrap();
        }
        if with_roster {
            let roster = AgentPresetRegistry::new(
                &context,
                PluginCatalog::new(),
                AgentPresetRegistryConfig {
                    roster: AgentPresetConfig {
                        default: "standard".to_owned(),
                        roots: vec![PresetRoot {
                            path: presets.path().to_string_lossy().into_owned(),
                            trust: PresetTrust::System,
                        }],
                        include_user_root: false,
                    },
                    user_root: None,
                },
            )
            .unwrap();
            roster.provide(&context).unwrap();
        }
        let project = tempfile::tempdir().unwrap();
        let presets_runtime = PresetApiProxyRuntime::from_context(
            &context,
            PresetApiProxyOptions {
                default_model_selection: Arc::new(|| ModelSelection {
                    provider: "provider".to_owned(),
                    model: "model".to_owned(),
                    reasoning_effort: None,
                }),
                save_default_model_selection: None,
                open_path: None,
                can_open_path: None,
                native_path_opener: PathOpenerInternals::default(),
            },
            Arc::new(TerminalDomains),
        );
        let runtime = SessionApiProxyRuntime::from_context(
            &context,
            SessionApiProxyOptions {
                default_cwd: Some(project.path().to_string_lossy().into_owned()),
                default_model_selection: Some(Arc::new(|| ModelSelection {
                    provider: "provider".to_owned(),
                    model: "model".to_owned(),
                    reasoning_effort: None,
                })),
                ..SessionApiProxyOptions::default()
            },
            presets_runtime,
        )
        .unwrap();
        Self {
            context,
            sessions,
            agents,
            runtime,
            _project: project,
            _presets: presets,
        }
    }
}

async fn create(runtime: &SessionApiProxyRuntime, payload: Value) -> RpcResult<Value> {
    runtime
        .unary(
            RpcMethod::SessionCreate,
            RpcRequest::new(RpcId::new("create-test"), payload),
            AbortSignal::default(),
        )
        .await
        .unwrap()
        .result
}

async fn fork(runtime: &SessionApiProxyRuntime, payload: Value) -> RpcResult<Value> {
    runtime
        .unary(
            RpcMethod::SessionFork,
            RpcRequest::new(RpcId::new("fork-test"), payload),
            AbortSignal::default(),
        )
        .await
        .unwrap()
        .result
}

fn append_completed_turn(session: &Session, turn: u64) {
    session
        .append(
            "turn/start",
            json!({ "turn": turn }),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: format!("prompt {turn}"),
                }],
                MessageSource::user(),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    session
        .append(
            "turn/end",
            json!({ "turn": turn, "reason": { "kind": "completed" } }),
            AppendOptions::default(),
        )
        .unwrap();
}

fn value(result: RpcResult<Value>) -> Value {
    match result {
        RpcResult::Success { value: Some(value) } => value,
        other => panic!("expected success, got {other:?}"),
    }
}

fn error(result: RpcResult<Value>) -> (String, Value) {
    match result {
        RpcResult::Failure { error } => (error.code, Value::Object(error.details)),
        other @ RpcResult::Success { .. } => panic!("expected failure, got {other:?}"),
    }
}

#[tokio::test]
async fn creation_records_and_mounts_the_resolved_default_or_named_preset() {
    let harness = Harness::new(true).await;
    let default = value(create(&harness.runtime, json!({ "sessionId": "default" })).await);
    let named = value(
        create(
            &harness.runtime,
            json!({ "sessionId": "named", "agentPreset": "minimal" }),
        )
        .await,
    );
    assert_eq!(default["agentPreset"], "standard");
    assert_eq!(named["agentPreset"], "minimal");
    let default_agent = harness.agents.get(&SessionId::new("default")).unwrap();
    let named_agent = harness.agents.get(&SessionId::new("named")).unwrap();
    assert_eq!(default_agent.status(), AgentStatus::Idle);
    assert_eq!(
        default_agent.session().header().agent_preset.as_deref(),
        Some("standard")
    );
    assert_eq!(
        named_agent.session().header().agent_preset.as_deref(),
        Some("minimal")
    );
}

#[tokio::test]
async fn unknown_preset_rolls_back_session_and_agent_publication() {
    let harness = Harness::new(true).await;
    let (code, details) = error(
        create(
            &harness.runtime,
            json!({ "sessionId": "unknown", "agentPreset": "missing" }),
        )
        .await,
    );
    assert_eq!(code, "agent-preset-not-found");
    assert_eq!(details["available"], json!(["minimal", "standard"]));
    assert!(harness.sessions.get(&SessionId::new("unknown")).is_none());
    assert!(harness.agents.get(&SessionId::new("unknown")).is_none());
}

#[tokio::test]
async fn adoption_uses_the_logged_switch_and_rejects_stale_preset_or_cwd() {
    let harness = Harness::new(true).await;
    value(
        create(
            &harness.runtime,
            json!({ "sessionId": "adopt", "agentPreset": "standard" }),
        )
        .await,
    );
    let session = harness.sessions.get(&SessionId::new("adopt")).unwrap();
    session
        .append(
            "agent-preset/selected",
            json!({ "agentPreset": "minimal" }),
            AppendOptions::default(),
        )
        .unwrap();
    let adopted = value(
        create(
            &harness.runtime,
            json!({ "sessionId": "adopt", "agentPreset": "minimal" }),
        )
        .await,
    );
    assert_eq!(adopted["agentPreset"], "minimal");
    assert_eq!(
        resolve_session_preset(session.header(), &session.events()).as_deref(),
        Some("minimal")
    );
    let (preset_code, preset_details) = error(
        create(
            &harness.runtime,
            json!({ "sessionId": "adopt", "agentPreset": "standard" }),
        )
        .await,
    );
    assert_eq!(preset_code, "agent-preset-conflict");
    assert_eq!(preset_details["existingPreset"], "minimal");
    let other = tempfile::tempdir().unwrap();
    let (cwd_code, cwd_details) = error(
        create(
            &harness.runtime,
            json!({ "sessionId": "adopt", "cwd": other.path() }),
        )
        .await,
    );
    assert_eq!(cwd_code, "session-conflict");
    assert_eq!(cwd_details["sessionId"], "adopt");
}

#[tokio::test]
async fn rosterless_named_creation_records_none_and_refuses_to_claim_the_name() {
    let harness = Harness::new(false).await;
    let (code, details) = error(
        create(
            &harness.runtime,
            json!({ "sessionId": "plain", "agentPreset": "ignored" }),
        )
        .await,
    );
    assert_eq!(code, "agent-preset-conflict");
    assert_eq!(details["requestedPreset"], "ignored");
    assert!(details.get("existingPreset").is_none());
    assert!(
        harness
            .sessions
            .get(&SessionId::new("plain"))
            .unwrap()
            .header()
            .agent_preset
            .is_none()
    );
    assert!(harness.context.get(SESSIONS).is_some());
}

#[tokio::test]
async fn fork_cuts_completed_turns_records_lineage_and_installs_logged_model_selection() {
    let harness = Harness::new(true).await;
    value(
        create(
            &harness.runtime,
            json!({ "sessionId": "fork-source", "agentPreset": "minimal" }),
        )
        .await,
    );
    let source = harness
        .sessions
        .get(&SessionId::new("fork-source"))
        .unwrap();
    append_completed_turn(&source, 1);
    append_completed_turn(&source, 2);
    let child = value(
        fork(
            &harness.runtime,
            json!({ "sessionId": source.id(), "atSeq": 1 }),
        )
        .await,
    );
    let child_id = SessionId::new(child["sessionId"].as_str().unwrap());
    let child_session = harness.sessions.get(&child_id).unwrap();
    assert_eq!(
        child_session
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["turn/start", "user/message", "turn/end", "session/end-seed"]
    );
    assert_eq!(
        child_session.header().parent_session.as_ref(),
        Some(source.id())
    );
    assert_eq!(
        child_session.header().cwd.as_deref(),
        source.header().cwd.as_deref()
    );
    assert_eq!(
        child_session.header().agent_preset.as_deref(),
        Some("minimal")
    );

    source
        .append(
            "request/header",
            json!({
                "header": {
                    "config": {
                        "provider": "inherited-provider",
                        "model": "inherited-model",
                        "reasoningEffort": "high"
                    }
                },
                "reason": "initial"
            }),
            AppendOptions::default(),
        )
        .unwrap();
    let routed_child = value(fork(&harness.runtime, json!({ "sessionId": source.id() })).await);
    let routed_child = SessionId::new(routed_child["sessionId"].as_str().unwrap());
    let child_agent = harness.agents.get(&routed_child).unwrap();
    let prompt = harness
        .context
        .get(seekdeep_system_prompt::SYSTEM_PROMPT)
        .unwrap();
    let assembly = prompt
        .assemble(assemble_context_for(&child_agent, None))
        .await
        .unwrap();
    assert_eq!(
        assembly.variables["provider"].as_deref(),
        Some("inherited-provider")
    );
    let routed: LlmCallConfig = AgentEvents::new(harness.context.clone(), child_agent)
        .waterfall("agent/request", (), || async {
            Ok(LlmCallConfig {
                provider: ProviderId::new("fallback"),
                model: ModelId::new("fallback"),
                reasoning_effort: None,
                temperature: None,
                max_tokens: None,
                stop: None,
            })
        })
        .await
        .unwrap();
    assert_eq!(routed.provider.as_str(), "inherited-provider");
    assert_eq!(routed.model.as_str(), "inherited-model");
    assert_eq!(routed.reasoning_effort.as_ref().unwrap().as_str(), "high");
}

#[tokio::test]
async fn fork_uses_last_completed_turn_only_for_omitted_or_past_end_anchors() {
    let harness = Harness::new(false).await;
    value(create(&harness.runtime, json!({ "sessionId": "fork-tail" })).await);
    let source = harness.sessions.get(&SessionId::new("fork-tail")).unwrap();
    append_completed_turn(&source, 1);
    source
        .append("turn/start", json!({ "turn": 2 }), AppendOptions::default())
        .unwrap();
    let open = source
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: "open".to_owned(),
                }],
                MessageSource::user(),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let unavailable = fork(
        &harness.runtime,
        json!({ "sessionId": source.id(), "atSeq": open.seq }),
    )
    .await;
    assert!(matches!(
        unavailable,
        RpcResult::Failure { ref error } if error.code == "fork-unavailable"
    ));
    for anchor in [None, Some(999_u64)] {
        let payload = anchor.map_or_else(
            || json!({ "sessionId": source.id() }),
            |at| json!({ "sessionId": source.id(), "atSeq": at }),
        );
        let child = value(fork(&harness.runtime, payload).await);
        let child = harness
            .sessions
            .get(&SessionId::new(child["sessionId"].as_str().unwrap()))
            .unwrap();
        assert_eq!(
            child
                .events()
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            ["turn/start", "user/message", "turn/end", "session/end-seed"]
        );
    }
}

#[tokio::test]
async fn workspace_session_creation_uses_the_registry_path_and_commits_membership_idempotently() {
    let harness = Harness::new(false).await;
    let storage_root = tempfile::tempdir().unwrap();
    let storage = Storage::new();
    let _storage_effect = storage.provide(&harness.context).unwrap();
    let backend = JsonStorageBackend::new(storage_root.path());
    let _backend_registration = storage
        .backend
        .register("json", backend.clone() as Arc<dyn StorageBackend>)
        .unwrap();
    let facility = DomainFacility::new(
        harness.context.clone(),
        storage,
        DomainConfig {
            backend: "json".to_owned(),
            routes: HashMap::default(),
        },
    );
    let (_facility_effect, _mount) = facility.mount(&harness.context).unwrap();
    let registry = WorkspaceRegistry::open(
        harness.context.clone(),
        &facility,
        Arc::new(EmptyPersistence),
        Some(harness.sessions.clone()),
    )
    .await
    .unwrap();
    registry.provide(&harness.context).unwrap();
    let directories = tempfile::tempdir().unwrap();
    let workspace_path = directories.path().join("workspace");
    tokio::fs::create_dir(&workspace_path).await.unwrap();
    let workspace = registry
        .create(workspace_path.to_string_lossy().into_owned(), None)
        .await
        .unwrap();

    for _ in 0..2 {
        let created = value(
            create(
                &harness.runtime,
                json!({
                    "sessionId": "workspace-session",
                    "workspaceId": workspace.id().as_str()
                }),
            )
            .await,
        );
        assert_eq!(created["sessionId"], "workspace-session");
    }
    assert_eq!(
        harness
            .sessions
            .get(&SessionId::new("workspace-session"))
            .unwrap()
            .header()
            .cwd
            .as_deref(),
        Some(workspace.path().as_str())
    );
    assert_eq!(
        workspace.session_ids(),
        [SessionId::new("workspace-session")]
    );

    value(
        create(
            &harness.runtime,
            json!({ "sessionId": "cwd-only", "cwd": workspace.path() }),
        )
        .await,
    );
    assert!(
        !workspace
            .session_ids()
            .contains(&SessionId::new("cwd-only"))
    );
    let (code, details) = error(
        create(
            &harness.runtime,
            json!({ "sessionId": "unknown-workspace", "workspaceId": "missing" }),
        )
        .await,
    );
    assert_eq!(code, "workspace-not-found");
    assert_eq!(details["workspaceId"], "missing");
}
