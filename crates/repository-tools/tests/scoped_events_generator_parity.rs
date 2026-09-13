//! Pinned source catalog transformation plus write/check freshness parity.

use seekdeep_repository_tools::scoped_events_generator::{
    OUTPUT_PATH, PRESENCE_EVENTS, SUBJECT_EVENTS, render_scoped_events, run_scoped_events_generator,
};

#[test]
fn pinned_source_resolvers_transform_to_twenty_subject_and_six_presence_events() {
    assert_eq!(SUBJECT_EVENTS.len(), 20);
    assert_eq!(PRESENCE_EVENTS.len(), 6);
    assert_eq!(
        SUBJECT_EVENTS,
        [
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
        ]
    );
    assert_eq!(
        PRESENCE_EVENTS,
        [
            "session/created",
            "session/disposed",
            "session/event",
            "session/flush",
            "subagent/end",
            "subagent/start",
        ]
    );
}

#[test]
fn rendered_catalog_is_the_committed_runtime_source() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert_eq!(
        std::fs::read_to_string(root.join(OUTPUT_PATH)).unwrap(),
        render_scoped_events()
    );
}

#[test]
fn write_check_and_stale_diagnostics_are_source_compatible() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("crates/scope/src")).unwrap();
    assert_eq!(
        run_scoped_events_generator(root.path(), false).unwrap(),
        format!("gen-scoped-events: wrote {OUTPUT_PATH}.\n")
    );
    assert_eq!(
        run_scoped_events_generator(root.path(), true).unwrap(),
        format!("gen-scoped-events: {OUTPUT_PATH} is up to date.\n")
    );
    std::fs::write(root.path().join(OUTPUT_PATH), "stale\n").unwrap();
    let error = run_scoped_events_generator(root.path(), true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("is stale"));
    assert!(error.contains("pnpm run gen-scoped-events"));
}
