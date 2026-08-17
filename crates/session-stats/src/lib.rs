//! Whole-log session turn, step, model, tool, and token timing statistics.

use std::sync::Arc;

use indexmap::IndexMap;
use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_core::session::SessionEvent;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SessionProjectionRegistry,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Projection registry key owned by this package.
pub const SESSION_STATS_KEY: &str = "sessionStats";

/// Whole-log conversation figures.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionStats {
    /// Distinct turns carrying at least one closed step.
    pub turns: u64,
    /// Closed steps, including cancelled and failed steps.
    pub steps: u64,
    /// Summed model wall time in milliseconds.
    pub llm_ms: u64,
    /// Summed matched tool call-to-result wall time in milliseconds.
    pub tool_ms: u64,
    /// Summed first-token latency in milliseconds.
    pub ttft_ms: u64,
    /// Steps carrying a recorded first token.
    pub ttft_steps: u64,
    /// Summed first-token-to-message decode time in milliseconds.
    pub decode_ms: u64,
    /// Summed valid provider output tokens for decode-timed steps.
    pub decode_tokens: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenStep {
    turn: u64,
    step: u64,
    start_time: i64,
    first_token_time: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionStatsState {
    #[serde(flatten)]
    totals: SessionStats,
    last_turn: Option<u64>,
    open_step: Option<OpenStep>,
    pending_calls: IndexMap<String, i64>,
}

/// Builds the pure `sessionStats` projection definition.
#[must_use]
pub fn definition() -> ProjectionDefinition {
    ProjectionDefinition::new(
        SESSION_STATS_KEY,
        1,
        || Ok(serde_json::to_value(SessionStatsState::default())?),
        apply,
        |state| {
            let state: SessionStatsState = serde_json::from_value(state.clone())?;
            validate_totals(&state.totals)?;
            Ok(serde_json::to_value(state.totals)?)
        },
    )
}

/// Registers the statistics fold on a projection registry.
///
/// # Errors
///
/// Returns ordinary projection registration failures.
pub fn install(
    context: &Context,
    projections: &Arc<SessionProjectionRegistry>,
) -> anyhow::Result<EffectHandle> {
    projections.register(context, definition())
}

fn apply(state: &Value, event: &SessionEvent) -> anyhow::Result<ProjectionTransition> {
    let mut state: SessionStatsState = serde_json::from_value(state.clone())?;
    match event.event_type.as_str() {
        "step/start" => {
            let (turn, step) = coordinates(event)?;
            state.open_step = Some(OpenStep {
                turn,
                step,
                start_time: event.time,
                first_token_time: None,
            });
        }
        "assistant/chunk" => {
            let (turn, step) = coordinates(event)?;
            let Some(open) = &mut state.open_step else {
                return Ok(ProjectionTransition::Unchanged);
            };
            if open.turn != turn
                || open.step != step
                || open.first_token_time.is_some()
                || !is_token_delta(event.data.get("chunk"))
            {
                return Ok(ProjectionTransition::Unchanged);
            }
            open.first_token_time = Some(event.time);
        }
        "assistant/message" => {
            let (turn, step) = coordinates(event)?;
            let Some(open) = state.open_step.as_ref() else {
                return Ok(ProjectionTransition::Unchanged);
            };
            if open.turn != turn || open.step != step {
                return Ok(ProjectionTransition::Unchanged);
            }
            let Some(open) = state.open_step.take() else {
                return Ok(ProjectionTransition::Unchanged);
            };
            state.totals.llm_ms = state
                .totals
                .llm_ms
                .saturating_add(nonnegative_elapsed(event.time, open.start_time));
            if let Some(first_token_time) = open.first_token_time {
                state.totals.ttft_ms = state
                    .totals
                    .ttft_ms
                    .saturating_add(nonnegative_elapsed(first_token_time, open.start_time));
                state.totals.ttft_steps = state.totals.ttft_steps.saturating_add(1);
                if let Some(output_tokens) = usage_output_tokens(event.data.get("usage")) {
                    state.totals.decode_ms = state
                        .totals
                        .decode_ms
                        .saturating_add(nonnegative_elapsed(event.time, first_token_time));
                    state.totals.decode_tokens += output_tokens;
                }
            }
        }
        "tool/call" => {
            let call_id = string_field(&event.data, "callId")?;
            state.pending_calls.insert(call_id.to_owned(), event.time);
        }
        "tool/result" => {
            let Some(call_id) = event
                .data
                .pointer("/message/source/callId")
                .and_then(Value::as_str)
            else {
                anyhow::bail!("tool/result lacks message.source.callId");
            };
            let Some(dispatched) = state.pending_calls.shift_remove(call_id) else {
                return Ok(ProjectionTransition::Unchanged);
            };
            state.totals.tool_ms = state
                .totals
                .tool_ms
                .saturating_add(nonnegative_elapsed(event.time, dispatched));
        }
        "step/end" => {
            let turn = integer_field(&event.data, "turn")?;
            if state.last_turn != Some(turn) {
                state.totals.turns = state.totals.turns.saturating_add(1);
            }
            state.totals.steps = state.totals.steps.saturating_add(1);
            state.last_turn = Some(turn);
            state.open_step = None;
        }
        "turn/end" => {
            if state.pending_calls.is_empty() {
                return Ok(ProjectionTransition::Unchanged);
            }
            state.pending_calls.clear();
        }
        _ => return Ok(ProjectionTransition::Unchanged),
    }
    ProjectionTransition::changed(state)
}

fn coordinates(event: &SessionEvent) -> anyhow::Result<(u64, u64)> {
    Ok((
        integer_field(&event.data, "turn")?,
        integer_field(&event.data, "step")?,
    ))
}

fn integer_field(value: &Value, name: &str) -> anyhow::Result<u64> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("{value} lacks non-negative integer {name}"))
}

fn string_field<'a>(value: &'a Value, name: &str) -> anyhow::Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{value} lacks string {name}"))
}

fn is_token_delta(chunk: Option<&Value>) -> bool {
    let Some(chunk) = chunk else {
        return false;
    };
    match chunk.get("type").and_then(Value::as_str) {
        Some("text-delta" | "reasoning-delta") => chunk
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        Some("tool-call-delta") => {
            chunk.get("name").is_some()
                || chunk
                    .get("argumentsDelta")
                    .and_then(Value::as_str)
                    .is_some_and(|delta| !delta.is_empty())
        }
        _ => false,
    }
}

fn usage_output_tokens(usage: Option<&Value>) -> Option<f64> {
    usage?
        .get("outputTokens")?
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn nonnegative_elapsed(end: i64, start: i64) -> u64 {
    u64::try_from(i128::from(end) - i128::from(start)).unwrap_or(0)
}

fn validate_totals(totals: &SessionStats) -> anyhow::Result<()> {
    anyhow::ensure!(
        totals.decode_tokens.is_finite() && totals.decode_tokens >= 0.0,
        "sessionStats decodeTokens must be a non-negative finite number"
    );
    Ok(())
}

/// Registers the statistics package's explained empty invariant companion.
///
/// The fold's wire value is validated by the projection registry, while
/// event lifecycle relations are owned by the session and agent-loop checks.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-session-stats", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use seekdeep_core::{
        session::{AppendOptions, Session, SessionId},
        session_store::{CreateSessionOptions, SessionStore},
    };
    use seekdeep_invariants::InvariantConfig;
    use seekdeep_session_projection::SessionProjectionRegistry;
    use serde_json::json;

    use super::*;

    fn at(seq: u64, time: i64, event_type: &str, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time,
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    fn fold(events: &[SessionEvent]) -> SessionStats {
        let definition = definition();
        let mut state = definition.initial_state().expect("initial state");
        for event in events {
            if let ProjectionTransition::Changed(next) =
                definition.apply_event(&state, event).expect("apply event")
            {
                state = next;
            }
        }
        serde_json::from_value(definition.project(&state).expect("project stats"))
            .expect("stats value")
    }

    fn totals(overrides: impl FnOnce(&mut SessionStats)) -> SessionStats {
        let mut totals = SessionStats::default();
        overrides(&mut totals);
        totals
    }

    fn setup() -> (Context, Arc<SessionProjectionRegistry>, Arc<Session>) {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let projections = SessionProjectionRegistry::install(&context).expect("projections");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("stats-test")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        (context, projections, session)
    }

    fn append(session: &Session, event_type: &str, data: Value) -> SessionEvent {
        session
            .append(event_type, data, AppendOptions::default())
            .expect("append")
    }

    #[test]
    fn counts_closed_steps_and_distinct_nonempty_turns() {
        let events = [
            at(0, 0, "turn/start", json!({ "turn": 1 })),
            at(1, 1, "turn/end", json!({ "turn": 1 })),
            at(2, 2, "step/start", json!({ "turn": 2, "step": 1 })),
            at(3, 3, "step/end", json!({ "turn": 2, "step": 1 })),
            at(4, 4, "step/start", json!({ "turn": 2, "step": 2 })),
            at(5, 5, "step/end", json!({ "turn": 2, "step": 2 })),
            at(6, 6, "step/start", json!({ "turn": 3, "step": 1 })),
            at(7, 7, "step/end", json!({ "turn": 3, "step": 1 })),
        ];
        assert_eq!(
            fold(&events),
            totals(|stats| {
                stats.turns = 2;
                stats.steps = 3;
            })
        );
    }

    #[test]
    fn accrues_model_first_token_and_decode_windows() {
        let events = [
            at(0, 1_000, "step/start", json!({ "turn": 1, "step": 1 })),
            at(
                1,
                1_800,
                "assistant/chunk",
                json!({ "turn": 1, "step": 1, "chunk": { "type": "text-delta", "index": 0, "text": "a" } }),
            ),
            at(
                2,
                4_800,
                "assistant/message",
                json!({ "turn": 1, "step": 1, "usage": { "inputTokens": 10, "outputTokens": 60 } }),
            ),
            at(3, 4_900, "step/end", json!({ "turn": 1, "step": 1 })),
        ];
        assert_eq!(
            fold(&events),
            totals(|stats| {
                stats.turns = 1;
                stats.steps = 1;
                stats.llm_ms = 3_800;
                stats.ttft_ms = 800;
                stats.ttft_steps = 1;
                stats.decode_ms = 3_000;
                stats.decode_tokens = 60.0;
            })
        );
    }

    #[test]
    fn retry_keeps_first_token_and_empty_or_foreign_chunks_do_not_count() {
        let events = [
            at(
                0,
                500,
                "assistant/chunk",
                json!({ "turn": 1, "step": 1, "chunk": { "type": "text-delta", "text": "stray" } }),
            ),
            at(1, 1_000, "step/start", json!({ "turn": 1, "step": 1 })),
            at(
                2,
                1_100,
                "assistant/chunk",
                json!({ "turn": 1, "step": 1, "chunk": { "type": "block-start" } }),
            ),
            at(
                3,
                1_200,
                "assistant/chunk",
                json!({ "turn": 1, "step": 1, "chunk": { "type": "text-delta", "text": "" } }),
            ),
            at(
                4,
                1_300,
                "assistant/chunk",
                json!({ "turn": 9, "step": 9, "chunk": { "type": "text-delta", "text": "other" } }),
            ),
            at(
                5,
                1_400,
                "assistant/chunk",
                json!({ "turn": 1, "step": 1, "chunk": { "type": "reasoning-delta", "text": "first" } }),
            ),
            at(6, 1_500, "llm/retry", json!({ "turn": 1, "step": 1 })),
            at(
                7,
                1_800,
                "assistant/chunk",
                json!({ "turn": 1, "step": 1, "chunk": { "type": "text-delta", "text": "later" } }),
            ),
            at(
                8,
                2_000,
                "assistant/message",
                json!({ "turn": 1, "step": 1 }),
            ),
            at(9, 2_100, "step/end", json!({ "turn": 1, "step": 1 })),
        ];
        assert_eq!(
            fold(&events),
            totals(|stats| {
                stats.turns = 1;
                stats.steps = 1;
                stats.llm_ms = 1_000;
                stats.ttft_ms = 400;
                stats.ttft_steps = 1;
            })
        );
    }

    #[test]
    fn cancelled_step_counts_but_partial_stream_time_does_not() {
        let events = [
            at(0, 1_000, "step/start", json!({ "turn": 1, "step": 1 })),
            at(
                1,
                1_500,
                "assistant/chunk",
                json!({ "turn": 1, "step": 1, "chunk": { "type": "tool-call-delta", "name": "read", "argumentsDelta": "" } }),
            ),
            at(2, 2_000, "step/end", json!({ "turn": 1, "step": 1 })),
        ];
        assert_eq!(
            fold(&events),
            totals(|stats| {
                stats.turns = 1;
                stats.steps = 1;
            })
        );
    }

    #[test]
    fn pairs_tools_by_own_call_id_and_prunes_unresolved_turn_calls() {
        let result = |call_id: &str| json!({ "turn": 1, "step": 1, "message": { "source": { "kind": "tool", "callId": call_id } } });
        let paired = [
            at(0, 1_000, "step/start", json!({ "turn": 1, "step": 1 })),
            at(1, 1_100, "tool/call", json!({ "callId": "a" })),
            at(2, 1_200, "tool/call", json!({ "callId": "b" })),
            at(3, 4_200, "tool/result", result("b")),
            at(4, 1_600, "tool/result", result("a")),
            at(5, 5_000, "tool/result", result("toString")),
            at(6, 5_100, "step/end", json!({ "turn": 1, "step": 1 })),
        ];
        assert_eq!(
            fold(&paired),
            totals(|stats| {
                stats.turns = 1;
                stats.steps = 1;
                stats.tool_ms = 3_500;
            })
        );

        let pruned = [
            at(0, 1_000, "step/start", json!({ "turn": 1, "step": 1 })),
            at(1, 1_100, "tool/call", json!({ "callId": "orphan" })),
            at(2, 2_000, "step/end", json!({ "turn": 1, "step": 1 })),
            at(3, 2_100, "turn/end", json!({ "turn": 1 })),
            at(4, 9_000, "tool/result", result("orphan")),
        ];
        assert_eq!(
            fold(&pruned),
            totals(|stats| {
                stats.turns = 1;
                stats.steps = 1;
            })
        );
    }

    #[test]
    fn invalid_usage_duplicate_message_and_clock_skew_are_defensive() {
        let events = [
            at(0, 2_000, "step/start", json!({ "turn": 1, "step": 1 })),
            at(
                1,
                1_400,
                "assistant/chunk",
                json!({ "turn": 1, "step": 1, "chunk": { "type": "text-delta", "text": "a" } }),
            ),
            at(
                2,
                1_000,
                "assistant/message",
                json!({ "turn": 1, "step": 1, "usage": { "outputTokens": -5 } }),
            ),
            at(
                3,
                3_000,
                "assistant/message",
                json!({ "turn": 1, "step": 1, "usage": { "outputTokens": 99 } }),
            ),
            at(4, 3_100, "step/end", json!({ "turn": 1, "step": 1 })),
        ];
        assert_eq!(
            fold(&events),
            totals(|stats| {
                stats.turns = 1;
                stats.steps = 1;
                stats.ttft_steps = 1;
            })
        );
    }

    #[tokio::test]
    async fn registry_integration_late_mount_change_feed_and_hmr_match_source() {
        let (context, projections, session) = setup();
        append(&session, "turn/start", json!({ "turn": 1 }));
        append(&session, "step/start", json!({ "turn": 1, "step": 1 }));
        append(&session, "step/end", json!({ "turn": 1, "step": 1 }));
        append(&session, "turn/end", json!({ "turn": 1 }));
        assert!(
            projections
                .snapshot(&session)
                .expect("without stats")
                .values
                .is_empty()
        );

        let installation = install(&context, &projections).expect("install stats");
        let initial: SessionStats = serde_json::from_value(
            projections
                .snapshot(&session)
                .expect("late snapshot")
                .values[SESSION_STATS_KEY]
                .clone(),
        )
        .expect("stats");
        assert_eq!(initial.turns, 1);
        assert_eq!(initial.steps, 1);

        let changes = Arc::new(Mutex::new(Vec::new()));
        let seen = changes.clone();
        projections
            .on_changed(
                &context,
                Arc::new(move |_, key, value, seq| {
                    seen.lock().push((key.to_owned(), value.clone(), seq));
                    Ok(())
                }),
            )
            .expect("change listener");
        append(&session, "turn/start", json!({ "turn": 2 }));
        append(&session, "step/start", json!({ "turn": 2, "step": 1 }));
        let closed = append(&session, "step/end", json!({ "turn": 2, "step": 1 }));
        assert!(changes.lock().iter().any(|(key, value, seq)| {
            key == SESSION_STATS_KEY
                && *seq == closed.seq
                && value.get("turns") == Some(&json!(2))
                && value.get("steps") == Some(&json!(2))
        }));

        installation.dispose().await.expect("uninstall stats");
        assert!(
            projections
                .snapshot(&session)
                .expect("after disposal")
                .values
                .is_empty()
        );
    }

    #[tokio::test]
    async fn wire_shape_and_invariant_companion_are_exact() {
        assert_eq!(
            serde_json::to_value(SessionStats::default()).expect("stats JSON"),
            json!({
                "turns": 0,
                "steps": 0,
                "llmMs": 0,
                "toolMs": 0,
                "ttftMs": 0,
                "ttftSteps": 0,
                "decodeMs": 0,
                "decodeTokens": 0.0,
            })
        );
        let context = Context::new();
        let invariants =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
        let registration = register_invariant(&invariants).expect("register invariant");
        registration.await_ready().await.expect("ready");
        assert!(invariants.is_registered("seekdeep-session-stats"));
        registration.dispose().await.expect("dispose");
        assert!(!invariants.is_registered("seekdeep-session-stats"));
    }
}
