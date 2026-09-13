//! Generated runtime lookup behavior.

use seekdeep_scope::scoped_events::{ScopedSubjectRequirement, scoped_subject_requirement};

#[test]
fn generated_catalog_routes_subject_presence_and_unscoped_events() {
    for event in [
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
    ] {
        assert_eq!(
            scoped_subject_requirement(event),
            Some(ScopedSubjectRequirement::Subject),
            "{event}"
        );
    }
    for event in [
        "session/created",
        "session/disposed",
        "session/event",
        "session/flush",
        "subagent/end",
        "subagent/start",
    ] {
        assert_eq!(
            scoped_subject_requirement(event),
            Some(ScopedSubjectRequirement::Presence),
            "{event}"
        );
    }
    assert_eq!(scoped_subject_requirement("ordinary/event"), None);
}
