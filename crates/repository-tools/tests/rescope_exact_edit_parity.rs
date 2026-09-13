//! Insertion, deletion, replacement, duplicate, and moved-site fixtures.

use seekdeep_repository_tools::rescope_exact_edit::{ExactEditState, exact_edit_state};

#[test]
fn insertion_counts_the_complete_target_form() {
    let anchor = "\n## Sync procedure";
    let inserted = format!("\n15. **rescope**: one log entry.\n{anchor}");
    assert_eq!(
        exact_edit_state(&format!("log\n{anchor}\n"), anchor, &inserted, 1),
        ExactEditState::Pending
    );
    assert_eq!(
        exact_edit_state(&format!("log{inserted}\n"), anchor, &inserted, 1),
        ExactEditState::Applied
    );
    assert_eq!(
        exact_edit_state(&format!("log{inserted}{inserted}\n"), anchor, &inserted, 1),
        ExactEditState::Invalid
    );
    assert_eq!(
        exact_edit_state("log\n", anchor, &inserted, 1),
        ExactEditState::Invalid
    );
}

#[test]
fn deletion_requires_its_remainder_to_survive() {
    let remainder = "exclude:\n";
    let source = "exclude:\n  - cordis@4\n";
    assert_eq!(
        exact_edit_state(source, source, remainder, 1),
        ExactEditState::Pending
    );
    assert_eq!(
        exact_edit_state(remainder, source, remainder, 1),
        ExactEditState::Applied
    );
    assert_eq!(
        exact_edit_state("unrelated:\n", source, remainder, 1),
        ExactEditState::Invalid
    );
}

#[test]
fn replacement_requires_no_source_and_exact_target_count() {
    assert_eq!(
        exact_edit_state("a = 1\n", "a = 1", "b = 2", 1),
        ExactEditState::Pending
    );
    assert_eq!(
        exact_edit_state("b = 2\n", "a = 1", "b = 2", 1),
        ExactEditState::Applied
    );
    for text in ["b = 2\nb = 2\n", "a = 1\nb = 2\n", "x\n"] {
        assert_eq!(
            exact_edit_state(text, "a = 1", "b = 2", 1),
            ExactEditState::Invalid
        );
    }
}
