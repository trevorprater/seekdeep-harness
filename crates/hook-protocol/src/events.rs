//! Durable, log-only hook event helpers.

use seekdeep_core::session::{AppendOptions, Session, SessionError};
use serde_json::{Map, Value, json};

use crate::types::{HookDecision, HookDialect, HookOutput};

/// The reference default character cap for a recorded stderr summary.
pub const DEFAULT_STDERR_SUMMARY_MAX_CHARS: usize = 500;

/// What identifies a hook invocation across its invoked/result pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookInvocation {
    /// The open turn the invocation lives inside.
    pub turn: u64,
    /// The hook point (`PreToolUse`, `Stop`, ...).
    pub point: String,
    /// The bridge dialect that ran it.
    pub dialect: HookDialect,
    /// A stable id correlating the invoked event with its result.
    pub handler_id: String,
    /// The matcher-group pattern that selected it (absent for match-all).
    pub matcher: Option<String>,
}

/// The decided outcome half of the pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookResultRecord {
    /// The open turn the invocation lives inside.
    pub turn: u64,
    /// The hook point.
    pub point: String,
    /// The handler id correlating this result with its invoked event.
    pub handler_id: String,
    /// The decoded outcome the run produced.
    pub output: HookOutput,
    /// Character cap for the derived stderrSummary.
    pub stderr_summary_max_chars: usize,
    /// Wall-clock duration of the run.
    pub duration_ms: i64,
}

/// Trims stderr, drops blank input, and caps it at `max_chars` with an ellipsis when over.
#[must_use]
pub fn summarize_stderr(stderr: &str, max_chars: usize) -> Option<String> {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > max_chars {
        let mut capped: String = trimmed.chars().take(max_chars).collect();
        capped.push('\u{2026}');
        Some(capped)
    } else {
        Some(trimmed.to_owned())
    }
}

fn dialect_str(dialect: HookDialect) -> &'static str {
    match dialect {
        HookDialect::ClaudeCode => "claude-code",
        HookDialect::Codex => "codex",
    }
}

fn decision_str(decision: HookDecision) -> &'static str {
    match decision {
        HookDecision::Approve => "approve",
        HookDecision::Allow => "allow",
        HookDecision::Block => "block",
        HookDecision::Deny => "deny",
        HookDecision::Ask => "ask",
    }
}

/// Appends a log-only hook/invoked event naming the handler and hook point.
///
/// # Errors
///
/// Returns a session append rejection.
pub fn append_hook_invoked(
    session: &Session,
    invocation: &HookInvocation,
) -> Result<(), SessionError> {
    let mut data = Map::new();
    data.insert("turn".to_owned(), json!(invocation.turn));
    data.insert("point".to_owned(), json!(invocation.point.clone()));
    data.insert("dialect".to_owned(), json!(dialect_str(invocation.dialect)));
    data.insert("handlerId".to_owned(), json!(invocation.handler_id.clone()));
    if let Some(matcher) = &invocation.matcher {
        data.insert("matcher".to_owned(), json!(matcher.clone()));
    }
    session
        .append(
            "hook/invoked",
            Value::Object(data),
            AppendOptions::default(),
        )
        .map(|_| ())
}

/// Appends the durable result paired with a prior hook/invoked.
///
/// # Errors
///
/// Returns a session append rejection.
pub fn append_hook_result(
    session: &Session,
    record: &HookResultRecord,
) -> Result<(), SessionError> {
    let decision = record.output.decision.map_or_else(
        || {
            if record.output.continue_ == Some(false) {
                "stop"
            } else {
                "pass"
            }
        },
        decision_str,
    );
    let mut data = Map::new();
    data.insert("turn".to_owned(), json!(record.turn));
    data.insert("point".to_owned(), json!(record.point.clone()));
    data.insert("handlerId".to_owned(), json!(record.handler_id.clone()));
    data.insert("decision".to_owned(), json!(decision));
    if let Some(exit_code) = record.output.exit_code {
        data.insert("exitCode".to_owned(), json!(exit_code));
    }
    if let Some(summary) = summarize_stderr(&record.output.stderr, record.stderr_summary_max_chars)
    {
        data.insert("stderrSummary".to_owned(), json!(summary));
    }
    data.insert("durationMs".to_owned(), json!(record.duration_ms));
    session
        .append("hook/result", Value::Object(data), AppendOptions::default())
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use seekdeep_llm::SessionId;

    use super::*;

    fn session() -> Arc<Session> {
        Session::create(&SessionId::new("s"), None, None).expect("new session")
    }

    fn output() -> HookOutput {
        HookOutput {
            exit_code: Some(0),
            stderr: String::new(),
            stdout: String::new(),
            continue_: None,
            stop_reason: None,
            decision: None,
            reason: None,
            hook_event_name: None,
            additional_context: None,
            system_message: None,
            updated_input: None,
        }
    }

    fn event_data(session: &Session, event_type: &str) -> Value {
        session
            .events()
            .into_iter()
            .find(|event| event.event_type == event_type)
            .expect("event exists")
            .data
    }

    #[test]
    fn invoked_records_turn_point_dialect_handler_and_matcher() {
        let session = session();
        append_hook_invoked(
            &session,
            &HookInvocation {
                turn: 1,
                point: "PreToolUse".to_owned(),
                dialect: HookDialect::ClaudeCode,
                handler_id: "h1".to_owned(),
                matcher: Some("Bash".to_owned()),
            },
        )
        .expect("append");
        let data = event_data(&session, "hook/invoked");
        assert_eq!(data["turn"], json!(1));
        assert_eq!(data["point"], json!("PreToolUse"));
        assert_eq!(data["dialect"], json!("claude-code"));
        assert_eq!(data["handlerId"], json!("h1"));
        assert_eq!(data["matcher"], json!("Bash"));
    }

    #[test]
    fn invoked_omits_matcher_when_absent() {
        let session = session();
        append_hook_invoked(
            &session,
            &HookInvocation {
                turn: 2,
                point: "Stop".to_owned(),
                dialect: HookDialect::Codex,
                handler_id: "h2".to_owned(),
                matcher: None,
            },
        )
        .expect("append");
        let data = event_data(&session, "hook/invoked");
        assert!(data.get("matcher").is_none());
    }

    #[test]
    fn result_derives_decision_exit_code_and_stderr_summary() {
        let session = session();
        let mut blocked = output();
        blocked.exit_code = Some(2);
        blocked.stderr = "blocked".to_owned();
        blocked.decision = Some(HookDecision::Deny);
        append_hook_result(
            &session,
            &HookResultRecord {
                turn: 1,
                point: "PreToolUse".to_owned(),
                handler_id: "h1".to_owned(),
                output: blocked,
                stderr_summary_max_chars: 500,
                duration_ms: 5,
            },
        )
        .expect("append");
        let data = event_data(&session, "hook/result");
        assert_eq!(data["turn"], json!(1));
        assert_eq!(data["point"], json!("PreToolUse"));
        assert_eq!(data["handlerId"], json!("h1"));
        assert_eq!(data["decision"], json!("deny"));
        assert_eq!(data["exitCode"], json!(2));
        assert_eq!(data["stderrSummary"], json!("blocked"));
        assert_eq!(data["durationMs"], json!(5));
    }

    #[test]
    fn result_omits_absent_exit_code_and_stderr() {
        let session = session();
        let mut allowed = output();
        allowed.exit_code = None;
        allowed.decision = Some(HookDecision::Allow);
        append_hook_result(
            &session,
            &HookResultRecord {
                turn: 1,
                point: "Stop".to_owned(),
                handler_id: "h3".to_owned(),
                output: allowed,
                stderr_summary_max_chars: 500,
                duration_ms: 5,
            },
        )
        .expect("append");
        let data = event_data(&session, "hook/result");
        assert!(data.get("exitCode").is_none());
        assert!(data.get("stderrSummary").is_none());
        assert_eq!(data["decision"], json!("allow"));
    }

    #[test]
    fn decision_falls_back_to_stop_then_pass() {
        let session = session();
        let mut halted = output();
        halted.continue_ = Some(false);
        append_hook_result(
            &session,
            &HookResultRecord {
                turn: 1,
                point: "Stop".to_owned(),
                handler_id: "halt".to_owned(),
                output: halted,
                stderr_summary_max_chars: 500,
                duration_ms: 5,
            },
        )
        .expect("append halted");
        append_hook_result(
            &session,
            &HookResultRecord {
                turn: 1,
                point: "Stop".to_owned(),
                handler_id: "noop".to_owned(),
                output: output(),
                stderr_summary_max_chars: 500,
                duration_ms: 5,
            },
        )
        .expect("append noop");
        let mut both = output();
        both.continue_ = Some(false);
        both.decision = Some(HookDecision::Block);
        append_hook_result(
            &session,
            &HookResultRecord {
                turn: 1,
                point: "Stop".to_owned(),
                handler_id: "both".to_owned(),
                output: both,
                stderr_summary_max_chars: 500,
                duration_ms: 5,
            },
        )
        .expect("append both");
        let decisions = session
            .events()
            .into_iter()
            .filter(|event| event.event_type == "hook/result")
            .map(|event| {
                (
                    event.data["handlerId"].as_str().unwrap().to_owned(),
                    event.data["decision"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            decisions,
            vec![
                ("halt".to_owned(), "stop".to_owned()),
                ("noop".to_owned(), "pass".to_owned()),
                ("both".to_owned(), "block".to_owned()),
            ]
        );
    }

    #[test]
    fn stderr_summary_is_trimmed_and_truncated_with_an_ellipsis() {
        let session = session();
        let mut long = output();
        long.exit_code = Some(2);
        long.stderr = format!("  {}  ", "x".repeat(600));
        append_hook_result(
            &session,
            &HookResultRecord {
                turn: 1,
                point: "PreToolUse".to_owned(),
                handler_id: "long".to_owned(),
                output: long,
                stderr_summary_max_chars: 500,
                duration_ms: 5,
            },
        )
        .expect("append");
        let data = event_data(&session, "hook/result");
        assert_eq!(
            data["stderrSummary"],
            json!(format!("{}\u{2026}", "x".repeat(500)))
        );
    }

    #[test]
    fn a_cap_exact_stderr_is_kept_verbatim() {
        let session = session();
        let mut edge = output();
        edge.exit_code = Some(2);
        edge.stderr = "y".repeat(500);
        append_hook_result(
            &session,
            &HookResultRecord {
                turn: 1,
                point: "PreToolUse".to_owned(),
                handler_id: "edge".to_owned(),
                output: edge,
                stderr_summary_max_chars: 500,
                duration_ms: 5,
            },
        )
        .expect("append");
        let data = event_data(&session, "hook/result");
        assert_eq!(data["stderrSummary"], json!("y".repeat(500)));
    }

    #[test]
    fn summarize_stderr_handles_blank_trim_and_cap() {
        assert_eq!(summarize_stderr("", 500), None);
        assert_eq!(summarize_stderr("  \n\t ", 500), None);
        assert_eq!(
            summarize_stderr("  blocked: bad tool  ", 500),
            Some("blocked: bad tool".to_owned())
        );
        assert_eq!(summarize_stderr("abc", 3), Some("abc".to_owned()));
        assert_eq!(
            summarize_stderr("abcdef", 4),
            Some("abcd\u{2026}".to_owned())
        );
        assert_eq!(
            summarize_stderr("x".repeat(600).as_str(), 500),
            Some(format!("{}\u{2026}", "x".repeat(500)))
        );
    }
}
