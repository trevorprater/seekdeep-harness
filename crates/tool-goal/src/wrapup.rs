//! Model-visible wrap-up instruction for a terminal autonomous goal update.

use seekdeep_llm::ContentBlock;
use serde_json::json;

const GROUNDING: &str = "Report only what earlier rounds and tool results in this session actually establish; when a detail is not in the session, say so instead of inventing it. ";

/// Renders the closing-message instruction injected after an autonomous goal
/// round reports `complete` or `blocked`.
#[must_use]
pub fn render_wrapup_context(objective: &str, blocked_reason: Option<&str>) -> Vec<ContentBlock> {
    let heading = format!("Objective: {}\n", json!(objective));
    let text = match blocked_reason {
        None => format!(
            "<goal_complete>\n{heading}The goal is marked complete and this autonomous run is ending. Write the closing message to the user now: state the outcome, summarize what was done and how it was verified, and point to the concrete results (files, commits, or other artifacts). {GROUNDING}Note anything the user should review or do next. Address the user directly. Do not call any more tools in this run; further work waits for the user's next instruction.\n</goal_complete>"
        ),
        Some(reason) => format!(
            "<goal_blocked>\n{heading}Blocked: {}\nThe goal is marked blocked and this autonomous run is ending. Write the closing message to the user now: state what has been completed so far, describe the concrete blocking condition and what you tried, and say exactly what you need from the user to continue. {GROUNDING}Address the user directly. Do not call any more tools in this run; further work waits for the user's next instruction.\n</goal_blocked>",
            json!(reason)
        ),
    };
    vec![ContentBlock::Text { text }]
}
