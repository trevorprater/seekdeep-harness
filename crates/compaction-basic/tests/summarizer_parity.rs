//! Behavioral parity tests for the compaction checkpoint framing.

use seekdeep_compaction_basic::summarizer::{SummarizationInput, frame_summary};
use seekdeep_llm::ContentBlock;

#[test]
fn frames_summary_with_preamble_and_tags() {
    let summary = vec![ContentBlock::Text {
        text: "## Primary Request\nbuild it".to_owned(),
    }];
    let framed = frame_summary(&summary);
    assert_eq!(framed.len(), 3);
    let ContentBlock::Text { text: preamble } = &framed[0] else {
        panic!("expected preamble text block");
    };
    assert!(
        preamble.starts_with("This is an automatically generated checkpoint"),
        "{preamble}"
    );
    assert!(preamble.ends_with("<compacted-summary>"), "{preamble}");
    let ContentBlock::Text { text: close } = &framed[2] else {
        panic!("expected close text block");
    };
    assert_eq!(close, "</compacted-summary>");
    assert_eq!(framed[1], summary[0]);
}

#[test]
fn exposes_summarization_input_shape() {
    let input = SummarizationInput {
        system: None,
        tools: None,
        messages: Vec::new(),
    };
    assert!(input.system.is_none());
    assert!(input.messages.is_empty());
}
