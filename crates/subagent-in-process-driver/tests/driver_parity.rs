//! Real agent-loop coverage for publication, metadata, settlement, and structured capture.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{AgentHandle, AgentOptions, CreateAgentOptions};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::{AppendOptions, SessionEvent, SessionId, SessionOrigin};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter,
    ModelId, ProviderId, StreamChunk,
};
use seekdeep_subagent::{
    ResolvedSubagentStartRequest, SubagentDescriptorInput, SubagentStartRequest,
    SubagentStopReason, snapshot_subagent_descriptor,
};
use seekdeep_subagent_in_process_driver::{
    InProcessRunOptions, STRUCTURED_OUTPUT_TOOL, start_in_process_run,
};
use seekdeep_tools::{ContentToolFixtureOptions, define_content_tool_fixture};
use serde_json::{Value, json};

enum Reply {
    Text(String),
    Structured(String),
    ToolCall {
        id: &'static str,
        name: &'static str,
        arguments: String,
    },
}

struct ScriptedAdapter {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        let chunks = match self.replies.lock().pop_front().expect("script reply") {
            Reply::Text(text) => vec![
                Ok(StreamChunk::TextDelta { index: 0, text }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ],
            Reply::Structured(arguments) => vec![
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("structured-call"),
                        name: STRUCTURED_OUTPUT_TOOL.to_owned(),
                        arguments,
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                }),
            ],
            Reply::ToolCall {
                id,
                name,
                arguments,
            } => vec![
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new(id),
                        name: name.to_owned(),
                        arguments,
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                }),
            ],
        };
        AdapterStream::new(stream::iter(chunks))
    }
}

struct Harness {
    context: Context,
    dependencies: AgentLoopTestDependencies,
    parent: AgentHandle,
    adapter: Arc<ScriptedAdapter>,
}

impl Harness {
    async fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self::new_with_cwd(replies, "/project".to_owned()).await
    }

    async fn new_with_cwd(replies: impl IntoIterator<Item = Reply>, cwd: String) -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .unwrap();
        let adapter = Arc::new(ScriptedAdapter {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        });
        dependencies
            .llm
            .register_adapter(&["mock".to_owned()], adapter.clone())
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
        let mut parent_options = CreateAgentOptions::new(SessionId::new("parent"));
        parent_options.meta.cwd = Some(cwd);
        parent_options.agent_options = AgentOptions {
            provider: Some(ProviderId::new("mock")),
            model: Some(ModelId::new("mock")),
            max_tokens: Some(321),
            subagent_depth: None,
        };
        let parent = dependencies.agents.create(parent_options).await.unwrap();
        Self {
            context,
            dependencies,
            parent,
            adapter,
        }
    }

    fn request(&self, signal: AbortSignal) -> ResolvedSubagentStartRequest {
        ResolvedSubagentStartRequest {
            request: SubagentStartRequest {
                label: Some("child task".to_owned()),
                prompt: vec![ContentBlock::Text {
                    text: "do the task".to_owned(),
                }],
                parent: self.parent.agent.clone(),
                signal,
                agent_options: None,
                output_schema: None,
                max_depth: Some(3),
                tool_filter: None,
                persona: None,
            },
            descriptor: snapshot_subagent_descriptor(&SubagentDescriptorInput::OneShot {
                provider: "spawn".to_owned(),
                label: Some("child task".to_owned()),
            })
            .unwrap(),
        }
    }
}

fn text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn assert_inherited_denial_events(events: &[SessionEvent]) {
    let inherited_policy = events
        .iter()
        .find(|event| event.event_type == "sandbox/mode")
        .expect("child inherits the parent sandbox override");
    assert_eq!(
        inherited_policy.data,
        json!({"mode":"read-only","source":"delegation"})
    );
    let denial = events
        .iter()
        .find(|event| {
            event.event_type == "tool/result"
                && event.data.to_string().contains("FS_SANDBOX_DENIED")
        })
        .expect("child log contains the model-facing filesystem denial");
    assert!(
        denial
            .data
            .to_string()
            .contains("[sandbox: file access denied under read-only mode]")
    );
}

fn assert_inherited_policy_request(adapter: &ScriptedAdapter) {
    let requests = adapter.requests.lock();
    assert_eq!(requests.len(), 2);
    let first_context = requests[0]
        .messages
        .iter()
        .flat_map(seekdeep_llm::Message::content)
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(first_context.contains(
        "Any available operation enforced by the SeekDeep file sandbox cannot modify files in the standing mode."
    ));
    assert!(first_context.contains("You are a delegated subagent:"));
    assert!(
        requests[0]
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .any(|tool| tool.name == "write")
    );
}

#[tokio::test]
async fn published_run_begins_its_initial_prompt_before_returning() {
    let harness = Harness::new([Reply::Text("child answer".to_owned())]).await;
    let run = start_in_process_run(
        harness.request(AbortSignal::default()),
        InProcessRunOptions::default(),
    )
    .await
    .unwrap();

    assert!(
        run.local_agent()
            .unwrap()
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "agent/inbox/spliced")
    );
    assert_eq!(harness.adapter.requests.lock().len(), 1);
    assert_eq!(
        run.result().await.unwrap().stop_reason,
        SubagentStopReason::Completed
    );

    run.dispose().await.unwrap();
    harness.parent.dispose().await.unwrap();
    harness.context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn published_fresh_child_inherits_route_cwd_depth_and_disposes_quiescently() {
    let harness = Harness::new([Reply::Text("child answer".to_owned())]).await;
    harness
        .parent
        .agent
        .session()
        .append(
            "agent-preset/selected",
            json!({ "agentPreset": "reviewing" }),
            AppendOptions::default(),
        )
        .unwrap();
    let preset_tool = define_content_tool_fixture(ContentToolFixtureOptions::new(
        "preset_tool",
        "Preset-scoped test tool.",
        json!({}),
        Arc::new(|_args: Value, _run| {
            Box::pin(async {
                Ok(vec![ContentBlock::Text {
                    text: "preset".to_owned(),
                }])
            })
        }),
    ))
    .unwrap();
    harness
        .dependencies
        .tools
        .register(harness.parent.agent.context(), preset_tool)
        .unwrap();
    let run = start_in_process_run(
        harness.request(AbortSignal::default()),
        InProcessRunOptions::default(),
    )
    .await
    .unwrap();
    let live = harness.dependencies.agents.get(run.id()).unwrap();
    assert!(Arc::ptr_eq(run.local_agent().unwrap(), &live));
    assert_ne!(live.id(), harness.parent.agent.id());
    assert_eq!(
        live.session().header().parent_session.as_ref(),
        Some(harness.parent.agent.id())
    );
    assert_eq!(live.session().header().cwd.as_deref(), Some("/project"));
    assert_eq!(
        live.session().header().origin,
        Some(SessionOrigin::Subagent)
    );
    assert_eq!(live.session().header().delegation_depth, Some(1));
    assert_eq!(
        live.session().header().agent_preset.as_deref(),
        Some("reviewing")
    );
    assert_eq!(live.options().subagent_depth, Some(1));
    assert_eq!(live.options().max_tokens, Some(321));

    let result = run.result().await.unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(text(&result.output), "child answer");
    let events = live.session().events();
    let descriptor = events
        .iter()
        .position(|event| event.event_type == "subagent/descriptor")
        .unwrap();
    let request = events
        .iter()
        .position(|event| event.event_type == "request/header")
        .unwrap();
    assert!(descriptor < request);
    assert_eq!(harness.adapter.requests.lock().len(), 1);
    assert!(
        harness.adapter.requests.lock()[0]
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .any(|tool| tool.name == "preset_tool")
    );

    run.dispose().await.unwrap();
    assert!(harness.dependencies.agents.get(run.id()).is_none());
    harness.parent.dispose().await.unwrap();
    harness.context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn inherited_read_only_policy_confines_a_real_child_write_under_a_wider_default() {
    let workspace = tempfile::tempdir().unwrap();
    let inherited_path = workspace.path().join("inherited.txt");
    let harness = Harness::new_with_cwd(
        [
            Reply::ToolCall {
                id: "child-write",
                name: "write",
                arguments: json!({
                    "file_path": "inherited.txt",
                    "content": "escaped"
                })
                .to_string(),
            },
            Reply::Text(
                "CHILD_DENIED [sandbox: file access denied under read-only mode]".to_owned(),
            ),
        ],
        workspace.path().to_string_lossy().into_owned(),
    )
    .await;
    let _policy = seekdeep_sandbox_policy::install(
        &harness.context,
        seekdeep_sandbox_policy::SandboxPolicyConfig {
            mode: seekdeep_sandbox::SandboxMode::WorkspaceWrite,
            workspace_root: Some(workspace.path().to_owned()),
        },
    )
    .unwrap();
    seekdeep_fs_sandbox::apply(
        &harness.context,
        seekdeep_fs_local::Config {
            cwd: Some(workspace.path().to_string_lossy().into_owned()),
            ..seekdeep_fs_local::Config::default()
        },
    )
    .unwrap();
    seekdeep_fs_observation_policy::apply(&harness.context).unwrap();
    seekdeep_tool_fs::apply(&harness.context, &seekdeep_tool_fs::Config::default()).unwrap();
    seekdeep_sandbox_policy::set_sandbox_mode(
        harness.parent.agent.session(),
        seekdeep_sandbox::SandboxMode::ReadOnly,
    )
    .unwrap();

    let run = start_in_process_run(
        harness.request(AbortSignal::default()),
        InProcessRunOptions::default(),
    )
    .await
    .unwrap();
    let live = harness.dependencies.agents.get(run.id()).unwrap();
    let result = run.result().await.unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(
        text(&result.output),
        "CHILD_DENIED [sandbox: file access denied under read-only mode]"
    );
    assert!(!inherited_path.exists());

    assert_inherited_denial_events(&live.session().events());
    assert_inherited_policy_request(&harness.adapter);

    run.dispose().await.unwrap();
    harness.parent.dispose().await.unwrap();
    harness.context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn prepublication_abort_and_depth_rejection_publish_nothing() {
    let harness = Harness::new([]).await;
    let before_agents = harness.dependencies.agents.list().len();
    let before_sessions = harness.dependencies.sessions.list().len();
    let aborted = AbortSignal::default();
    aborted.abort();
    let error = match start_in_process_run(harness.request(aborted), InProcessRunOptions::default())
        .await
    {
        Ok(_) => panic!("pre-aborted start must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("aborted before child publication"));

    let mut request = harness.request(AbortSignal::default());
    request.request.max_depth = Some(0);
    let error = match start_in_process_run(request, InProcessRunOptions::default()).await {
        Ok(_) => panic!("exceeded depth must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("subagent depth 1 exceeds maxDepth 0"));
    assert_eq!(harness.dependencies.agents.list().len(), before_agents);
    assert_eq!(harness.dependencies.sessions.list().len(), before_sessions);
}

#[tokio::test]
async fn structured_child_commits_the_exact_value_and_concludes_without_an_extra_step() {
    let harness = Harness::new([Reply::Structured(r#"{"answer":42}"#.to_owned())]).await;
    let mut request = harness.request(AbortSignal::default());
    request.request.output_schema = Some(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": { "answer": { "type": "number" } },
        "required": ["answer"]
    }));
    let run = start_in_process_run(request, InProcessRunOptions::default())
        .await
        .unwrap();
    let result = run.result().await.unwrap();
    assert_eq!(result.stop_reason, SubagentStopReason::Completed);
    assert_eq!(result.structured, Some(json!({ "answer": 42 })));
    assert_eq!(harness.adapter.requests.lock().len(), 1);
    run.dispose().await.unwrap();
}
