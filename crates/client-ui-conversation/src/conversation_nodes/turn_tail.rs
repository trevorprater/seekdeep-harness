use std::{collections::BTreeMap, rc::Rc};

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, AssistantBlock, ConversationAssemblerError, ConversationLocation,
    ConversationLocationData, ConversationLocationDataScope, ConversationMatchResult,
    ConversationMatchRole, ConversationNodeContext, ConversationPublication, ConversationViewNode,
    to_assistant_block,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{AssistantMetricNode, AssistantTiming, derive_turn_metrics};

use super::{
    ASSISTANT_STEP_KIND, CHAT_FINALIZED_FOLLOWUP_OFFSET, CHAT_INTERRUPTED_FOLLOWUP_OFFSET,
    chat_node, command::EventEvidence, conversation_coordinate, is_append_surface_event, js_string,
    sequence_anchor,
};

/// Completed-turn footer definition kind and Location-data key.
pub const TURN_TAIL_KIND: &str = "turn-tail";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TurnTailState {
    turn: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end: Option<EventEvidence>,
}

#[derive(Clone, Copy, Default)]
struct StepEvidence {
    streamed_text: bool,
    finalized: bool,
}

/// Builds the completed-turn footer and Turn-scoped summary definition.
#[must_use]
pub fn conversation_turn_tail_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TURN_TAIL_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| {
            let turn = if matches!(
                event.event_type.as_str(),
                "turn/start" | "turn/end" | "tool/call" | "tool/result"
            ) {
                event.data.get("turn").cloned()
            } else {
                turn_coordinates(&event.event_type, &event.data).map(|(turn, _)| json!(turn))
            };
            Ok(turn.as_ref().map(|turn| ConversationMatchResult {
                id: js_string(turn),
                role: if event.event_type == "turn/start" {
                    ConversationMatchRole::Start
                } else {
                    ConversationMatchRole::Update
                },
            }))
        }),
        start: Rc::new(|_context, accepted, _reader| {
            if accepted.event.event_type != "turn/start" {
                return Err(ConversationAssemblerError::new(
                    "turn-tail start requires turn/start",
                ));
            }
            encode(&TurnTailState {
                turn: required_coordinate(&accepted.event.data, "turn")?,
                end: None,
            })
            .map(Some)
        }),
        update: Rc::new(|context, accepted| {
            if accepted.event.event_type != "turn/end" {
                return Ok(context.state.clone());
            }
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let mut state = decode::<TurnTailState>(state)?;
            state.end = Some(EventEvidence::from(accepted.as_ref()));
            encode(&state).map(Some)
        }),
        publication: Some(Rc::new(|accepted| {
            Ok(if accepted.event.event_type == "turn/end" {
                ConversationPublication::Immediate
            } else {
                ConversationPublication::None
            })
        })),
        build_location_data: Some(Rc::new(build_location_data)),
        build_view_node: Some(Rc::new(|context| Ok(build_view_node(context)))),
    }
}

fn build_location_data(
    context: &ConversationNodeContext,
    scope: ConversationLocationDataScope,
) -> Result<Option<Rc<ConversationLocationData>>, ConversationAssemblerError> {
    if scope != ConversationLocationDataScope::Turn {
        return Ok(None);
    }
    let Some(value) = tail_data(context)? else {
        return Ok(None);
    };
    let turn = value
        .get("turn")
        .and_then(Value::as_u64)
        .ok_or_else(|| ConversationAssemblerError::new("turn-tail data omitted turn"))?;
    Ok(Some(Rc::new(ConversationLocationData::Turn {
        turn,
        key: TURN_TAIL_KIND.to_owned(),
        value: Rc::new(value),
    })))
}

fn build_view_node(context: &ConversationNodeContext) -> Option<Rc<ConversationViewNode>> {
    let turn = turn_location(context)?;
    let data = turn.data.get(TURN_TAIL_KIND)?;
    Some(chat_node(
        context,
        TURN_TAIL_KIND,
        closing_anchor(context),
        data.as_ref().clone(),
    ))
}

fn tail_data(
    context: &ConversationNodeContext,
) -> Result<Option<Value>, ConversationAssemblerError> {
    let state = context
        .state
        .as_deref()
        .map(decode::<TurnTailState>)
        .transpose()?;
    let end = state
        .as_ref()
        .and_then(|state| state.end.as_ref())
        .cloned()
        .or_else(|| {
            context
                .matches
                .borrow()
                .iter()
                .find(|accepted| accepted.event.event_type == "turn/end")
                .map(|accepted| EventEvidence::from(accepted.as_ref()))
        });
    let Some(end) = end.filter(|end| end.event_type == "turn/end") else {
        return Ok(None);
    };
    let Some(turn) = turn_location(context) else {
        return Ok(None);
    };
    let mut finalized = turn
        .steps
        .iter()
        .filter_map(|step| step.data.get(ASSISTANT_STEP_KIND))
        .filter(|assistant| assistant.get("finalNode").is_some())
        .map(|assistant| assistant.as_ref().clone())
        .collect::<Vec<_>>();
    finalized.sort_by(|left, right| {
        final_seq(left)
            .partial_cmp(&final_seq(right))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let closing = finalized
        .iter()
        .rev()
        .find(|assistant| assistant_has_text(assistant))
        .cloned();
    let mut latest_transcript_seq = finalized.last().map(final_seq);
    for accepted in context.matches.borrow().iter() {
        let event = &accepted.event;
        let candidate = if event.event_type == "tool/call"
            || (event.event_type == "tool/result" && is_append_surface_event(event))
            || (event.event_type == "turn/end"
                && event
                    .data
                    .get("reason")
                    .and_then(|reason| reason.get("kind"))
                    .and_then(Value::as_str)
                    == Some("error"))
            || event.event_type == "llm/retry"
        {
            Some(sequence_anchor(event.seq))
        } else {
            None
        };
        if candidate
            .is_some_and(|candidate| latest_transcript_seq.is_none_or(|known| candidate > known))
        {
            latest_transcript_seq = candidate;
        }
    }
    let closing_seq = closing.as_ref().map(final_seq);
    let metrics = derive_turn_metrics(
        &finalized
            .iter()
            .filter_map(assistant_metric)
            .collect::<Vec<_>>(),
    )
    .get(&required_coordinate(&end.data, "turn")?)
    .copied();
    let mut value = Map::from_iter([
        (
            "turn".to_owned(),
            end.data.get("turn").cloned().unwrap_or(Value::Null),
        ),
        ("seq".to_owned(), json!(end.seq)),
        ("time".to_owned(), json!(end.time)),
        ("closing".to_owned(), closing.unwrap_or(Value::Null)),
        (
            "branchUnavailable".to_owned(),
            json!(closing_seq.is_none() || latest_transcript_seq != closing_seq),
        ),
    ]);
    if let Some(ttft) = metrics.and_then(|metrics| metrics.ttft_ms) {
        value.insert("ttftMs".to_owned(), json!(ttft));
    }
    if let Some(rate) = metrics.and_then(|metrics| metrics.tokens_per_second) {
        value.insert("tokensPerSecond".to_owned(), json!(rate));
    }
    Ok(Some(Value::Object(value)))
}

fn closing_anchor(context: &ConversationNodeContext) -> f64 {
    let matches = context.matches.borrow();
    let mut anchor = matches
        .iter()
        .find(|accepted| accepted.event.event_type == "turn/end")
        .map(|accepted| sequence_anchor(accepted.event.seq))
        .or_else(|| {
            context
                .start
                .as_ref()
                .map(|start| sequence_anchor(start.event.seq))
        })
        .or_else(|| {
            matches
                .first()
                .map(|accepted| sequence_anchor(accepted.event.seq))
        })
        .unwrap_or(0.0);
    let mut steps = BTreeMap::<u64, StepEvidence>::new();
    for accepted in matches.iter() {
        let event = &accepted.event;
        if event.event_type == "turn/end" {
            continue;
        }
        let Some((_, Some(step))) = turn_coordinates(&event.event_type, &event.data) else {
            continue;
        };
        let previous = steps.get(&step).copied().unwrap_or_default();
        match event.event_type.as_str() {
            "assistant/chunk" => {
                steps.insert(
                    step,
                    StepEvidence {
                        streamed_text: previous.streamed_text || chunk_has_text(&event.data),
                        ..previous
                    },
                );
            }
            "assistant/message" => {
                steps.insert(
                    step,
                    StepEvidence {
                        streamed_text: false,
                        finalized: true,
                    },
                );
                if has_text_assistant(event) {
                    anchor = sequence_anchor(event.seq) + CHAT_FINALIZED_FOLLOWUP_OFFSET;
                }
            }
            "llm/retry" => {
                steps.insert(step, StepEvidence::default());
            }
            "step/end" if previous.streamed_text && !previous.finalized => {
                anchor = sequence_anchor(event.seq) + CHAT_INTERRUPTED_FOLLOWUP_OFFSET;
            }
            _ => {}
        }
    }
    anchor
}

fn turn_location(
    context: &ConversationNodeContext,
) -> Option<Rc<seekdeep_client_runtime::TurnLocation>> {
    let location = context
        .start
        .as_ref()
        .map(|start| start.location.clone())
        .or_else(|| {
            context
                .matches
                .borrow()
                .first()
                .map(|accepted| accepted.location.clone())
        })?;
    match location {
        ConversationLocation::Turn { turn } | ConversationLocation::Step { turn, .. } => Some(turn),
        ConversationLocation::Session | ConversationLocation::Unresolved => None,
    }
}

fn turn_coordinates(event_type: &str, data: &Value) -> Option<(u64, Option<u64>)> {
    if matches!(
        event_type,
        "assistant/message" | "assistant/chunk" | "step/end" | "llm/retry"
    ) {
        Some((
            data.get("turn").and_then(conversation_coordinate)?,
            data.get("step").and_then(conversation_coordinate),
        ))
    } else {
        None
    }
}

fn has_text_assistant(event: &seekdeep_client_runtime::ConversationLocationEvent) -> bool {
    event.event_type == "assistant/message"
        && is_append_surface_event(event)
        && event
            .data
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .is_some_and(|content| {
                content.iter().any(|block| match to_assistant_block(block) {
                    AssistantBlock::Text { text } => !text.trim().is_empty(),
                    _ => false,
                })
            })
}

fn chunk_has_text(data: &Value) -> bool {
    let Some(chunk) = data.get("chunk") else {
        return false;
    };
    match chunk.get("type").and_then(Value::as_str) {
        Some("text-delta") => chunk
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty()),
        Some("block-end") => chunk.get("block").is_some_and(|block| {
            block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
        }),
        _ => false,
    }
}

fn assistant_has_text(assistant: &Value) -> bool {
    assistant
        .get("finalNode")
        .is_some_and(|final_node| !final_node.is_null())
        && assistant
            .get("blocks")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block.get("kind").and_then(Value::as_str) == Some("text")
                        && block
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.trim().is_empty())
                })
            })
}

fn final_seq(assistant: &Value) -> f64 {
    assistant
        .get("finalNode")
        .and_then(|node| node.get("seq"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

fn assistant_metric(assistant: &Value) -> Option<AssistantMetricNode> {
    let final_node = assistant.get("finalNode")?;
    let timing = final_node.get("timing").and_then(|timing| {
        Some(AssistantTiming {
            step_start_time: timing.get("stepStartTime").and_then(Value::as_f64),
            first_token_time: timing.get("firstTokenTime").and_then(Value::as_f64),
            completed_time: timing.get("completedTime")?.as_f64()?,
        })
    });
    Some(AssistantMetricNode {
        turn: assistant.get("turn")?.as_u64()?,
        step: assistant.get("step")?.as_u64()?,
        timing,
        output_tokens: final_node
            .get("usage")
            .and_then(|usage| usage.get("outputTokens"))
            .and_then(Value::as_f64),
    })
}

fn required_coordinate(value: &Value, key: &str) -> Result<u64, ConversationAssemblerError> {
    value
        .get(key)
        .and_then(conversation_coordinate)
        .ok_or_else(|| ConversationAssemblerError::new(format!("turn-tail event omitted {key}")))
}

fn encode(state: &TurnTailState) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(state)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn decode<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
