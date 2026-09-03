//! Direct Agent turn interval and usage aggregation parity.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentControlError, AgentController, AgentOptions, CancelOptions, Inbox, InboxTarget,
    MaintenanceReservation, NoopInboxNotifications,
};
use seekdeep_core::{
    session::{AgentCancelCause, AppendOptions, Session, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{
    AbortSignal, CallId, ContentBlock, Message, MessageRole, MessageSource, TokenUsage, UserMessage,
};
use seekdeep_loader_smoke::{FixtureTurnOptions, FixtureTurnResultKind, run_fixture_turn};
use seekdeep_scope::ScopeKey;
use serde_json::json;

#[derive(Clone, Copy)]
enum Script {
    Full,
    InboxOnly,
    FailSecondIdle,
}

struct FixtureController {
    session: Arc<Session>,
    foreign: Arc<Session>,
    idle_calls: Arc<AtomicUsize>,
    script: Script,
}

impl FixtureController {
    fn assistant(
        &self,
        turn: u64,
        step: u64,
        content: Vec<ContentBlock>,
        usage: Option<TokenUsage>,
        sources: Vec<u64>,
    ) {
        let message = Message::new(
            MessageRole::Assistant,
            content,
            MessageSource::model("mock", "mock"),
        );
        let mut data = json!({"turn":turn,"step":step,"message":message});
        if let Some(usage) = usage {
            data["usage"] = serde_json::to_value(usage).unwrap();
        }
        self.session
            .append(
                "assistant/message",
                data,
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    source_event_seqs: Some(sources),
                    ..AppendOptions::default()
                },
            )
            .unwrap();
    }
}

impl AgentController for FixtureController {
    fn send(
        &self,
        message: UserMessage,
        _target: InboxTarget,
        _wakeup: bool,
    ) -> Result<(), AgentControlError> {
        if matches!(self.script, Script::FailSecondIdle) {
            return Ok(());
        }
        self.foreign
            .append("turn/start", json!({"turn":1}), AppendOptions::default())
            .unwrap();
        self.assistant(1, 0, Vec::new(), None, Vec::new());
        self.session
            .append(
                "agent/inbox/spliced",
                json!({"target":"next-turn","start":0,"inserted":[message]}),
                AppendOptions::default(),
            )
            .unwrap();
        if matches!(self.script, Script::InboxOnly) {
            return Ok(());
        }
        let step_one = self
            .session
            .append(
                "assistant/chunk",
                json!({
                    "turn":1,"step":1,
                    "chunk":{"type":"usage","usage":{
                        "inputTokens":2,"outputTokens":3,"reasoningTokens":1
                    }}
                }),
                AppendOptions::default(),
            )
            .unwrap();
        self.assistant(
            1,
            1,
            vec![ContentBlock::Text {
                text: "final answer".to_owned(),
            }],
            Some(TokenUsage {
                input_tokens: 4,
                output_tokens: 5,
                cache_read_tokens: Some(6),
                cache_write_tokens: None,
                reasoning_tokens: None,
            }),
            vec![step_one.seq],
        );
        let step_two = self
            .session
            .append(
                "assistant/chunk",
                json!({
                    "turn":1,"step":2,
                    "chunk":{"type":"usage","usage":{
                        "inputTokens":1,"outputTokens":2,"cacheWriteTokens":7,"reasoningTokens":2
                    }}
                }),
                AppendOptions::default(),
            )
            .unwrap();
        self.assistant(
            1,
            2,
            vec![ContentBlock::ToolCall {
                id: CallId::new("fixture-call"),
                name: "fixture".to_owned(),
                arguments: "{}".to_owned(),
            }],
            None,
            vec![step_two.seq],
        );
        Ok(())
    }

    fn cancel(
        &self,
        _cause: AgentCancelCause,
        _options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        Ok(())
    }

    fn when_idle(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        let call = self.idle_calls.fetch_add(1, Ordering::AcqRel) + 1;
        let fail = matches!(self.script, Script::FailSecondIdle) && call == 2;
        Box::pin(async move {
            if fail {
                anyhow::bail!("turn failed")
            }
            Ok(())
        })
    }

    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError> {
        Ok(MaintenanceReservation::new(
            AbortSignal::default(),
            Arc::new(|| {}),
        ))
    }
}

struct Harness {
    context: seekdeep_cordis::Context,
    flushes: Arc<AtomicUsize>,
    idle_calls: Arc<AtomicUsize>,
}

impl Harness {
    fn new(agent_count: usize, script: Script) -> Self {
        let context = seekdeep_cordis::Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(seekdeep_agent::AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let foreign = sessions
            .create(
                &context,
                Some(SessionId::new("foreign")),
                CreateSessionOptions::default(),
            )
            .unwrap();
        let idle_calls = Arc::new(AtomicUsize::new(0));
        for index in 0..agent_count {
            let id = SessionId::new(format!("fixture-{index}"));
            let session = sessions
                .create(&context, Some(id.clone()), CreateSessionOptions::default())
                .unwrap();
            let inbox =
                Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
            let agent = Arc::new(Agent::new(
                id,
                AgentOptions::default(),
                session.clone(),
                inbox,
                context.clone(),
                ScopeKey::new(),
            ));
            agent
                .install_controller(Arc::new(FixtureController {
                    session,
                    foreign: foreign.clone(),
                    idle_calls: Arc::clone(&idle_calls),
                    script,
                }))
                .unwrap();
            agents.register(&context, &agent, None).unwrap();
        }
        let flushes = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&flushes);
        context
            .events()
            .on_sync(
                &context,
                "session/flush",
                move |_, _| {
                    observed.fetch_add(1, Ordering::AcqRel);
                    Ok(seekdeep_cordis::EventReply::Undefined)
                },
                seekdeep_cordis::EventOptions::default(),
            )
            .unwrap();
        Self {
            context,
            flushes,
            idle_calls,
        }
    }
}

#[tokio::test]
async fn rejects_absent_and_ambiguous_root_agents() {
    for (count, expected) in [(0, 0), (2, 2)] {
        let harness = Harness::new(count, Script::InboxOnly);
        let error = run_fixture_turn(
            &harness.context,
            FixtureTurnOptions {
                task: "ignored".to_owned(),
                on_event: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("exactly one top-level agent, found {expected}"))
        );
        harness.context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn observes_owned_interval_final_text_and_deduplicated_usage() {
    let harness = Harness::new(1, Script::Full);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&observed);
    let result = run_fixture_turn(
        &harness.context,
        FixtureTurnOptions {
            task: "prove the fixture".to_owned(),
            on_event: Some(Arc::new(move |session_id, event| {
                sink.lock()
                    .push((session_id.as_str().to_owned(), event.clone()));
            })),
        },
    )
    .await
    .unwrap();
    assert_eq!(result.kind, FixtureTurnResultKind::Result);
    assert_eq!(result.session_id.as_str(), "fixture-0");
    assert_eq!(result.output, "final answer");
    assert_eq!(
        result.usage,
        Some(TokenUsage {
            input_tokens: 5,
            output_tokens: 7,
            cache_read_tokens: Some(6),
            cache_write_tokens: Some(7),
            reasoning_tokens: Some(2),
        })
    );
    assert_eq!(
        observed
            .lock()
            .iter()
            .map(|(session, _)| session.as_str())
            .collect::<Vec<_>>(),
        ["fixture-0"; 5]
    );
    let mut stream = observed
        .lock()
        .iter()
        .map(|(session_id, event)| {
            json!({"type":"session_event","sessionId":session_id,"event":event})
        })
        .collect::<Vec<_>>();
    stream.push(serde_json::to_value(&result).unwrap());
    assert!(stream[..stream.len() - 1].iter().all(|record| {
        record["type"] == "session_event"
            && record["sessionId"] == "fixture-0"
            && record["event"].is_object()
    }));
    assert_eq!(
        stream.last().unwrap(),
        &json!({
            "type":"result",
            "sessionId":"fixture-0",
            "output":"final answer",
            "usage":{
                "inputTokens":5,
                "outputTokens":7,
                "cacheReadTokens":6,
                "cacheWriteTokens":7,
                "reasoningTokens":2
            }
        })
    );
    let jsonl = stream
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(
        jsonl
            .lines()
            .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
    );
    assert_eq!(harness.idle_calls.load(Ordering::Acquire), 2);
    assert_eq!(harness.flushes.load(Ordering::Acquire), 1);
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn omits_usage_when_the_owned_interval_records_none() {
    let harness = Harness::new(1, Script::InboxOnly);
    let result = run_fixture_turn(
        &harness.context,
        FixtureTurnOptions {
            task: "no model step".to_owned(),
            on_event: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(result.output, "");
    assert_eq!(result.usage, None);
    assert_eq!(harness.flushes.load(Ordering::Acquire), 1);
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn always_removes_listener_when_the_turn_fails() {
    let harness = Harness::new(1, Script::FailSecondIdle);
    let before = harness
        .context
        .events()
        .listener_count(&harness.context, "session/event");
    let error = run_fixture_turn(
        &harness.context,
        FixtureTurnOptions {
            task: "fail".to_owned(),
            on_event: None,
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("turn failed"));
    assert_eq!(
        harness
            .context
            .events()
            .listener_count(&harness.context, "session/event"),
        before
    );
    assert_eq!(harness.flushes.load(Ordering::Acquire), 0);
    harness.context.fiber().dispose().await.unwrap();
}
