//! Production `session.create` identity, composition, rollback, and adoption cases.

use std::sync::Arc;

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use seekdeep_agent::{AgentRegistry, AgentStatus};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices};
use seekdeep_agent_presets::{
    AgentPresetConfig, AgentPresetRegistry, AgentPresetRegistryConfig, COMPOSITION_FILE,
    PresetRoot, PresetTrust, resolve_session_preset,
};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, SessionId},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, ModelSelection, RpcId, RpcMethod,
    RpcReceipt, RpcReceiptReason, RpcRequest, RpcResponse, SessionApiProxyOptions,
    SessionApiProxyRuntime,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::{AbortSignal, LlmRuntime};
use seekdeep_loader::PluginCatalog;
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tools::{ToolRuntime, ToolRuntimeConfig};
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
        let tools =
            ToolRuntime::new_with_system_prompt(&context, &prompt, ToolRuntimeConfig::default())
                .unwrap();
        let factory = AgentLoop::new(
            context.clone(),
            sessions.clone(),
            (*agents).clone(),
            AgentLoopServices {
                llm,
                system_prompt: prompt,
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
            Arc::new(TerminalDomains),
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
