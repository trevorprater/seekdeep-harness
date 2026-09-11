//! The continuable-stack harness the pinned subagent specs boot: agent-loop
//! test dependencies, JSONL session persistence, the agent loop factory, the
//! subagent runtime, and both in-process providers, driven by a scripted
//! model adapter whose entries may wait on a caller-released gate.

#![allow(dead_code, reason = "shared by several spec ports")]

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use async_trait::async_trait;
use futures::{FutureExt, StreamExt, stream};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentHandle, AgentOptions, AgentRegistry, CreateAgentOptions};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::{SessionEvent, SessionId};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter,
    MessageId, MessageSource, ModelId, ProviderId, StreamChunk, TokenUsage,
};
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionInspection};
use seekdeep_session_persistence_jsonl::JsonlConfig;
use seekdeep_subagent::{
    ContinuableStart, ContinuableStartRequest, ContinuableStartSpec, SubagentFollowupOptions,
    SubagentRuntime,
};
use tokio::sync::oneshot;

/// One scripted model reply.
pub(crate) enum Entry {
    /// Stream these chunks, optionally after a caller-released gate.
    Chunks {
        chunks: Vec<StreamChunk>,
        gate: Option<oneshot::Receiver<()>>,
    },
    /// Stream one partial block, then wait until the request is aborted.
    Hang,
}

impl Entry {
    pub(crate) fn chunks(chunks: Vec<StreamChunk>) -> Self {
        Self::Chunks { chunks, gate: None }
    }

    pub(crate) fn gated(chunks: Vec<StreamChunk>) -> (Self, Release) {
        let (sender, receiver) = oneshot::channel();
        (
            Self::Chunks {
                chunks,
                gate: Some(receiver),
            },
            Release(Some(sender)),
        )
    }
}

/// The caller's side of a gated entry (`Promise.withResolvers` resolve).
pub(crate) struct Release(Option<oneshot::Sender<()>>);

impl Release {
    /// Release the gated model call; releasing twice or after the stream was
    /// dropped is harmless.
    pub(crate) fn open_sender(mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

/// The scripted adapter: each model call consumes the next entry and every
/// request is recorded for assertions.
pub(crate) struct MockAdapter {
    script: Mutex<VecDeque<Entry>>,
    pub(crate) requests: Mutex<Vec<GenerateOptions>>,
    /// Request indexes whose activity signal aborted, keyed by the abort
    /// sequence number: the observable trace of `Agent.cancel` the source
    /// spies on directly.
    pub(crate) cancelled: Arc<Mutex<Vec<(u64, usize)>>>,
}

impl MockAdapter {
    pub(crate) fn new(script: Vec<Entry>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            cancelled: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Cancelled request indexes in cancellation order.
    pub(crate) fn cancelled(&self) -> Vec<usize> {
        let mut cancelled = self.cancelled.lock().clone();
        cancelled.sort_unstable();
        cancelled.into_iter().map(|(_, index)| index).collect()
    }

    pub(crate) fn request_count(&self) -> usize {
        self.requests.lock().len()
    }
}

#[async_trait]
impl LlmAdapter for MockAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        let signal = options.signal.clone().unwrap_or_default();
        let index = {
            let mut requests = self.requests.lock();
            requests.push(options);
            requests.len() - 1
        };
        {
            let cancelled = Arc::clone(&self.cancelled);
            let watched = signal.clone();
            tokio::spawn(async move {
                watched.cancelled().await;
                cancelled
                    .lock()
                    .push((watched.abort_order().unwrap_or(u64::MAX), index));
            });
        }
        let entry = self.script.lock().pop_front();
        match entry {
            None => AdapterStream::new(stream::once(async {
                Err(anyhow::anyhow!("MockAdapter: script exhausted"))
            })),
            Some(Entry::Hang) => {
                let head = stream::iter(vec![
                    Ok(StreamChunk::BlockStart {
                        index: 0,
                        block_type: "text".to_owned(),
                    }),
                    Ok(StreamChunk::TextDelta {
                        index: 0,
                        text: "partial".to_owned(),
                    }),
                ]);
                let tail = stream::once(async move {
                    signal.cancelled().await;
                    Err(anyhow::anyhow!("aborted"))
                });
                AdapterStream::new(head.chain(tail))
            }
            Some(Entry::Chunks { chunks, gate }) => {
                let gated = async move {
                    if let Some(gate) = gate {
                        let _ = gate.await;
                    }
                    stream::iter(chunks).map(move |chunk| {
                        if signal.is_aborted() {
                            Err(anyhow::anyhow!("aborted"))
                        } else {
                            Ok(chunk)
                        }
                    })
                }
                .flatten_stream();
                AdapterStream::new(gated)
            }
        }
    }
}

fn usage(output_tokens: u64) -> TokenUsage {
    TokenUsage {
        input_tokens: 10,
        output_tokens,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    }
}

/// The source `textResponse`: one text block streamed per character.
pub(crate) fn text_response(text: &str) -> Vec<StreamChunk> {
    let mut chunks = vec![StreamChunk::BlockStart {
        index: 0,
        block_type: "text".to_owned(),
    }];
    chunks.extend(text.chars().map(|character| StreamChunk::TextDelta {
        index: 0,
        text: character.to_string(),
    }));
    chunks.push(StreamChunk::BlockEnd {
        index: 0,
        block: ContentBlock::Text { text: text.into() },
    });
    chunks.push(StreamChunk::Usage {
        usage: usage(text.chars().count() as u64),
    });
    chunks.push(StreamChunk::Finish {
        reason: FinishReason::Stop,
        replay_state: None,
    });
    chunks
}

/// The source `maxTokensResponse`: a text block cut off at the output ceiling.
pub(crate) fn max_tokens_response(text: &str) -> Vec<StreamChunk> {
    let mut chunks = text_response(text);
    chunks.pop();
    chunks.push(StreamChunk::Finish {
        reason: FinishReason::MaxTokens,
        replay_state: None,
    });
    chunks
}

/// The source `toolCallResponse`: optional text, then one streamed tool call.
pub(crate) fn tool_call_response(
    raw_call_id: &str,
    name: &str,
    arguments: &serde_json::Value,
    text: Option<&str>,
) -> Vec<StreamChunk> {
    let call_id = CallId::new(raw_call_id);
    let arguments_json = serde_json::to_string(arguments).unwrap();
    let mut chunks = Vec::new();
    let mut index = 0;
    if let Some(text) = text {
        chunks.push(StreamChunk::BlockStart {
            index,
            block_type: "text".to_owned(),
        });
        chunks.push(StreamChunk::TextDelta {
            index,
            text: text.to_owned(),
        });
        chunks.push(StreamChunk::BlockEnd {
            index,
            block: ContentBlock::Text { text: text.into() },
        });
        index += 1;
    }
    let (head, tail) = arguments_json.split_at(arguments_json.len().min(5));
    chunks.push(StreamChunk::BlockStart {
        index,
        block_type: "tool-call".to_owned(),
    });
    chunks.push(StreamChunk::ToolCallDelta {
        index,
        id: call_id.clone(),
        name: Some(name.to_owned()),
        arguments_delta: head.to_owned(),
    });
    chunks.push(StreamChunk::ToolCallDelta {
        index,
        id: call_id.clone(),
        name: None,
        arguments_delta: tail.to_owned(),
    });
    chunks.push(StreamChunk::BlockEnd {
        index,
        block: ContentBlock::ToolCall {
            id: call_id,
            name: name.to_owned(),
            arguments: arguments_json,
        },
    });
    chunks.push(StreamChunk::Usage { usage: usage(5) });
    chunks.push(StreamChunk::Finish {
        reason: FinishReason::ToolCalls,
        replay_state: None,
    });
    chunks
}

/// Keep the top-level test parent out of a scripted model corpus: every
/// child settlement wakes its parent, so a suite that scripts only child
/// responses would otherwise spend them on the parent's own turns.
pub(crate) fn park_parent(context: &Context, parent: &Arc<Agent>) {
    let parent = Arc::clone(parent);
    context
        .events()
        .on_waterfall(
            context,
            "agent/pre-step",
            move |_, args, next| {
                let parent = Arc::clone(&parent);
                Box::pin(async move {
                    let event = args
                        .get::<seekdeep_agent::AgentEvent<seekdeep_agent_loop::AgentPreStepEvent>>(
                            0,
                        )
                        .ok_or_else(|| anyhow::anyhow!("missing pre-step payload"))?;
                    if Arc::ptr_eq(&event.agent, &parent) {
                        Ok(seekdeep_cordis::EventReply::Value(Arc::new(
                            seekdeep_agent::PreStepDecision::Reject,
                        )))
                    } else {
                        next.run().await
                    }
                })
            },
            seekdeep_cordis::EventOptions::default(),
        )
        .unwrap();
}

/// The stable `SubagentError` code carried by an operation failure.
pub(crate) fn error_code(error: &anyhow::Error) -> Option<String> {
    error
        .downcast_ref::<seekdeep_subagent::SubagentError>()
        .map(|error| error.code.clone())
}

/// Register the source spec's `noop` tool.
pub(crate) fn register_noop_tool(stack: &Stack) {
    let noop = seekdeep_tools::define_content_tool_fixture(
        seekdeep_tools::ContentToolFixtureOptions::new(
            "noop",
            "does nothing",
            serde_json::json!({}),
            Arc::new(|_args: serde_json::Value, _run| {
                Box::pin(async {
                    Ok(vec![ContentBlock::Text {
                        text: "noop".into(),
                    }])
                })
            }),
        ),
    )
    .unwrap();
    stack
        .dependencies
        .tools
        .register(&stack.context, noop)
        .unwrap();
}

/// The booted continuable stack.
pub(crate) struct Stack {
    pub(crate) context: Context,
    pub(crate) dependencies: AgentLoopTestDependencies,
    pub(crate) subagents: Arc<SubagentRuntime>,
    pub(crate) adapter: Arc<MockAdapter>,
    pub(crate) parent: AgentHandle,
    pub(crate) root: Option<tempfile::TempDir>,
}

pub(crate) struct SetupOptions {
    pub(crate) persistence: bool,
    pub(crate) fork: bool,
}

impl Default for SetupOptions {
    fn default() -> Self {
        Self {
            persistence: true,
            fork: true,
        }
    }
}

/// Boot the full continuable stack: loop, persistence, providers, and subagents.
pub(crate) async fn setup(script: Vec<Entry>) -> Stack {
    setup_with(script, SetupOptions::default()).await
}

pub(crate) async fn setup_with(script: Vec<Entry>, options: SetupOptions) -> Stack {
    let root = options.persistence.then(|| tempfile::tempdir().unwrap());
    let adapter = MockAdapter::new(script);
    let (context, dependencies, subagents) = boot(
        root.as_ref().map(tempfile::TempDir::path),
        &adapter,
        options.fork,
    )
    .await;
    let parent = create_agent(&dependencies.agents, "parent", true).await;
    Stack {
        context,
        dependencies,
        subagents,
        adapter,
        parent,
        root,
    }
}

/// Boot a stack over an existing persistence root, as a fresh process would.
pub(crate) async fn boot(
    root: Option<&std::path::Path>,
    adapter: &Arc<MockAdapter>,
    fork: bool,
) -> (Context, AgentLoopTestDependencies, Arc<SubagentRuntime>) {
    boot_with(root, adapter, fork, false).await
}

/// Boot the stack, optionally mounting the session projection registry the
/// listing surface requires (the continuation specs leave it unmounted).
pub(crate) async fn boot_with(
    root: Option<&std::path::Path>,
    adapter: &Arc<MockAdapter>,
    fork: bool,
    projections: bool,
) -> (Context, AgentLoopTestDependencies, Arc<SubagentRuntime>) {
    let context = Context::new();
    let dependencies =
        mount_agent_loop_test_dependencies(&context, AgentLoopTestDependenciesOptions::default())
            .unwrap();
    if let Some(root) = root {
        let fiber =
            seekdeep_session_persistence_jsonl::install(&context, JsonlConfig::new(root)).unwrap();
        fiber.await_settled().await.unwrap();
    }
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
    if let Some(persistence) = context.get(SESSION_PERSISTENCE) {
        factory.set_persistence(persistence.persistence()).unwrap();
    }
    dependencies.agents.set_factory(Arc::new(factory)).unwrap();
    if projections {
        seekdeep_session_projection::SessionProjectionRegistry::install(&context).unwrap();
    }
    let subagents = SubagentRuntime::install(&context).unwrap();
    seekdeep_subagent_spawn_in_process::apply(
        &context,
        seekdeep_subagent_spawn_in_process::Config {
            provider_name: "spawn".to_owned(),
        },
    )
    .unwrap();
    if fork {
        seekdeep_subagent_fork_in_process::apply(
            &context,
            seekdeep_subagent_fork_in_process::Config {
                provider_name: "fork".to_owned(),
            },
        )
        .unwrap();
    }
    context.registry().await_quiescent().await;
    (context, dependencies, subagents)
}

/// `ctx.agentLoop.create(SessionId(id), { provider: 'mock', model: 'mock' })`,
/// or the routeless form when `routed` is false.
pub(crate) async fn create_agent(
    agents: &Arc<AgentRegistry>,
    id: &str,
    routed: bool,
) -> AgentHandle {
    let mut options = CreateAgentOptions::new(SessionId::new(id));
    if routed {
        options.agent_options = AgentOptions {
            provider: Some(ProviderId::new("mock")),
            model: Some(ModelId::new("mock")),
            max_tokens: None,
            subagent_depth: None,
        };
    }
    agents.create(options).await.unwrap()
}

pub(crate) fn message(text: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text { text: text.into() }]
}

pub(crate) fn start_spec(
    parent: &Arc<Agent>,
    provider: &str,
    signal: AbortSignal,
) -> ContinuableStartSpec {
    ContinuableStartSpec {
        provider: provider.to_owned(),
        label: "child task".to_owned(),
        request: ContinuableStartRequest {
            prompt: message("child task"),
            parent: Arc::clone(parent),
            agent_options: None,
            max_depth: None,
            tool_filter: None,
            persona: None,
        },
        signal,
    }
}

pub(crate) fn spawn_spec(parent: &Arc<Agent>) -> ContinuableStartSpec {
    start_spec(parent, "spawn", AbortSignal::default())
}

pub(crate) async fn followup(
    stack: &Stack,
    parent: &Arc<Agent>,
    child: &SessionId,
    content: Vec<ContentBlock>,
) -> anyhow::Result<MessageId> {
    followup_with(
        &stack.subagents,
        parent,
        child,
        content,
        AbortSignal::default(),
    )
    .await
}

pub(crate) async fn followup_with(
    subagents: &SubagentRuntime,
    parent: &Arc<Agent>,
    child: &SessionId,
    content: Vec<ContentBlock>,
    signal: AbortSignal,
) -> anyhow::Result<MessageId> {
    subagents
        .followup(
            parent,
            child,
            content,
            SubagentFollowupOptions {
                source: MessageSource::user(),
                signal,
            },
        )
        .await
}

/// Poll until `probe` succeeds or the timeout elapses (`vi.waitFor`).
pub(crate) async fn wait_for<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition did not hold within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait until a child's Activation is gone, i.e. its handle finished disposal.
pub(crate) async fn wait_no_activation(agents: &AgentRegistry, child: &SessionId) {
    wait_for(Duration::from_secs(5), || {
        agents.get(child).is_none().then_some(())
    })
    .await;
}

/// Wait until the child's Activation is live.
pub(crate) async fn wait_activation(agents: &AgentRegistry, child: &SessionId) -> Arc<Agent> {
    wait_for(Duration::from_secs(5), || agents.get(child)).await
}

pub(crate) fn agent_ids(agents: &AgentRegistry) -> Vec<String> {
    agents
        .list()
        .iter()
        .map(|agent| agent.id().to_string())
        .collect()
}

pub(crate) async fn load(context: &Context, id: &SessionId) -> SessionInspection {
    context
        .get(SESSION_PERSISTENCE)
        .expect("persistence service")
        .persistence()
        .load(id)
        .await
        .unwrap()
}

pub(crate) fn has_user_text(events: &[SessionEvent], text: &str) -> bool {
    events.iter().any(|event| {
        event.event_type == "user/message"
            && event.data["content"].as_array().is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block["type"] == "text" && block["text"] == text)
            })
    })
}

/// Caller-supplied user message texts in log order (runtime-context snapshots excluded).
pub(crate) fn user_texts(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|event| {
            event.event_type == "user/message" && event.data["source"]["kind"] != "plugin"
        })
        .flat_map(|event| {
            event.data["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|block| block["type"] == "text")
                .map(|block| block["text"].as_str().unwrap_or_default().to_owned())
        })
        .collect()
}

pub(crate) fn events_of(events: &[SessionEvent], event_type: &str) -> Vec<SessionEvent> {
    events
        .iter()
        .filter(|event| event.event_type == event_type)
        .cloned()
        .collect()
}

/// Exercise manager-wide teardown through the package-private owner.
pub(crate) fn drain_manager(
    subagents: &SubagentRuntime,
) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
    subagents
        .continuations()
        .expect("expected a bound continuation manager")
        .drain()
}

pub(crate) fn start_result(started: &ContinuableStart) -> (SessionId, MessageId) {
    (started.child_id.clone(), started.message_id.clone())
}

pub(crate) type Shared<T> = Arc<StdMutex<T>>;

/// A caller-released gate (`Promise.withResolvers` used as a latch).
#[derive(Default)]
pub(crate) struct Gate {
    open: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl Gate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn open(&self) {
        self.open.store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub(crate) async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.open.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

/// One settlement notice a parent received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) sender: String,
    pub(crate) summary: String,
    pub(crate) text: String,
}

/// Every settlement notice this agent received, in order, as flat text:
/// logged user messages first, then the inbox's step and turn queues.
pub(crate) fn settlement_notices(agent: &Agent) -> Vec<Notice> {
    let mut notices = Vec::new();
    for event in agent.session().events() {
        if event.event_type != "user/message" || event.data["source"]["kind"] != "subagent-settled"
        {
            continue;
        }
        notices.push(Notice {
            sender: event.data["source"]["senderSessionId"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            summary: event.data["source"]["summary"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            text: event.data["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter(|block| block["type"] == "text")
                .map(|block| block["text"].as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }
    let inbox = agent.inbox();
    for message in inbox.next_step().into_iter().chain(inbox.next_turn()) {
        let source = message.source();
        if source.kind != "subagent-settled" {
            continue;
        }
        notices.push(Notice {
            sender: source.fields["senderSessionId"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            summary: source.fields["summary"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            text: message
                .content()
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(
                        text.clone()
                            .try_into_string()
                            .expect("fixture uses scalar text"),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }
    notices
}

/// Parent-scope pre-step interception: `handler` decides for child agents
/// (those with a parent session) and the parent runs untouched.
pub(crate) fn child_pre_step<F, Fut>(context: &Context, handler: F)
where
    F: Fn(Arc<Agent>, u64) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<Option<seekdeep_agent::PreStepDecision>>>
        + Send
        + 'static,
{
    let handler = Arc::new(handler);
    context
        .events()
        .on_waterfall(
            context,
            "agent/pre-step",
            move |_, args, next| {
                let handler = Arc::clone(&handler);
                Box::pin(async move {
                    let event = args
                        .get::<seekdeep_agent::AgentEvent<seekdeep_agent_loop::AgentPreStepEvent>>(
                            0,
                        )
                        .ok_or_else(|| anyhow::anyhow!("missing pre-step payload"))?;
                    if event.agent.session().header().parent_session.is_none() {
                        return next.run().await;
                    }
                    match handler(Arc::clone(&event.agent), event.payload.turn).await? {
                        Some(decision) => {
                            Ok(seekdeep_cordis::EventReply::Value(Arc::new(decision)))
                        }
                        None => next.run().await,
                    }
                })
            },
            seekdeep_cordis::EventOptions::default(),
        )
        .unwrap();
}

pub(crate) fn turn_starts(agent: &Agent) -> Vec<u64> {
    events_of(&agent.session().events(), "turn/start")
        .iter()
        .filter_map(|event| event.data["turn"].as_u64())
        .collect()
}
