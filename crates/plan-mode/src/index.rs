//! Plan mode pure fold and validation.

use std::sync::LazyLock;

use regex::Regex;
use seekdeep_core::session::SessionEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The model-facing exit tool's name.
pub const EXIT_PLAN_MODE: &str = "exit_plan_mode";

/// The review question's id.
pub const REVIEW_ID: &str = "plan-review";
/// The review question's approve option label.
pub const APPROVE_LABEL: &str = "Approve";
/// The review question's keep-planning option label.
pub const KEEP_PLANNING_LABEL: &str = "Keep planning";

/// The model-facing exit tool description.
pub const EXIT_DESCRIPTION: &str = "Use only in plan mode. Present your plan for the user's review and, on approval, leave plan mode. Send the COMPLETE plan as markdown, starting with a # heading that names it. The user may approve (carry out the plan from your next step) or keep planning — their feedback comes back in the tool result; revise and present again.";

/// Deployment-owned plan guidance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanModeConfig {
    /// Guidance rendered while plan mode is active.
    pub section: String,
}

static HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#{1,6}\s+(.+?)\s*$").expect("static heading regex"));

/// The plan's first markdown heading (any level), or none.
#[must_use]
pub fn first_heading(plan: &str) -> Option<String> {
    for line in plan.split('\n') {
        if let Some(captures) = HEADING.captures(line) {
            if let Some(heading) = captures.get(1) {
                return Some(heading.as_str().to_owned());
            }
        }
    }
    None
}

/// Validates deployment-owned plan guidance.
///
/// # Errors
///
/// Returns a blank-section failure.
pub fn resolve_config(config: &PlanModeConfig) -> anyhow::Result<PlanModeConfig> {
    if config.section.trim().is_empty() {
        anyhow::bail!("PlanModeConfig needs a non-empty 'section'");
    }
    Ok(config.clone())
}

/// Whether plan mode is active after the first end events.
#[must_use]
pub fn fold_plan_mode(events: &[SessionEvent], end: usize) -> bool {
    let mut active = false;
    for event in events.iter().take(end) {
        if event.event_type == "plan/mode" {
            active = event
                .data
                .get("active")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        }
    }
    active
}

/// Whether the log holds an opened turn without its closing turn/end.
#[must_use]
pub fn has_open_turn(events: &[SessionEvent]) -> bool {
    let mut open = false;
    for event in events {
        match event.event_type.as_str() {
            "turn/start" => open = true,
            "turn/end" => open = false,
            _ => {}
        }
    }
    open
}

/// Plan state at the last logged request header, or none before the first header.
#[must_use]
pub fn plan_mode_at_last_header(events: &[SessionEvent]) -> Option<bool> {
    let mut last_header = None;
    for (index, event) in events.iter().enumerate() {
        if event.event_type == "request/header" {
            last_header = Some(index);
        }
    }
    Some(fold_plan_mode(events, last_header? + 1))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(event_type: &str, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq: 0,
            time: 0,
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn fold_is_last_plan_mode_wins() {
        let events = vec![
            event("plan/mode", json!({"active": true})),
            event("plan/mode", json!({"active": false})),
            event("plan/mode", json!({"active": true})),
        ];
        assert!(fold_plan_mode(&events, events.len()));
        assert!(!fold_plan_mode(&events, 2));
        assert!(!fold_plan_mode(&[], 0));
    }

    #[test]
    fn first_heading_extracts_any_level() {
        assert_eq!(first_heading("# My plan\nrest"), Some("My plan".to_owned()));
        assert_eq!(
            first_heading("## Deep plan  \nmore"),
            Some("Deep plan".to_owned())
        );
        assert_eq!(first_heading("no heading"), None);
    }

    #[test]
    fn open_turn_detection_and_last_header() {
        let events = vec![
            event("turn/start", json!({})),
            event("plan/mode", json!({"active": true})),
            event("request/header", json!({})),
            event("plan/mode", json!({"active": false})),
        ];
        assert!(has_open_turn(&events));
        assert_eq!(plan_mode_at_last_header(&events), Some(true));
        let closed = vec![event("turn/start", json!({})), event("turn/end", json!({}))];
        assert!(!has_open_turn(&closed));
    }

    #[test]
    fn config_requires_non_empty_section() {
        assert!(
            resolve_config(&PlanModeConfig {
                section: "  ".to_owned()
            })
            .is_err()
        );
        let ok = resolve_config(&PlanModeConfig {
            section: "guidance".to_owned(),
        })
        .expect("ok");
        assert_eq!(ok.section, "guidance");
    }
}
