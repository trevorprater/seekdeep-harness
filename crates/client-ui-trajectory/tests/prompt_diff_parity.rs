//! Unified prompt-diff parity with the pinned `diff@9` source dependency.

use seekdeep_client_ui_trajectory::{
    TrajectoryPromptDiffKind, TrajectoryPromptDiffLine, trajectory_prompt_diff_lines,
};

fn compact(lines: &[TrajectoryPromptDiffLine]) -> Vec<(TrajectoryPromptDiffKind, &str)> {
    lines
        .iter()
        .map(|line| (line.kind, line.text.as_str()))
        .collect()
}

#[test]
fn one_replacement_matches_structured_patch_context_and_header() {
    assert_eq!(
        compact(&trajectory_prompt_diff_lines("a\nb\nc\n", "a\nB\nc\n")),
        vec![
            (TrajectoryPromptDiffKind::Meta, "@@ -1,3 +1,3 @@"),
            (TrajectoryPromptDiffKind::Context, " a"),
            (TrajectoryPromptDiffKind::Removed, "-b"),
            (TrajectoryPromptDiffKind::Added, "+B"),
            (TrajectoryPromptDiffKind::Context, " c"),
        ]
    );
}

#[test]
fn distant_changes_split_hunks_with_one_blank_meta_line() {
    assert_eq!(
        compact(&trajectory_prompt_diff_lines(
            "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n",
            "a\nB\nc\nd\ne\nf\ng\nh\ni\nJ\n",
        )),
        vec![
            (TrajectoryPromptDiffKind::Meta, "@@ -1,5 +1,5 @@"),
            (TrajectoryPromptDiffKind::Context, " a"),
            (TrajectoryPromptDiffKind::Removed, "-b"),
            (TrajectoryPromptDiffKind::Added, "+B"),
            (TrajectoryPromptDiffKind::Context, " c"),
            (TrajectoryPromptDiffKind::Context, " d"),
            (TrajectoryPromptDiffKind::Context, " e"),
            (TrajectoryPromptDiffKind::Meta, ""),
            (TrajectoryPromptDiffKind::Meta, "@@ -7,4 +7,4 @@"),
            (TrajectoryPromptDiffKind::Context, " g"),
            (TrajectoryPromptDiffKind::Context, " h"),
            (TrajectoryPromptDiffKind::Context, " i"),
            (TrajectoryPromptDiffKind::Removed, "-j"),
            (TrajectoryPromptDiffKind::Added, "+J"),
        ]
    );
}

#[test]
fn empty_insert_delete_and_equal_inputs_match_structured_patch() {
    assert_eq!(
        compact(&trajectory_prompt_diff_lines("", "one\n")),
        vec![
            (TrajectoryPromptDiffKind::Meta, "@@ -1,0 +1,1 @@"),
            (TrajectoryPromptDiffKind::Added, "+one"),
        ]
    );
    assert_eq!(
        compact(&trajectory_prompt_diff_lines("one\n", "")),
        vec![
            (TrajectoryPromptDiffKind::Meta, "@@ -1,1 +1,0 @@"),
            (TrajectoryPromptDiffKind::Removed, "-one"),
        ]
    );
    assert!(trajectory_prompt_diff_lines("same", "same").is_empty());
}
