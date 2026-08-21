//! Generated scope-filtered event subject requirements.

/// How a scope-filtered event exposes its routing subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopedSubjectRequirement {
    /// A carrier is required, but the payload has no external subject.
    Presence,
    /// The payload-derived subject must equal the carrier key.
    Subject,
}

/// Returns the generated requirement for one event, or `None` when unscoped.
#[must_use]
pub fn scoped_subject_requirement(event: &str) -> Option<ScopedSubjectRequirement> {
    use ScopedSubjectRequirement::{Presence, Subject};

    Some(match event {
        "agent/created"
        | "agent/disposed"
        | "agent/error"
        | "agent/inbox/claimed"
        | "agent/inbox/discarded"
        | "agent/inbox/inserted"
        | "agent/pre-step"
        | "agent/request"
        | "agent/request-error"
        | "agent/session-start"
        | "agent/status"
        | "agent/turn-stopping"
        | "approval/request"
        | "goal/changed"
        | "system-prompt/assemble"
        | "tools/code-dispatch-log"
        | "tools/execute"
        | "tools/post-execute"
        | "tools/pre-execute"
        | "tools/result" => Subject,
        "session/created" | "session/disposed" | "session/event" | "session/flush"
        | "subagent/end" | "subagent/start" => Presence,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_catalog_has_twenty_subject_and_six_presence_events() {
        let subject = [
            "agent/created",
            "agent/disposed",
            "agent/error",
            "agent/inbox/claimed",
            "agent/inbox/discarded",
            "agent/inbox/inserted",
            "agent/pre-step",
            "agent/request",
            "agent/request-error",
            "agent/session-start",
            "agent/status",
            "agent/turn-stopping",
            "approval/request",
            "goal/changed",
            "system-prompt/assemble",
            "tools/code-dispatch-log",
            "tools/execute",
            "tools/post-execute",
            "tools/pre-execute",
            "tools/result",
        ];
        let presence = [
            "session/created",
            "session/disposed",
            "session/event",
            "session/flush",
            "subagent/end",
            "subagent/start",
        ];
        assert!(subject.iter().all(|event| {
            scoped_subject_requirement(event) == Some(ScopedSubjectRequirement::Subject)
        }));
        assert!(presence.iter().all(|event| {
            scoped_subject_requirement(event) == Some(ScopedSubjectRequirement::Presence)
        }));
        assert_eq!(scoped_subject_requirement("ordinary/event"), None);
    }
}
