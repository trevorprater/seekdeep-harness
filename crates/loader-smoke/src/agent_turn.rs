//! Direct-agent turn driver shared by assembled Loader fixtures.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_agent::AGENTS;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent, SessionId},
    session_store::SESSIONS,
};
use seekdeep_llm::{ContentBlock, MessageSource, TokenUsage, UserMessage};
use serde::{Deserialize, Serialize};

/// Canonical-event observer for one fixture-owned Session interval.
pub type FixtureEventObserver = Arc<dyn Fn(&SessionId, &SessionEvent) + Send + Sync>;

/// Options for one fixture turn against exactly one configured root Agent.
#[derive(Clone, Default)]
pub struct FixtureTurnOptions {
    /// User task delivered through the durable inbox.
    pub task: String,
    /// Optional observer activated after the task's inbox receipt.
    pub on_event: Option<FixtureEventObserver>,
}

impl std::fmt::Debug for FixtureTurnOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FixtureTurnOptions")
            .field("task", &self.task)
            .field("on_event", &self.on_event.as_ref().map(|_| "<observer>"))
            .finish()
    }
}

/// Result envelope consumed only by snapshot and composition tests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureTurnResult {
    /// Exact result discriminant.
    #[serde(rename = "type")]
    pub kind: FixtureTurnResultKind,
    /// Configured root Session identity.
    pub session_id: SessionId,
    /// Final assistant text observed in the owned interval.
    pub output: String,
    /// Deduplicated usage accumulated by turn and step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

/// Closed fixture result discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixtureTurnResultKind {
    /// Completed fixture turn.
    Result,
}

#[derive(Default)]
struct TurnObservation {
    received: bool,
    output: String,
    usage_by_step: Vec<((u64, u64), TokenUsage)>,
}

impl TurnObservation {
    fn observe(
        &mut self,
        session_id: &SessionId,
        event: &SessionEvent,
        message_id: &str,
        callback: Option<&FixtureEventObserver>,
    ) {
        if !self.received {
            if event.event_type != "agent/inbox/spliced"
                || !event
                    .data
                    .get("inserted")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|inserted| {
                        inserted.iter().any(|message| {
                            message.get("id").and_then(serde_json::Value::as_str)
                                == Some(message_id)
                        })
                    })
            {
                return;
            }
            self.received = true;
        }

        if let Some(callback) = callback {
            callback(session_id, event);
        }
        let turn = event.data.get("turn").and_then(serde_json::Value::as_u64);
        let step = event.data.get("step").and_then(serde_json::Value::as_u64);
        if event.event_type == "assistant/chunk"
            && event
                .data
                .pointer("/chunk/type")
                .and_then(serde_json::Value::as_str)
                == Some("usage")
            && let (Some(turn), Some(step), Some(usage)) = (
                turn,
                step,
                event
                    .data
                    .get("chunk")
                    .and_then(|chunk| chunk.get("usage"))
                    .and_then(|usage| serde_json::from_value(usage.clone()).ok()),
            )
        {
            self.set_usage((turn, step), usage);
        }
        if event.event_type == "assistant/message" {
            if let Some(output) = assistant_text(event) {
                self.output = output;
            }
            if let (Some(turn), Some(step), Some(usage)) = (
                turn,
                step,
                event
                    .data
                    .get("usage")
                    .and_then(|usage| serde_json::from_value(usage.clone()).ok()),
            ) {
                self.set_usage((turn, step), usage);
            }
        }
    }

    fn set_usage(&mut self, key: (u64, u64), usage: TokenUsage) {
        if let Some((_, current)) = self
            .usage_by_step
            .iter_mut()
            .find(|(candidate, _)| *candidate == key)
        {
            *current = usage;
        } else {
            self.usage_by_step.push((key, usage));
        }
    }

    fn usage(&self) -> Option<TokenUsage> {
        let mut total = None;
        for (_, usage) in &self.usage_by_step {
            total = Some(match total {
                Some(current) => add_usage(&current, usage),
                None => usage.clone(),
            });
        }
        total
    }
}

fn assistant_text(event: &SessionEvent) -> Option<String> {
    let blocks = event
        .data
        .pointer("/message/content")
        .and_then(serde_json::Value::as_array)?;
    let text = blocks
        .iter()
        .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    (!text.is_empty()).then(|| text.concat())
}

fn add_usage(total: &TokenUsage, step: &TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: total.input_tokens + step.input_tokens,
        output_tokens: total.output_tokens + step.output_tokens,
        cache_read_tokens: add_optional(total.cache_read_tokens, step.cache_read_tokens),
        cache_write_tokens: add_optional(total.cache_write_tokens, step.cache_write_tokens),
        reasoning_tokens: add_optional(total.reasoning_tokens, step.reasoning_tokens),
    }
}

fn add_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    (left.is_some() || right.is_some()).then(|| left.unwrap_or(0) + right.unwrap_or(0))
}

/// Drives one task from durable inbox receipt through whole-Agent idle.
///
/// # Errors
///
/// Rejects missing or ambiguous root Agents, controller failures, listener
/// ownership failures, or a failed Session durability checkpoint.
pub async fn run_fixture_turn(
    context: &Context,
    options: FixtureTurnOptions,
) -> anyhow::Result<FixtureTurnResult> {
    let roots = context
        .get(AGENTS)
        .map_or_else(Vec::new, |agents| agents.roots());
    let [agent] = roots.as_slice() else {
        anyhow::bail!(
            "fixture turn requires exactly one top-level agent, found {}",
            roots.len()
        );
    };
    agent.when_idle()?.await?;

    let message = UserMessage::new(
        vec![ContentBlock::Text { text: options.task }],
        MessageSource::user(),
    );
    let message_id = message.id().as_str().to_owned();
    let session = agent.session().clone();
    let session_id = session.id().clone();
    let observation = Arc::new(Mutex::new(TurnObservation::default()));
    let observed = Arc::clone(&observation);
    let owned_session = session.clone();
    let observed_id = session_id.clone();
    let callback = options.on_event.clone();
    let listener = context.events().on_sync(
        context,
        "session/event",
        move |_, args| {
            let published = args
                .get::<Session>(0)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its Session"))?;
            if !Arc::ptr_eq(&published, &owned_session) {
                return Ok(EventReply::Undefined);
            }
            let event = args
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
            observed
                .lock()
                .observe(&observed_id, event.as_ref(), &message_id, callback.as_ref());
            Ok(EventReply::Undefined)
        },
        EventOptions::default(),
    )?;

    let turn = async {
        agent.followup(message)?;
        agent.when_idle()?.await
    }
    .await;
    if let Err(error) = turn {
        let _ = listener.dispose().await;
        return Err(error);
    }
    listener.dispose().await?;
    context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("fixture turn requires sessions"))?
        .flush(&session)
        .await?;
    let observation = observation.lock();
    Ok(FixtureTurnResult {
        kind: FixtureTurnResultKind::Result,
        session_id,
        output: observation.output.clone(),
        usage: observation.usage(),
    })
}
