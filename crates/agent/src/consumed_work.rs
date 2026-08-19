//! Durable accounting for work consumed or dropped by an agent log.

use std::collections::HashSet;

use seekdeep_core::session::SessionEvent;

/// How one agent log accounts for the work it consumed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConsumedWork {
    /// Latest closed turn that entered a step or claimed then failed/stopped.
    pub end: Option<SessionEvent>,
    /// Whether accepted input was canceled unrun after that turn.
    pub dropped_unrun: bool,
}

/// Folds a complete log or owned suffix into consumed-work accounting.
#[must_use]
pub fn fold_consumed_work(events: &[SessionEvent]) -> ConsumedWork {
    let mut stepped = HashSet::new();
    let mut claimed = HashSet::new();
    let mut open = None;
    let mut end = None;
    let mut dropped_unrun = false;
    for event in events {
        match event.event_type.as_str() {
            "turn/start" => {
                open = event.data.get("turn").and_then(serde_json::Value::as_u64);
            }
            "step/start" => {
                if let Some(turn) = event.data.get("turn").and_then(serde_json::Value::as_u64) {
                    stepped.insert(turn);
                }
            }
            "agent/inbox/spliced" => {
                if event.data.get("removedCount").is_none() {
                    continue;
                }
                if event
                    .data
                    .get("outcome")
                    .and_then(serde_json::Value::as_str)
                    == Some("canceled")
                {
                    dropped_unrun |= event
                        .data
                        .get("inserted")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(Vec::is_empty);
                } else if let Some(turn) = open {
                    claimed.insert(turn);
                }
            }
            "turn/end" => {
                let Some(turn) = event.data.get("turn").and_then(serde_json::Value::as_u64) else {
                    open = None;
                    continue;
                };
                open = None;
                let reason = event
                    .data
                    .get("reason")
                    .and_then(|reason| reason.get("kind"))
                    .and_then(serde_json::Value::as_str);
                let accounts_for_claim = reason != Some("completed");
                if stepped.remove(&turn) || (claimed.remove(&turn) && accounts_for_claim) {
                    end = Some(event.clone());
                    dropped_unrun = false;
                }
            }
            _ => {}
        }
    }
    ConsumedWork { end, dropped_unrun }
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::{AppendOptions, Session, SessionId};
    use serde_json::json;

    use super::*;

    fn events(items: &[(&str, serde_json::Value)]) -> Vec<SessionEvent> {
        let id = SessionId::new("fold");
        let session = Session::create(&id, None, None).expect("session");
        for (event_type, data) in items {
            session
                .append(*event_type, data.clone(), AppendOptions::default())
                .expect("append");
        }
        session.events()
    }

    #[test]
    fn latest_stepped_turn_accounts_for_prior_drop() {
        let events = events(&[
            (
                "agent/inbox/spliced",
                json!({
                    "target": "next-turn", "start": 0, "removedCount": 1,
                    "inserted": [], "outcome": "canceled"
                }),
            ),
            ("turn/start", json!({"turn": 1})),
            ("step/start", json!({"turn": 1, "step": 1})),
            (
                "step/end",
                json!({"turn": 1, "step": 1, "reason": {"kind": "completed"}}),
            ),
            (
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ]);
        let folded = fold_consumed_work(&events);
        assert_eq!(folded.end.expect("end").data["turn"], 1);
        assert!(!folded.dropped_unrun);
    }

    #[test]
    fn claim_without_step_counts_only_noncompleted_end() {
        let aborted = events(&[
            ("turn/start", json!({"turn": 1})),
            (
                "agent/inbox/spliced",
                json!({
                    "target": "next-turn", "start": 0, "removedCount": 1,
                    "inserted": []
                }),
            ),
            (
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "aborted"}}),
            ),
        ]);
        assert!(fold_consumed_work(&aborted).end.is_some());
        let completed = events(&[
            ("turn/start", json!({"turn": 1})),
            (
                "agent/inbox/spliced",
                json!({
                    "target": "next-turn", "start": 0, "removedCount": 1,
                    "inserted": []
                }),
            ),
            (
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ]);
        assert!(fold_consumed_work(&completed).end.is_none());
    }

    #[test]
    fn empty_log_reports_no_work() {
        assert_eq!(
            fold_consumed_work(&[]),
            ConsumedWork {
                end: None,
                dropped_unrun: false,
            }
        );
    }

    #[test]
    fn ignores_turns_that_stopped_failed_or_blocked_without_claiming() {
        let events = events(&[
            ("turn/start", json!({"turn": 1})),
            ("step/start", json!({"turn": 1, "step": 1})),
            ("step/end", json!({"turn": 1, "step": 1})),
            (
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
            ("turn/start", json!({"turn": 2})),
            (
                "turn/end",
                json!({"turn": 2, "reason": {"kind": "aborted", "reason": {"kind": "parent"}}}),
            ),
            ("turn/start", json!({"turn": 3})),
            (
                "turn/end",
                json!({"turn": 3, "reason": {"kind": "error", "error": {"message": "x", "code": "UNKNOWN"}}}),
            ),
            ("turn/start", json!({"turn": 4})),
            (
                "turn/end",
                json!({"turn": 4, "reason": {"kind": "blocked"}}),
            ),
        ]);
        let folded = fold_consumed_work(&events);
        assert_eq!(folded.end.expect("end").data["turn"], 1);
        assert!(!folded.dropped_unrun);
    }

    #[test]
    fn replacement_is_pending_but_empty_cancellation_is_dropped() {
        let replaced = events(&[(
            "agent/inbox/spliced",
            json!({
                "target": "next-turn", "start": 0, "removedCount": 1,
                "inserted": [{"id": "replacement"}], "outcome": "canceled"
            }),
        )]);
        assert!(!fold_consumed_work(&replaced).dropped_unrun);
        let dropped = events(&[(
            "agent/inbox/spliced",
            json!({
                "target": "next-turn", "start": 0, "removedCount": 1,
                "inserted": [], "outcome": "canceled"
            }),
        )]);
        assert!(fold_consumed_work(&dropped).dropped_unrun);
    }
}
