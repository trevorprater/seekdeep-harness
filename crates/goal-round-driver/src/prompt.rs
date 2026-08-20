//! Model-visible continuation prompt for one same-session goal round.

use seekdeep_goal::GoalView;
use seekdeep_llm::ContentBlock;

/// Renders the complete goal-round instruction retained in session history.
///
/// # Panics
///
/// Panics if the objective cannot be JSON-serialized, which cannot happen.
#[must_use]
pub fn render_goal_round_prompt(goal: &GoalView, round: u64) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: format!(
            "<goal_round>
Objective: {}
Round: {}/{}

Continue working toward the objective in this same session. Treat the current workspace, tool results, and durable session state as authoritative; inspect them instead of assuming earlier narration is still current. Make concrete progress and verify the result. Before claiming completion, gather evidence that the whole objective is achieved, read the current goal, and mark it complete. If work remains, leave the goal active for the next round. Follow the configured goal-tool policy before reporting a blocker.
</goal_round>",
            serde_json::to_string(&goal.objective).expect("objective serializes"),
            round,
            goal.max_goal_rounds,
        ),
    }]
}
