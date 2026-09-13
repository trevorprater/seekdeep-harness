//! First-token and finalized assistant timing parity.

use std::collections::BTreeMap;

use seekdeep_client_runtime::*;

#[test]
fn token_delta_ignores_empty_frames_and_accepts_text_reasoning_name_or_arguments() {
    assert!(!is_token_delta(&AssistantStreamChunk::TextDelta {
        text: String::new()
    }));
    assert!(is_token_delta(&AssistantStreamChunk::TextDelta {
        text: "x".to_owned()
    }));
    assert!(is_token_delta(&AssistantStreamChunk::ReasoningDelta {
        text: "r".to_owned()
    }));
    assert!(!is_token_delta(&AssistantStreamChunk::ToolCallDelta {
        name: None,
        arguments_delta: String::new(),
    }));
    assert!(is_token_delta(&AssistantStreamChunk::ToolCallDelta {
        name: Some("bash".to_owned()),
        arguments_delta: String::new(),
    }));
    assert!(is_token_delta(&AssistantStreamChunk::ToolCallDelta {
        name: None,
        arguments_delta: "{".to_owned(),
    }));
    assert!(!is_token_delta(&AssistantStreamChunk::Other));
}

#[test]
fn step_key_uses_a_collision_free_nul_separator() {
    assert_eq!(assistant_step_key(12, 3), "12\u{0}3");
    assert_ne!(assistant_step_key(1, 23), assistant_step_key(12, 3));
}

#[test]
fn timing_index_records_start_and_only_the_first_nonempty_token() {
    let mut steps = BTreeMap::new();
    index_assistant_step_timing(
        &mut steps,
        &AssistantTimingEvent::StepStart {
            turn: 2,
            step: 1,
            time: 100,
        },
    );
    index_assistant_step_timing(
        &mut steps,
        &AssistantTimingEvent::AssistantChunk {
            turn: 2,
            step: 1,
            time: 110,
            chunk: AssistantStreamChunk::TextDelta {
                text: String::new(),
            },
        },
    );
    for (time, text) in [(120, "first"), (130, "later")] {
        index_assistant_step_timing(
            &mut steps,
            &AssistantTimingEvent::AssistantChunk {
                turn: 2,
                step: 1,
                time,
                chunk: AssistantStreamChunk::TextDelta {
                    text: text.to_owned(),
                },
            },
        );
    }
    assert_eq!(
        steps[&assistant_step_key(2, 1)],
        AssistantStepMetadata {
            step_start_time: Some(100),
            first_token_time: Some(120),
        }
    );
}

#[test]
fn missing_start_or_whole_step_degrades_boundaries_to_none() {
    let mut steps = BTreeMap::new();
    index_assistant_step_timing(
        &mut steps,
        &AssistantTimingEvent::AssistantChunk {
            turn: 3,
            step: 4,
            time: 200,
            chunk: AssistantStreamChunk::ReasoningDelta {
                text: "first".to_owned(),
            },
        },
    );
    assert_eq!(
        settled_assistant_timing(&steps, 3, 4, 250),
        AssistantTiming {
            step_start_time: None,
            first_token_time: Some(200),
            completed_time: 250,
        }
    );
    assert_eq!(
        settled_assistant_timing(&steps, 9, 9, 300),
        AssistantTiming {
            step_start_time: None,
            first_token_time: None,
            completed_time: 300,
        }
    );
}
