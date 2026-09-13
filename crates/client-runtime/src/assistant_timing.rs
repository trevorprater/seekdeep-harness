//! Shared assistant step timing fold for Chat and Trajectory projections.

use std::collections::BTreeMap;

/// Stream chunk fields relevant to the visible first-token boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistantStreamChunk {
    /// Text output delta.
    TextDelta {
        /// Appended text.
        text: String,
    },
    /// Reasoning output delta.
    ReasoningDelta {
        /// Appended reasoning text.
        text: String,
    },
    /// Tool-call name or arguments delta.
    ToolCallDelta {
        /// Tool name when first announced.
        name: Option<String>,
        /// Appended JSON argument text.
        arguments_delta: String,
    },
    /// Heartbeat, lifecycle, usage, or another non-token chunk.
    Other,
}

/// Whether a stream chunk carries non-empty visible model output.
#[must_use]
pub fn is_token_delta(chunk: &AssistantStreamChunk) -> bool {
    match chunk {
        AssistantStreamChunk::TextDelta { text }
        | AssistantStreamChunk::ReasoningDelta { text } => !text.is_empty(),
        AssistantStreamChunk::ToolCallDelta {
            name,
            arguments_delta,
        } => name.is_some() || !arguments_delta.is_empty(),
        AssistantStreamChunk::Other => false,
    }
}

/// Timing-relevant Session event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistantTimingEvent {
    /// Step lifecycle start.
    StepStart {
        /// Turn number.
        turn: i64,
        /// Step number.
        step: i64,
        /// Event epoch milliseconds.
        time: i64,
    },
    /// Assistant stream chunk.
    AssistantChunk {
        /// Turn number.
        turn: i64,
        /// Step number.
        step: i64,
        /// Event epoch milliseconds.
        time: i64,
        /// Stream chunk.
        chunk: AssistantStreamChunk,
    },
    /// Any other event.
    Other,
}

/// Pre-finalize timing boundaries for one assistant step.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssistantStepMetadata {
    /// Step start epoch milliseconds.
    pub step_start_time: Option<i64>,
    /// First non-empty token epoch milliseconds.
    pub first_token_time: Option<i64>,
}

/// Finalized assistant timing record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssistantTiming {
    /// Step start epoch milliseconds, absent when outside the window.
    pub step_start_time: Option<i64>,
    /// First token epoch milliseconds, absent when outside the window.
    pub first_token_time: Option<i64>,
    /// Final assistant message epoch milliseconds.
    pub completed_time: i64,
}

/// Collision-free composite map key for one turn and step.
#[must_use]
pub fn assistant_step_key(turn: i64, step: i64) -> String {
    format!("{turn}\0{step}")
}

/// Folds one raw event into the per-step timing index.
pub fn index_assistant_step_timing(
    steps: &mut BTreeMap<String, AssistantStepMetadata>,
    event: &AssistantTimingEvent,
) {
    match event {
        AssistantTimingEvent::StepStart { turn, step, time } => {
            steps.insert(
                assistant_step_key(*turn, *step),
                AssistantStepMetadata {
                    step_start_time: Some(*time),
                    first_token_time: None,
                },
            );
        }
        AssistantTimingEvent::AssistantChunk {
            turn,
            step,
            time,
            chunk,
        } if is_token_delta(chunk) => {
            let metadata = steps.entry(assistant_step_key(*turn, *step)).or_default();
            if metadata.first_token_time.is_none() {
                metadata.first_token_time = Some(*time);
            }
        }
        AssistantTimingEvent::AssistantChunk { .. } | AssistantTimingEvent::Other => {}
    }
}

/// Settles final timing from one indexed step, preserving absent window boundaries.
#[must_use]
pub fn settled_assistant_timing(
    steps: &BTreeMap<String, AssistantStepMetadata>,
    turn: i64,
    step: i64,
    completed_time: i64,
) -> AssistantTiming {
    let metadata = steps
        .get(&assistant_step_key(turn, step))
        .cloned()
        .unwrap_or_default();
    AssistantTiming {
        step_start_time: metadata.step_start_time,
        first_token_time: metadata.first_token_time,
        completed_time,
    }
}
