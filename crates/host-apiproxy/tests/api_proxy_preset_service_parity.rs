//! Production Agent-preset roster, authoring, native-open, and switching RPC cases.

use std::sync::Arc;

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_agent_presets::{
    AgentPresetConfig, AgentPresetRegistry, AgentPresetRegistryConfig, COMPOSITION_FILE,
    PresetRoot, PresetTrust, resolve_session_preset,
};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::{
    session::{AppendOptions, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, ModelSelection, PathOpenerInternals,
    PresetApiProxyOptions, PresetApiProxyRuntime, RpcId, RpcMethod, RpcReceipt, RpcReceiptReason,
    RpcRequest, RpcResponse,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::AbortSignal;
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::{ScopeKey, create_scope};
use seekdeep_skill::{
    Config as SkillConfig, SkillDefinition, SkillInvocationPolicy, SkillRegistry, SkillSource,
    SkillSummary,
};
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
    roster: Arc<AgentPresetRegistry>,
    runtime: Arc<PresetApiProxyRuntime>,
    _system_root: tempfile::TempDir,
    user_root: tempfile::TempDir,
}

impl Harness {
    #[allow(clippy::too_many_lines)] // One fixture assembles both private and layered preset capabilities.
    async fn new(can_open: bool, opened: Arc<Mutex<Vec<String>>>) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let system_root = tempfile::tempdir().unwrap();
        let user_root = tempfile::tempdir().unwrap();
        preset(system_root.path(), "standard", "Standard").await;
        preset(user_root.path(), "minimal", "Minimal").await;
        let layered_skills = SkillRegistry::install(&context, &SkillConfig::default()).unwrap();
        let catalog = PluginCatalog::new();
        catalog
            .register_named(
                "preset:capabilities",
                Plugin::new(
                    "preset:capabilities",
                    std::iter::empty::<&str>(),
                    |plugin_context, _| {
                        Box::pin(async move {
                            seekdeep_goal::GoalService::install(
                                &plugin_context,
                                seekdeep_goal::Config::default(),
                            )?;
                            let skills =
                                SkillRegistry::install(&plugin_context, &SkillConfig::default())?;
                            for (name, user_invocable) in
                                [("preset-owned", true), ("model-only", false)]
                            {
                                skills.register(
                                    &plugin_context,
                                    SkillDefinition {
                                        summary: SkillSummary {
                                            name: name.to_owned(),
                                            description: format!("{name} description"),
                                            when_to_use: None,
                                            invocation: SkillInvocationPolicy {
                                                model_invocable: true,
                                                user_invocable,
                                            },
                                            source: SkillSource("preset".to_owned()),
                                            provider: "preset".to_owned(),
                                            resource_base: None,
                                        },
                                        content: format!("{name} instructions"),
                                        path: None,
                                        metadata: None,
                                    },
                                )?;
                            }
                            Ok(())
                        })
                    },
                ),
            )
            .unwrap();
        catalog
            .register_named(
                "preset:layered-skill",
                Plugin::new(
                    "preset:layered-skill",
                    std::iter::empty::<&str>(),
                    move |plugin_context, _| {
                        let skills = layered_skills.clone();
                        Box::pin(async move {
                            skills.register(
                                &plugin_context,
                                SkillDefinition {
                                    summary: SkillSummary {
                                        name: "layered-owned".to_owned(),
                                        description: "layered-owned description".to_owned(),
                                        when_to_use: None,
                                        invocation: SkillInvocationPolicy {
                                            model_invocable: true,
                                            user_invocable: true,
                                        },
                                        source: SkillSource("preset".to_owned()),
                                        provider: "preset".to_owned(),
                                        resource_base: None,
                                    },
                                    content: "layered-owned instructions".to_owned(),
                                    path: None,
                                    metadata: None,
                                },
                            )?;
                            Ok(())
                        })
                    },
                ),
            )
            .unwrap();
        let roster = AgentPresetRegistry::new(
            &context,
            catalog,
            AgentPresetRegistryConfig {
                roster: AgentPresetConfig {
                    default: "standard".to_owned(),
                    roots: vec![
                        PresetRoot {
                            path: system_root.path().to_string_lossy().into_owned(),
                            trust: PresetTrust::System,
                        },
                        PresetRoot {
                            path: user_root.path().to_string_lossy().into_owned(),
                            trust: PresetTrust::User,
                        },
                    ],
                    include_user_root: false,
                },
                user_root: None,
            },
        )
        .unwrap();
        roster.provide(&context).unwrap();
        let open_paths = opened.clone();
        let runtime = PresetApiProxyRuntime::from_context(
            &context,
            PresetApiProxyOptions {
                default_model_selection: Arc::new(|| ModelSelection {
                    provider: "provider".to_owned(),
                    model: "model".to_owned(),
                    reasoning_effort: None,
                }),
                open_path: Some(Arc::new(move |path, _| {
                    open_paths.lock().push(path);
                    async { Ok(()) }.boxed()
                })),
                can_open_path: Some(Arc::new(move || can_open)),
                native_path_opener: PathOpenerInternals::default(),
            },
            Arc::new(TerminalDomains),
        );
        Self {
            context,
            sessions,
            agents,
            roster,
            runtime,
            _system_root: system_root,
            user_root,
        }
    }

    async fn live_agent(&self, id: &str, preset: &str) -> Arc<Agent> {
        let session = self
            .sessions
            .create(
                &self.context,
                Some(SessionId::new(id)),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    agent_preset: Some(preset.to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        let scope = create_scope(&self.context, ScopeKey::new(), None).unwrap();
        self.roster
            .mount(&scope.context, Some(preset))
            .await
            .unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent_context = scope.context.clone();
        let scope_key = seekdeep_scope::scope_of(&agent_context).unwrap();
        let agent = Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox,
            agent_context,
            scope_key,
        ));
        self.agents.register(&self.context, &agent, None).unwrap();
        agent
    }
}

async fn preset(root: &std::path::Path, id: &str, name: &str) {
    let directory = root.join(id);
    tokio::fs::create_dir_all(&directory).await.unwrap();
    tokio::fs::write(
        directory.join(COMPOSITION_FILE),
        concat!(
            "- id: capabilities\n",
            "  name: cordis:group\n",
            "  group: true\n",
            "  isolate:\n",
            "    goals: true\n",
            "    skills: true\n",
            "  config:\n",
            "    - id: provider\n",
            "      name: preset:capabilities\n",
            "- id: layered-skill\n",
            "  name: preset:layered-skill\n",
        ),
    )
    .await
    .unwrap();
    tokio::fs::write(
        directory.join("preset.yml"),
        format!("name: {name}\ndescription: {name} description\n"),
    )
    .await
    .unwrap();
}

async fn invoke(
    runtime: &PresetApiProxyRuntime,
    method: RpcMethod,
    payload: Value,
) -> RpcResult<Value> {
    runtime
        .unary(
            method,
            RpcRequest::new(RpcId::new("preset-test"), payload),
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

fn error_code(result: RpcResult<Value>) -> String {
    match result {
        RpcResult::Failure { error } => error.code,
        other @ RpcResult::Success { .. } => panic!("expected failure, got {other:?}"),
    }
}

#[tokio::test]
async fn list_marks_default_trust_authoring_and_native_document_capability() {
    let harness = Harness::new(true, Arc::new(Mutex::new(Vec::new()))).await;
    let listed = value(invoke(&harness.runtime, RpcMethod::AgentPresetList, json!({})).await);
    assert_eq!(listed["authorable"], true);
    assert_eq!(listed["hasDocument"], true);
    assert_eq!(listed["presets"].as_array().unwrap().len(), 2);
    assert_eq!(listed["presets"][0]["id"], "standard");
    assert_eq!(listed["presets"][0]["trust"], "system");
    assert_eq!(listed["presets"][0]["isDefault"], true);
    assert_eq!(listed["presets"][1]["trust"], "user");
}

#[tokio::test]
async fn read_copy_open_and_remove_use_only_host_resolved_ids_and_paths() {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let harness = Harness::new(true, opened.clone()).await;
    let read = value(
        invoke(
            &harness.runtime,
            RpcMethod::AgentPresetRead,
            json!({ "agentPreset": "standard" }),
        )
        .await,
    );
    assert_eq!(read["trust"], "system");
    assert!(
        read["content"]
            .as_str()
            .unwrap()
            .contains("name: preset:capabilities")
    );
    value(
        invoke(
            &harness.runtime,
            RpcMethod::AgentPresetCopy,
            json!({ "from": "standard", "agentPreset": "mine", "name": "Mine" }),
        )
        .await,
    );
    assert!(harness.user_root.path().join("mine").exists());
    value(
        invoke(
            &harness.runtime,
            RpcMethod::AgentPresetOpenDocument,
            json!({ "agentPreset": "mine" }),
        )
        .await,
    );
    assert_eq!(opened.lock().len(), 1);
    assert!(opened.lock()[0].ends_with("/mine"));
    assert_eq!(
        error_code(
            invoke(
                &harness.runtime,
                RpcMethod::AgentPresetOpenDocument,
                json!({ "agentPreset": "standard" }),
            )
            .await
        ),
        "agent-preset-read-only"
    );
    value(
        invoke(
            &harness.runtime,
            RpcMethod::AgentPresetRemove,
            json!({ "agentPreset": "mine" }),
        )
        .await,
    );
    assert!(!harness.user_root.path().join("mine").exists());
}

#[tokio::test]
async fn no_native_opener_returns_the_resolved_user_directory_as_text() {
    let harness = Harness::new(false, Arc::new(Mutex::new(Vec::new()))).await;
    let opened = value(
        invoke(
            &harness.runtime,
            RpcMethod::AgentPresetOpenDocument,
            json!({ "agentPreset": "minimal" }),
        )
        .await,
    );
    assert_eq!(opened["opened"], false);
    assert!(opened["path"].as_str().unwrap().ends_with("/minimal"));
}

#[tokio::test]
async fn blank_select_recomposes_and_records_while_started_or_unknown_selects_do_not() {
    let harness = Harness::new(false, Arc::new(Mutex::new(Vec::new()))).await;
    let agent = harness.live_agent("switch", "standard").await;
    let switched = value(
        invoke(
            &harness.runtime,
            RpcMethod::AgentPresetSelect,
            json!({ "sessionId": agent.id(), "agentPreset": "minimal" }),
        )
        .await,
    );
    assert_eq!(switched["agentPreset"], "minimal");
    assert_eq!(
        seekdeep_agent_presets::resolve_session_preset(
            agent.session().header(),
            &agent.session().events()
        )
        .as_deref(),
        Some("minimal")
    );
    assert_eq!(
        error_code(
            invoke(
                &harness.runtime,
                RpcMethod::AgentPresetSelect,
                json!({ "sessionId": agent.id(), "agentPreset": "missing" }),
            )
            .await
        ),
        "agent-preset-not-found"
    );
    agent
        .session()
        .append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    assert_eq!(
        error_code(
            invoke(
                &harness.runtime,
                RpcMethod::AgentPresetSelect,
                json!({ "sessionId": agent.id(), "agentPreset": "standard" }),
            )
            .await
        ),
        "agent-preset-locked"
    );
}

#[tokio::test]
async fn concurrent_blank_selects_serialize_and_log_the_committed_composition_order() {
    let harness = Harness::new(false, Arc::new(Mutex::new(Vec::new()))).await;
    let agent = harness.live_agent("switch-race", "standard").await;
    let first = invoke(
        &harness.runtime,
        RpcMethod::AgentPresetSelect,
        json!({ "sessionId": agent.id(), "agentPreset": "minimal" }),
    );
    let second = invoke(
        &harness.runtime,
        RpcMethod::AgentPresetSelect,
        json!({ "sessionId": agent.id(), "agentPreset": "standard" }),
    );
    let (first, second) = tokio::join!(first, second);
    value(first);
    value(second);
    let selected = agent
        .session()
        .events()
        .into_iter()
        .filter(|event| event.event_type == "agent-preset/selected")
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 2);
    let logged = resolve_session_preset(agent.session().header(), &agent.session().events())
        .expect("latest selected preset");
    assert_eq!(
        harness.roster.composed_preset(agent.context()).as_deref(),
        Some(logged.as_str())
    );
}

#[tokio::test]
async fn committed_selection_is_forwarded_to_the_host_remote_event_stream() {
    let harness = Harness::new(false, Arc::new(Mutex::new(Vec::new()))).await;
    let agent = harness.live_agent("remote-selection", "standard").await;
    let mut host = harness.runtime.host(
        RpcRequest::new(RpcId::new("host"), json!({})),
        AbortSignal::default(),
    );
    value(
        invoke(
            &harness.runtime,
            RpcMethod::AgentPresetSelect,
            json!({ "sessionId": agent.id(), "agentPreset": "minimal" }),
        )
        .await,
    );
    let frame = tokio::time::timeout(std::time::Duration::from_secs(1), host.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        frame.payload,
        HostFrame::RemoteEvent {
            event: "agent-preset/selected".to_owned(),
            args: vec![json!("remote-selection"), json!("minimal")],
        }
    );
}

#[tokio::test]
async fn skill_and_goal_rpcs_address_the_live_agents_isolated_preset_services() {
    let harness = Harness::new(false, Arc::new(Mutex::new(Vec::new()))).await;
    let agent = harness.live_agent("capabilities", "standard").await;
    assert!(harness.context.get(seekdeep_skill::SKILLS).is_some());
    assert!(harness.context.get(seekdeep_goal::GOAL).is_none());
    assert!(
        harness
            .roster
            .service_for(&agent, seekdeep_skill::SKILLS)
            .is_some()
    );
    assert!(
        harness
            .roster
            .service_for(&agent, seekdeep_goal::GOAL)
            .is_some()
    );

    let skills = value(
        invoke(
            &harness.runtime,
            RpcMethod::SkillList,
            json!({ "sessionId": agent.id() }),
        )
        .await,
    );
    assert_eq!(
        skills,
        json!({
            "skills": [{
                "name": "preset-owned",
                "description": "preset-owned description",
                "modelInvocable": true
            }]
        })
    );

    let created = value(
        invoke(
            &harness.runtime,
            RpcMethod::GoalCreate,
            json!({ "sessionId": agent.id(), "objective": "Finish the port" }),
        )
        .await,
    );
    assert_eq!(created["ref"]["revision"], 1);
    let paused = value(
        invoke(
            &harness.runtime,
            RpcMethod::GoalPause,
            json!({ "sessionId": agent.id(), "ref": created["ref"] }),
        )
        .await,
    );
    assert_eq!(paused["ref"]["id"], created["ref"]["id"]);
    assert_eq!(paused["ref"]["revision"], 2);
    let stale = error_code(
        invoke(
            &harness.runtime,
            RpcMethod::GoalComplete,
            json!({ "sessionId": agent.id(), "ref": created["ref"] }),
        )
        .await,
    );
    assert_eq!(stale, "internal");
}

#[tokio::test]
async fn skill_list_uses_the_recorded_standing_scope_for_an_attached_cold_session() {
    let harness = Harness::new(false, Arc::new(Mutex::new(Vec::new()))).await;
    let cold = harness
        .sessions
        .create(
            &harness.context,
            Some(SessionId::new("cold-skills")),
            CreateSessionOptions {
                cwd: Some("/project".to_owned()),
                agent_preset: Some("minimal".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    assert!(harness.agents.get(cold.id()).is_none());
    let skills = value(
        invoke(
            &harness.runtime,
            RpcMethod::SkillList,
            json!({ "sessionId": cold.id() }),
        )
        .await,
    );
    assert_eq!(skills["skills"][0]["name"], "layered-owned");
    assert!(harness.agents.get(cold.id()).is_none());

    let gone = harness
        .sessions
        .create(
            &harness.context,
            Some(SessionId::new("gone-skills")),
            CreateSessionOptions {
                cwd: Some("/project".to_owned()),
                agent_preset: Some("gone".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    let fallback = value(
        invoke(
            &harness.runtime,
            RpcMethod::SkillList,
            json!({ "sessionId": gone.id() }),
        )
        .await,
    );
    assert_eq!(fallback, json!({ "skills": [] }));
}

#[tokio::test]
async fn rosterless_deployment_lists_empty_and_refuses_management() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let runtime = PresetApiProxyRuntime::from_context(
        &context,
        PresetApiProxyOptions {
            default_model_selection: Arc::new(|| ModelSelection {
                provider: "p".to_owned(),
                model: "m".to_owned(),
                reasoning_effort: None,
            }),
            open_path: None,
            can_open_path: Some(Arc::new(|| false)),
            native_path_opener: PathOpenerInternals::default(),
        },
        Arc::new(TerminalDomains),
    );
    let listed = value(invoke(&runtime, RpcMethod::AgentPresetList, json!({})).await);
    assert_eq!(
        listed,
        json!({ "presets": [], "authorable": false, "hasDocument": false })
    );
    assert_eq!(
        error_code(
            invoke(
                &runtime,
                RpcMethod::AgentPresetRead,
                json!({ "agentPreset": "standard" }),
            )
            .await
        ),
        "agent-preset-not-found"
    );
    sessions
        .create(
            &context,
            Some(SessionId::new("no-skills")),
            CreateSessionOptions {
                cwd: Some("/project".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        error_code(
            invoke(
                &runtime,
                RpcMethod::SkillList,
                json!({ "sessionId": "no-skills" }),
            )
            .await,
        ),
        "internal"
    );
    drop((sessions, agents));
}
