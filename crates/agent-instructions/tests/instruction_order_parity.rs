//! Baseline instruction changes keep the order the instruction files were loaded in.

use seekdeep_agent_instructions::render::LoadedInstructionFile;
use seekdeep_agent_instructions::state::baseline_instruction_state;

#[test]
fn baseline_changes_follow_load_order() {
    // The change list reaches a model-visible user message, so its order is part of that contract;
    // the oracle keeps the baseline in a Map and builds the list from it. Five names, because the
    // scopes are strings and a hash-ordered map would be unlikely to reproduce this order by chance.
    let files: Vec<LoadedInstructionFile> = ["alpha", "beta", "gamma", "delta", "epsilon"]
        .iter()
        .map(|name| LoadedInstructionFile {
            absolute_path: format!("/project/{name}/AGENTS.md"),
            display_path: format!("{name}/AGENTS.md"),
            content: format!("content for {name}"),
            version: None,
        })
        .collect();

    let state = baseline_instruction_state(&files);
    let ordered: Vec<String> = state
        .changes
        .values()
        .map(|change| change.path.clone())
        .collect();
    let expected: Vec<String> = files.iter().map(|file| file.display_path.clone()).collect();

    assert_eq!(ordered, expected, "changes must follow load order");
}
