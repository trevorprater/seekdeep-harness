//! Real continuation-manager delivery through a headless parent tool turn.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt as _, stream};
use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, AgentEvent};
use seekdeep_agent_loop::{
    AgentInboxMessage, AgentPreStepEvent, Config as LoopConfig, PLUGIN_INJECT,
};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_core::session::{Session, SessionHeader};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LLM, LlmAdapter,
    StreamChunk, TokenUsage,
};
use seekdeep_loader::PluginCatalog;
use seekdeep_loader_smoke::{FixtureTurnOptions, run_fixture_turn};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use serde_json::{Value, json};
use tokio::sync::Notify;

const PARENT_ID: &str = "settlement-parent";
const CHILD_RESULT: &str = "CHILD_RESULT";
const PARENT_RESULT: &str = "PARENT_RECEIVED_CHILD_RESULT";

#[derive(Default)]
struct OrderingFence {
    child_release: Notify,
    notice: Notify,
    delivered: AtomicBool,
}

struct SettlementAdapter {
    fence: Arc<OrderingFence>,
    parent_requests: AtomicUsize,
    child_requests: AtomicUsize,
}

fn usage(input_tokens: u64, output_tokens: u64) -> TokenUsage {
    TokenUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    }
}

fn answer(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta {
            index: 0,
            text: text.to_owned(),
        },
        StreamChunk::Usage {
            usage: usage(10, 5),
        },
        StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        },
    ]
}

#[async_trait]
impl LlmAdapter for SettlementAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        if options
            .session_id
            .as_ref()
            .is_none_or(|id| id.as_str() != PARENT_ID)
        {
            self.child_requests.fetch_add(1, Ordering::AcqRel);
            let fence = self.fence.clone();
            return AdapterStream::new(
                stream::once(async move {
                    fence.child_release.notified().await;
                    Ok(StreamChunk::BlockStart {
                        index: 0,
                        block_type: "text".to_owned(),
                    })
                })
                .chain(stream::iter(answer(CHILD_RESULT).into_iter().map(Ok))),
            );
        }
        let call = self.parent_requests.fetch_add(1, Ordering::AcqRel);
        let chunks = match call {
            0 => vec![
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("start-child"),
                        name: "subagent".to_owned(),
                        arguments: json!({
                            "description": "Return child result",
                            "prompt": "Reply with exactly CHILD_RESULT and nothing else. Do not call report."
                        }).to_string(),
                    },
                },
                StreamChunk::Usage { usage: usage(10, 5) },
                StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None },
            ],
            1 => answer("STARTED"),
            2 => {
                let delivered = options.messages.iter().any(|message| {
                    message.source().kind == "subagent-settled"
                        && message.content().iter().any(|block| {
                            matches!(block, ContentBlock::Text { text } if text == CHILD_RESULT)
                        })
                });
                answer(if delivered { PARENT_RESULT } else { "MISSING_SETTLEMENT_NOTICE" })
            }
            _ => panic!("unexpected parent request after settlement response"),
        };
        AdapterStream::new(stream::iter(chunks.into_iter().map(Ok)))
    }
}

fn fixture_plugin(adapter: Arc<SettlementAdapter>) -> Plugin {
    Plugin::new("settlement-fixture", ["llm"], move |context, _| {
        let adapter = adapter.clone();
        Box::pin(async move {
            let registration = Arc::new(
                context
                    .get(LLM)
                    .unwrap()
                    .register_adapter(&["settlement-mock".to_owned()], adapter.clone())?,
            );
            context.own(EffectHandle::new("settlement mock adapter", move || {
                Box::pin(async move { registration.dispose().await })
            }))?;
            let inbox_fence = adapter.fence.clone();
            context.events().on_sync(
                &context,
                "agent/inbox/inserted",
                move |_, args| {
                    let event = args.get::<AgentEvent<AgentInboxMessage>>(0).unwrap();
                    if event.agent.id().as_str() == PARENT_ID
                        && event.payload.message.source().kind == "subagent-settled"
                    {
                        inbox_fence.delivered.store(true, Ordering::Release);
                        inbox_fence.notice.notify_waiters();
                    }
                    Ok(EventReply::Undefined)
                },
                EventOptions {
                    global: true,
                    prepend: false,
                },
            )?;
            let fence = adapter.fence.clone();
            context.events().on_waterfall(
                &context,
                "agent/pre-step",
                move |_, args, next| {
                    let fence = fence.clone();
                    Box::pin(async move {
                        let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                        if event.agent.id().as_str() == PARENT_ID
                            && event.payload.turn == 1
                            && event.payload.step == 2
                        {
                            // Fix the source snapshot's interleave after the parent's second claim.
                            fence.child_release.notify_one();
                            loop {
                                let notice = fence.notice.notified();
                                if fence.delivered.load(Ordering::Acquire) {
                                    break;
                                }
                                notice.await;
                            }
                        }
                        next.run().await
                    })
                },
                EventOptions {
                    global: true,
                    prepend: false,
                },
            )?;
            Ok(())
        })
    })
}

fn catalog(adapter: Arc<SettlementAdapter>) -> anyhow::Result<PluginCatalog> {
    let catalog = PluginCatalog::new();
    for (name, plugin) in [
        ("sessions", seekdeep_core::session_store::plugin()),
        ("llm", seekdeep_llm::plugin()),
        ("agents", seekdeep_agent::plugin()),
        ("prompt", seekdeep_system_prompt::plugin()),
        ("tools", seekdeep_tools::plugin()),
        ("persistence", seekdeep_session_persistence_jsonl::plugin()),
        ("subagents", seekdeep_subagent::plugin()),
        ("spawn", seekdeep_subagent_spawn_in_process::plugin()),
        ("tool-subagent", seekdeep_tool_subagent::plugin()),
        ("fixture", fixture_plugin(adapter)),
    ] {
        catalog.register_named(name, plugin)?;
    }
    catalog.register_named(
        "loop",
        Plugin::new(
            "loop",
            PLUGIN_INJECT.iter().copied().chain(["sessionPersistence"]),
            |context, config| {
                Box::pin(async move {
                    seekdeep_agent_loop::apply(
                        &context,
                        serde_json::from_value::<LoopConfig>(config)?,
                    )
                    .await?;
                    Ok(())
                })
            },
        ),
    )?;
    Ok(catalog)
}

fn composition_source(
    root: &std::path::Path,
    workspace: &std::path::Path,
) -> anyhow::Result<String> {
    Ok(format!(
        concat!(
            "- {{ id: sessions, name: sessions }}\n- {{ id: llm, name: llm }}\n",
            "- {{ id: agents, name: agents }}\n- {{ id: prompt, name: prompt, config: {{}} }}\n",
            "- {{ id: tools, name: tools, config: {{ mode: native }} }}\n",
            "- {{ id: persistence, name: persistence, config: {{ root: {root}, compression: none }} }}\n",
            "- {{ id: fixture, name: fixture }}\n- {{ id: subagents, name: subagents }}\n",
            "- {{ id: spawn, name: spawn, config: {{ providerName: spawn }} }}\n",
            "- {{ id: delegation, name: tool-subagent, config: {{ provider: spawn, backgroundMode: continuable }} }}\n",
            "- id: loop\n  name: loop\n  config:\n    agents:\n",
            "      - {{ id: main, sessionId: settlement-parent, provider: settlement-mock, model: mock, cwd: {cwd} }}\n"
        ),
        root = serde_json::to_string(&root.join("sessions"))?,
        cwd = serde_json::to_string(workspace)?
    ))
}

#[tokio::test]
async fn manager_delivers_one_durable_child_notice_without_parent_polling() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let context = Context::new();
    let headers = Arc::new(Mutex::new(Vec::<SessionHeader>::new()));
    let observed_headers = headers.clone();
    context.events().on_sync(
        &context,
        "session/created",
        move |_, args| {
            observed_headers
                .lock()
                .push(args.get::<Session>(0).unwrap().header().clone());
            Ok(EventReply::Undefined)
        },
        EventOptions {
            global: true,
            prepend: false,
        },
    )?;
    let adapter = Arc::new(SettlementAdapter {
        fence: Arc::new(OrderingFence::default()),
        parent_requests: AtomicUsize::new(0),
        child_requests: AtomicUsize::new(0),
    });
    let source = composition_source(temporary.path(), &workspace)?;
    let composition = catalog(adapter.clone())?
        .load_yaml(&context, &source)
        .await?;
    let result = tokio::time::timeout(Duration::from_secs(10), run_fixture_turn(&context, FixtureTurnOptions {
        task: "Start one continuable background subagent and answer from its completion notice. Do not call list_agents, send_message, job_output, or job_list.".to_owned(),
        on_event: None,
    })).await??;
    let parent = context
        .get(AGENTS)
        .unwrap()
        .get(&result.session_id)
        .unwrap();
    let events = parent.session().events();
    let child_header = headers
        .lock()
        .iter()
        .find(|header| header.parent_session.is_some())
        .cloned()
        .unwrap();
    let persistence = context.get(SESSION_PERSISTENCE).unwrap().persistence();
    let child = persistence.inspect(&child_header.id, None).await?;
    composition.dispose().await?;
    context.root_fiber().dispose().await?;

    assert_eq!(result.output, PARENT_RESULT);
    assert_eq!(result.usage, Some(usage(30, 15)));
    assert_eq!(adapter.parent_requests.load(Ordering::Acquire), 3);
    assert_eq!(adapter.child_requests.load(Ordering::Acquire), 1);
    assert_eq!(child.meta.parent_session.as_ref(), Some(&result.session_id));
    let calls = events
        .iter()
        .filter(|event| event.event_type == "tool/call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].data["name"], "subagent");
    let arguments: Value = serde_json::from_str(calls[0].data["arguments"].as_str().unwrap())?;
    assert!(arguments.get("run_in_background").is_none());
    let notices = events
        .iter()
        .filter(|event| event.event_type == "agent/inbox/spliced")
        .flat_map(|event| event.data["inserted"].as_array().into_iter().flatten())
        .filter(|message| message["source"]["kind"] == "subagent-settled")
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].to_string().contains(CHILD_RESULT));
    assert_eq!(
        notices[0]["source"]["senderSessionId"],
        child.meta.id.as_str()
    );
    assert!(
        child
            .events
            .iter()
            .any(|event| event.event_type == "assistant/message"
                && event.data.to_string().contains(CHILD_RESULT))
    );
    assert!(
        !child
            .events
            .iter()
            .any(|event| event.event_type == "tool/call")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "turn/start")
            .count(),
        1
    );
    assert_eq!(events.last().unwrap().data["reason"]["kind"], "completed");
    Ok(())
}
