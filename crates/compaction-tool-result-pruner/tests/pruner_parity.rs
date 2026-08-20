//! Behavioral parity tests for the deterministic tool-result pruner.

use seekdeep_compaction_tool_result_pruner::{
    DEFAULTS, PRUNE_MARKER, ToolResultPruneConfig, ToolResultPruner, code_point_length,
    resolve_config,
};
use seekdeep_cordis::Context;
use seekdeep_llm::ContentBlock;

fn default_pruner() -> ToolResultPruner {
    let context = Context::new();
    ToolResultPruner::new(&context, &ToolResultPruneConfig::default())
        .expect("pruner")
        .as_ref()
        .clone()
}

#[test]
fn counts_code_points_without_splitting_surrogate_pairs() {
    assert_eq!(code_point_length("hello"), 5);
    assert_eq!(code_point_length(""), 0);
    assert_eq!(code_point_length("😀"), 1);
    assert_eq!(code_point_length("a😀b"), 3);
}

#[test]
fn resolves_defaults_and_rejects_oversized_emitted_budget() {
    let resolved = resolve_config(&ToolResultPruneConfig::default()).expect("defaults");
    assert_eq!(resolved.threshold_chars, DEFAULTS.threshold_chars);
    assert_eq!(resolved.head_chars, DEFAULTS.head_chars);
    assert_eq!(resolved.tail_chars, DEFAULTS.tail_chars);

    let oversized = ToolResultPruneConfig {
        threshold_chars: Some(100),
        head_chars: Some(60),
        tail_chars: Some(60),
    };
    let error = resolve_config(&oversized).expect_err("must reject");
    assert!(
        error.to_string().contains("headChars + marker + tailChars"),
        "{error}"
    );
}

#[test]
fn measures_only_text_blocks() {
    let pruner = default_pruner();
    let blocks = vec![
        ContentBlock::Text {
            text: "abcd".to_owned(),
        },
        ContentBlock::Reasoning {
            text: "ignored".to_owned(),
        },
        ContentBlock::Text {
            text: "ef".to_owned(),
        },
    ];
    assert_eq!(pruner.measure_content(&blocks), 6);
}

#[test]
fn prunes_an_over_budget_text_middle() {
    let pruner = default_pruner();
    let text = "a".repeat(9_000);
    let pruned_blocks = pruner
        .prune_content(&[ContentBlock::Text { text }])
        .expect("over-budget must prune");
    let ContentBlock::Text { text } = &pruned_blocks[0] else {
        panic!("expected a text block");
    };
    assert!(
        text.contains(PRUNE_MARKER),
        "{text:?} must carry the marker"
    );
    assert!(text.starts_with(&"a".repeat(DEFAULTS.head_chars)));
    assert!(text.ends_with(&"a".repeat(DEFAULTS.tail_chars)));
    assert!(pruner.measure_content(&pruned_blocks) < 9_000);
}

#[test]
fn leaves_within_budget_content_untouched() {
    let pruner = default_pruner();
    let blocks = [ContentBlock::Text {
        text: "short".to_owned(),
    }];
    assert!(pruner.prune_content(&blocks).is_none());
}
