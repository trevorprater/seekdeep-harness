//! Delegation routing, provider lifecycle, config, settlement, and jobs parity.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::{future::BoxFuture, stream};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, CreateAgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::{Context, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_core::session::SessionId;
use seekdeep_jobs::{JobRegistry, JobStatus};
use seekdeep_jobs_local::{Config as JobsConfig, LocalJobRegistry};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter,
    StreamChunk,
};
use seekdeep_scope::{ScopeKey, create_scope, scope_of};
use seekdeep_subagent::{
    ContinuableCreateRequest, ContinuableCreateSpec, ResolvedSubagentStartRequest,
    SubagentCapabilities, SubagentProvider, SubagentResult, SubagentRun, SubagentRuntime,
    SubagentStopReason,
};
use seekdeep_subagent_spawn_in_process::apply as apply_spawn;
use seekdeep_system_prompt::AssembleContext;
use seekdeep_tool_subagent::{
    BackgroundMode, Config, ConfigAgentOptions, MaxDepth, ProviderManaged, apply, plugin,
};
use seekdeep_tools::{ToolExecutionInput, ToolRestriction};
use serde_json::{Value, json};

#[derive(Clone)]
struct RunBehavior {
    result: SubagentResult,
    dispose_error: Option<String>,
}

struct ScriptedRun {
    id: SessionId,
    behavior: RunBehavior,
    disposals: Arc<AtomicUsize>,
}

impl SubagentRun for ScriptedRun {
    fn id(&self) -> &SessionId {
        &self.id
    }

    fn local_agent(&self) -> Option<&Arc<Agent>> {
        None
    }

    fn result(&self) -> BoxFuture<'static, SubagentResult> {
        let result = self.behavior.result.clone();
        Box::pin(async move { result })
    }

    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        self.disposals.fetch_add(1, Ordering::SeqCst);
        let error = self.behavior.dispose_error.clone();
        Box::pin(async move {
            match error {
                Some(error) => Err(anyhow::anyhow!(error)),
                None => Ok(()),
            }
        })
    }
}

struct ScriptedProvider {
    name: String,
    capabilities: SubagentCapabilities,
    inherits: bool,
    continuable: bool,
    behavior: Mutex<RunBehavior>,
    seen: Mutex<Vec<ResolvedSubagentStartRequest>>,
    starts: AtomicUsize,
    disposals: Arc<AtomicUsize>,
}

impl ScriptedProvider {
    fn new(name: &str, reply: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            capabilities: SubagentCapabilities {
                depth_limit: true,
                tool_filter: true,
                persona: true,
                ..SubagentCapabilities::default()
            },
            inherits: false,
            continuable: false,
            behavior: Mutex::new(RunBehavior {
                result: SubagentResult {
                    output: vec![ContentBlock::Text {
                        text: reply.to_owned(),
                    }],
                    structured: None,
                    stop_reason: SubagentStopReason::Completed,
                },
                dispose_error: None,
            }),
            seen: Mutex::new(Vec::new()),
            starts: AtomicUsize::new(0),
            disposals: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn capless(name: &str, reply: &str) -> Arc<Self> {
        let mut provider = Self::new(name, reply);
        Arc::get_mut(&mut provider).unwrap().capabilities = SubagentCapabilities::default();
        provider
    }

    fn inherited(name: &str, reply: &str) -> Arc<Self> {
        let mut provider = Self::new(name, reply);
        Arc::get_mut(&mut provider).unwrap().inherits = true;
        provider
    }

    fn continuable(name: &str, reply: &str) -> Arc<Self> {
        let mut provider = Self::new(name, reply);
        Arc::get_mut(&mut provider).unwrap().continuable = true;
        provider
    }

    fn set_behavior(&self, stop_reason: SubagentStopReason, dispose_error: Option<&str>) {
        *self.behavior.lock() = RunBehavior {
            result: SubagentResult {
                output: vec![ContentBlock::Text {
                    text: "partial".to_owned(),
                }],
                structured: None,
                stop_reason,
            },
            dispose_error: dispose_error.map(str::to_owned),
        };
    }
}

#[async_trait]
impl SubagentProvider for ScriptedProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    fn inherits_parent_context(&self) -> bool {
        self.inherits
    }

    fn supports_continuable(&self) -> bool {
        self.continuable
    }

    async fn start(
        &self,
        request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        if request.request.signal.is_aborted() {
            anyhow::bail!("scripted provider start aborted");
        }
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().push(request);
        Ok(Arc::new(ScriptedRun {
            id: SessionId::new(format!("{}-child", self.name)),
            behavior: self.behavior.lock().clone(),
            disposals: Arc::clone(&self.disposals),
        }))
    }

    async fn prepare_continuable(
        &self,
        _request: ContinuableCreateRequest,
    ) -> anyhow::Result<ContinuableCreateSpec> {
        if self.continuable {
            Ok(ContinuableCreateSpec::default())
        } else {
            anyhow::bail!("continuable preparation unsupported")
        }
    }
}

struct Harness {
    context: Context,
    dependencies: AgentLoopTestDependencies,
    subagents: Arc<SubagentRuntime>,
    parent: Arc<Agent>,
}

impl Harness {
    fn new() -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .unwrap();
        let subagents = SubagentRuntime::install(&context).unwrap();
        let session = dependencies
            .sessions
            .create(
                &context,
                Some(SessionId::new("parent")),
                seekdeep_core::session_store::CreateSessionOptions::default(),
            )
            .unwrap();
        let scope = create_scope(&context, ScopeKey::new(), None).unwrap();
        let scope_key = scope_of(&scope.context).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let parent = Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox,
            scope.context,
            scope_key,
        ));
        dependencies
            .agents
            .register(&context, &parent, None)
            .unwrap();
        Self {
            context,
            dependencies,
            subagents,
            parent,
        }
    }

    fn mount(&self, provider: Arc<ScriptedProvider>) -> EffectHandle {
        let provider: Arc<dyn SubagentProvider> = provider;
        self.subagents.register_provider(provider).unwrap()
    }

    async fn call(&self, name: &str, arguments: Value) -> seekdeep_tools::ToolExecutionResult {
        self.dependencies
            .tools
            .execute(
                ToolExecutionInput::new(
                    CallId::new(format!("call-{name}")),
                    name,
                    arguments,
                    AbortSignal::default(),
                )
                .with_agent(self.parent.clone()),
            )
            .await
    }
}

fn config(provider: &str) -> Config {
    Config {
        provider: provider.to_owned(),
        tool_name: None,
        enable_run_in_background: None,
        background_mode: None,
        agent_options: None,
        persona: None,
        tool_filter: None,
        max_depth: Some(MaxDepth::ProviderManaged(ProviderManaged::Value)),
    }
}

fn text(result: &seekdeep_tools::ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn foreground_delegation_schema_config_forwarding_and_disposal_match_the_source() {
    let harness = Harness::new();
    let provider = ScriptedProvider::new("mock", "scripted reply");
    let _provider_effect = harness.mount(provider.clone());
    let mut resolved = config("mock");
    resolved.agent_options = Some(ConfigAgentOptions {
        provider: Some("child-provider".to_owned()),
        model: Some("child-model".to_owned()),
        max_tokens: Some(123),
    });
    resolved.persona = Some("reviewer".to_owned());
    resolved.tool_filter = Some(ToolRestriction {
        allow: None,
        deny: Some(vec!["bash".to_owned()]),
    });
    apply(&harness.context, resolved).unwrap();

    let schema = harness
        .dependencies
        .tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == "subagent")
        .unwrap();
    assert_eq!(
        schema.parameters["required"],
        json!(["description", "prompt"])
    );
    assert_eq!(
        schema.parameters["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["description", "prompt", "run_in_background"]
    );
    assert!(
        schema
            .description
            .contains("does not see this conversation")
    );
    let definition = harness.dependencies.tools.get("subagent", None).unwrap();
    assert!(definition.is_concurrency_safe.as_ref().unwrap()(&json!({
        "description": "review code",
        "prompt": "review it"
    })));

    let result = harness
        .call(
            "subagent",
            json!({ "description": "review code", "prompt": "review it" }),
        )
        .await;
    assert!(!result.is_error());
    assert_eq!(text(&result), "scripted reply");
    assert_eq!(result.value().unwrap()["kind"], "foreground");
    assert_eq!(provider.disposals.load(Ordering::SeqCst), 1);
    let seen = provider.seen.lock();
    let request = &seen[0].request;
    assert_eq!(request.label.as_deref(), Some("review code"));
    assert_eq!(request.persona.as_deref(), Some("reviewer"));
    assert_eq!(request.max_depth, None);
    assert_eq!(request.tool_filter.as_ref().unwrap().allow, None);
    assert_eq!(
        request
            .agent_options
            .as_ref()
            .unwrap()
            .model
            .as_ref()
            .unwrap()
            .as_str(),
        "child-model"
    );
}

#[tokio::test]
async fn abnormal_foreground_output_is_an_error_and_disposal_failure_stays_independent() {
    let harness = Harness::new();
    let provider = ScriptedProvider::new("mock", "unused");
    provider.set_behavior(SubagentStopReason::MaxTokens, Some("dispose exploded"));
    let _provider_effect = harness.mount(provider.clone());
    apply(&harness.context, config("mock")).unwrap();
    let result = harness
        .call(
            "subagent",
            json!({ "description": "long task", "prompt": "go" }),
        )
        .await;
    assert!(result.is_error());
    let rendered = text(&result);
    assert!(rendered.contains("hit its token limit"));
    assert!(rendered.contains("Partial output before the run ended:\npartial"));
    assert!(rendered.contains("dispose failed: dispose exploded"));
    assert_eq!(provider.disposals.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn provider_lifecycle_controls_registration_and_rederives_wording() {
    let harness = Harness::new();
    apply(&harness.context, config("late")).unwrap();
    assert!(harness.dependencies.tools.get("subagent", None).is_none());

    let fresh = ScriptedProvider::new("late", "first");
    let fresh_effect = harness.mount(fresh);
    assert!(
        harness
            .dependencies
            .tools
            .get("subagent", None)
            .unwrap()
            .description
            .contains("does not see this conversation")
    );
    fresh_effect.dispose().await.unwrap();
    assert!(harness.dependencies.tools.get("subagent", None).is_none());

    let inherited = ScriptedProvider::inherited("late", "second");
    let _inherited_effect = harness.mount(inherited);
    assert!(
        harness
            .dependencies
            .tools
            .get("subagent", None)
            .unwrap()
            .description
            .contains("inherits this conversation")
    );
}

#[tokio::test]
async fn disabled_background_and_capability_misconfiguration_fail_at_the_owned_boundary() {
    let harness = Harness::new();
    let provider = ScriptedProvider::capless("capless", "ok");
    let _provider_effect = harness.mount(provider);
    let mut disabled = config("capless");
    disabled.enable_run_in_background = Some(false);
    apply(&harness.context, disabled).unwrap();
    let schema = harness.dependencies.tools.get("subagent", None).unwrap();
    assert!(
        schema.parameters["properties"]
            .get("run_in_background")
            .is_none()
    );
    let forced = harness
        .call(
            "subagent",
            json!({
                "description": "forced background",
                "prompt": "go",
                "run_in_background": true
            }),
        )
        .await;
    assert!(forced.is_error());
    assert!(text(&forced).contains("run_in_background is disabled"));

    let second = Harness::new();
    let provider = ScriptedProvider::capless("capless", "ok");
    let _provider_effect = second.mount(provider);
    let mut numeric = config("capless");
    numeric.max_depth = Some(MaxDepth::Numeric(3));
    let error = apply(&second.context, numeric).unwrap_err().to_string();
    assert!(error.contains("no depthLimit capability"));

    let third = Harness::new();
    let provider = ScriptedProvider::new("plain", "ok");
    let _provider_effect = third.mount(provider);
    let mut continuable = config("plain");
    continuable.background_mode = Some(BackgroundMode::Continuable);
    let error = apply(&third.context, continuable).unwrap_err().to_string();
    assert!(error.contains("does not support `backgroundMode: continuable`"));
}

#[tokio::test]
async fn one_shot_background_returns_a_job_and_collection_yields_the_child_output() {
    let harness = Harness::new();
    let jobs = LocalJobRegistry::new(&harness.context, JobsConfig::default()).unwrap();
    let _controller = jobs.attach_controller("tool-jobs");
    let provider = ScriptedProvider::new("mock", "background answer");
    let _provider_effect = harness.mount(provider.clone());
    apply(&harness.context, config("mock")).unwrap();

    let started = harness
        .call(
            "subagent",
            json!({
                "description": "background task",
                "prompt": "go",
                "run_in_background": true
            }),
        )
        .await;
    assert!(!started.is_error());
    assert!(text(&started).starts_with("started background subagent task subagent-"));
    let id = seekdeep_jobs::JobId::new(
        started.value().unwrap()["jobId"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let settled = jobs
        .wait(&id, 5_000.0, Some(&harness.parent), None)
        .await
        .unwrap();
    assert_eq!(settled.status, JobStatus::Completed);
    let read = jobs.read(&id, Some(&harness.parent)).unwrap();
    assert_eq!(read.text, "background answer");
    assert_eq!(provider.disposals.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn continuable_policy_defaults_to_background_guidance_and_allows_foreground_override() {
    let harness = Harness::new();
    let provider = ScriptedProvider::continuable("continuable", "foreground answer");
    let _provider_effect = harness.mount(provider);
    let mut resolved = config("continuable");
    resolved.background_mode = Some(BackgroundMode::Continuable);
    apply(&harness.context, resolved).unwrap();
    let schema = harness.dependencies.tools.get("subagent", None).unwrap();
    assert!(schema.description.contains("background by default"));
    assert!(
        schema.parameters["properties"]["run_in_background"]["description"]
            .as_str()
            .unwrap()
            .contains("Defaults to true")
    );
    let assembly = harness
        .dependencies
        .system_prompt
        .assemble(AssembleContext::default())
        .await
        .unwrap();
    assert!(
        assembly
            .sections
            .iter()
            .any(|section| section.name == "tool:subagent"
                && section.text.contains("independent delegations"))
    );
    let result = harness
        .call(
            "subagent",
            json!({
                "description": "blocking child",
                "prompt": "go",
                "run_in_background": false
            }),
        )
        .await;
    assert_eq!(text(&result), "foreground answer");
}

#[tokio::test]
async fn loader_defaults_depth_to_three_and_plugin_disposal_prevents_zombie_mounts() {
    let harness = Harness::new();
    let provider = ScriptedProvider::new("mock", "ok");
    let _provider_effect = harness.mount(provider.clone());
    let mounted = harness
        .context
        .plugin(plugin(), json!({ "provider": "mock" }))
        .unwrap();
    mounted.await_settled().await.unwrap();
    harness
        .call(
            "subagent",
            json!({ "description": "depth check", "prompt": "go" }),
        )
        .await;
    assert_eq!(provider.seen.lock()[0].request.max_depth, Some(3));
    mounted.dispose().await.unwrap();
    assert!(harness.dependencies.tools.get("subagent", None).is_none());

    let waiting = harness
        .context
        .plugin(
            plugin(),
            json!({
                "provider": "later",
                "toolName": "subagent_later",
                "maxDepth": "provider-managed"
            }),
        )
        .unwrap();
    waiting.await_settled().await.unwrap();
    waiting.dispose().await.unwrap();
    let late = ScriptedProvider::capless("later", "late");
    let _late_effect = harness.mount(late);
    assert!(
        harness
            .dependencies
            .tools
            .get("subagent_later", None)
            .is_none()
    );
}

struct AnswerAdapter;

#[async_trait]
impl LlmAdapter for AnswerAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::TextDelta {
                index: 0,
                text: "real spawned child answer".to_owned(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

#[tokio::test]
async fn assembled_tool_runtime_delegates_through_spawn_to_a_real_child_agent_loop() {
    let context = Context::new();
    let dependencies =
        mount_agent_loop_test_dependencies(&context, AgentLoopTestDependenciesOptions::default())
            .unwrap();
    dependencies
        .llm
        .register_adapter(&["mock".to_owned()], Arc::new(AnswerAdapter))
        .unwrap();
    let factory = AgentLoop::new(
        context.clone(),
        dependencies.sessions.clone(),
        dependencies.agents.as_ref().clone(),
        AgentLoopServices {
            llm: dependencies.llm.clone(),
            system_prompt: dependencies.system_prompt.clone(),
            tools: dependencies.tools.clone(),
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
        },
    )
    .unwrap();
    dependencies.agents.set_factory(Arc::new(factory)).unwrap();
    SubagentRuntime::install(&context).unwrap();
    apply_spawn(
        &context,
        seekdeep_subagent_spawn_in_process::Config::default(),
    )
    .unwrap();
    let mut parent_options = CreateAgentOptions::new(SessionId::new("assembled-parent"));
    parent_options.agent_options = AgentOptions {
        provider: Some(seekdeep_llm::ProviderId::new("mock")),
        model: Some(seekdeep_llm::ModelId::new("mock")),
        max_tokens: None,
        subagent_depth: None,
    };
    let parent = dependencies.agents.create(parent_options).await.unwrap();
    apply(&context, config("spawn")).unwrap();
    let starts = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&starts);
    context
        .events()
        .on_sync(
            &context,
            "subagent/start",
            move |_, args| {
                let info = args
                    .get::<seekdeep_subagent::SubagentRunInfo>(0)
                    .ok_or_else(|| anyhow::anyhow!("missing start info"))?;
                observed.lock().push((*info).clone());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();

    let result = dependencies
        .tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("assembled-call"),
                "subagent",
                json!({ "description": "real child", "prompt": "answer" }),
                AbortSignal::default(),
            )
            .with_agent(parent.agent.clone()),
        )
        .await;
    assert!(!result.is_error());
    assert_eq!(text(&result), "real spawned child answer");
    assert_eq!(starts.lock().len(), 1);
    assert_eq!(starts.lock()[0].provider, "spawn");
    assert!(starts.lock()[0].local);
    assert_ne!(starts.lock()[0].id, *parent.agent.id());
    assert_eq!(dependencies.agents.list().len(), 1);
    parent.dispose().await.unwrap();
}
