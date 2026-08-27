//! Subagent selector, trigger, token, duration, and locale parity.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{RuntimeSessionListState, RuntimeSessionSummary, SessionListPhase};
use seekdeep_client_ui_subagent::{
    AddressedSubagentState, SUBAGENT_LOCALES, SUBAGENT_NS, SubagentMode, SubagentReadOnlyReason,
    TokenUsage, child_labels, format_duration, format_exact_duration, format_tokens,
    picked_reference, select_read_only_subagent, serialized_reference, split_duration, token_total,
};
use seekdeep_identity::SessionId;

fn summary(id: &str, parent: Option<&str>, running: bool, title: &str) -> RuntimeSessionSummary {
    RuntimeSessionSummary {
        id: SessionId::new(id),
        title: Some(title.to_owned()),
        display_title: title.to_owned(),
        cwd: None,
        agent_preset: None,
        parent_id: parent.map(SessionId::new),
        origin: Some("subagent".to_owned()),
        running,
        pending_interaction: None,
        completed: false,
        blank: false,
        updated_at: 0,
        projection_values: None,
    }
}

#[test]
fn selector_and_running_child_reference_vocabulary_match_the_source() {
    assert_eq!(select_read_only_subagent(None), None);
    assert_eq!(
        select_read_only_subagent(Some(AddressedSubagentState {
            mode: SubagentMode::OneShot,
            parent_available: true,
            running: true,
        })),
        Some(SubagentReadOnlyReason::OneShot)
    );
    assert_eq!(
        select_read_only_subagent(Some(AddressedSubagentState {
            mode: SubagentMode::Continuable,
            parent_available: true,
            running: false,
        })),
        None
    );
    assert_eq!(
        select_read_only_subagent(Some(AddressedSubagentState {
            mode: SubagentMode::Continuable,
            parent_available: false,
            running: true,
        })),
        None
    );
    assert_eq!(
        select_read_only_subagent(Some(AddressedSubagentState {
            mode: SubagentMode::Continuable,
            parent_available: false,
            running: false,
        })),
        Some(SubagentReadOnlyReason::ParentUnavailable)
    );

    let rows = vec![
        summary("one", Some("parent"), true, "worker"),
        summary("two", Some("parent"), true, "reviewer"),
        summary("stopped", Some("parent"), false, "stopped"),
        summary("foreign", Some("other"), true, "worker-foreign"),
    ];
    let list = RuntimeSessionListState {
        ids: Rc::new(rows.iter().map(|row| row.id.clone()).collect()),
        by_id: Rc::new(
            rows.into_iter()
                .map(|row| (row.id.clone(), Rc::new(row)))
                .collect(),
        ),
        current: None,
        phase: SessionListPhase::Ready,
        subagents_by_parent: Rc::new(IndexMap::new()),
        jobs_by_session: Rc::new(IndexMap::new()),
        current_address: None,
    };
    assert_eq!(
        child_labels(&list, &SessionId::new("parent"), "work"),
        ["worker"]
    );
    assert_eq!(
        child_labels(&list, &SessionId::new("parent"), ""),
        ["worker", "reviewer"]
    );
    assert_eq!(picked_reference("worker"), "@worker ");
    assert_eq!(serialized_reference("worker"), "@worker");
}

#[test]
fn token_and_duration_models_preserve_every_precision_bucket() {
    for (value, expected) in [
        (999, "999"),
        (1_000, "1K"),
        (1_250, "1.3K"),
        (99_940, "99.9K"),
        (100_400, "100K"),
        (1_250_000, "1.3M"),
    ] {
        assert_eq!(format_tokens(value), expected);
    }
    assert_eq!(token_total(None), None);
    assert_eq!(
        token_total(Some(TokenUsage {
            uncached_input_tokens: 1,
            output_tokens: 2,
            cache_read_tokens: 3,
            cache_write_tokens: 4,
        })),
        Some(10)
    );
    assert_eq!(split_duration(-1).seconds, 0);
    assert_eq!(format_duration(42_000).key, "seconds");
    assert_eq!(format_duration(5 * 60_000 + 7_000).key, "minutes");
    assert_eq!(
        format_duration(3 * 3_600_000 + 4 * 60_000 + 5_000).values,
        [
            ("hours", "3".to_owned()),
            ("minutes", "04".to_owned()),
            ("seconds", "05".to_owned())
        ]
    );
    assert_eq!(format_duration(2 * 86_400_000).key, "days");
    assert_eq!(
        format_duration(2 * 86_400_000 + 3 * 3_600_000).key,
        "daysHours"
    );
    assert_eq!(format_duration(60 * 86_400_000).key, "months");
    assert_eq!(format_duration(61 * 86_400_000).key, "monthsDays");
    assert_eq!(format_duration(365 * 86_400_000).key, "years");
    assert_eq!(format_duration(395 * 86_400_000).key, "yearsMonths");
    assert_eq!(
        format_exact_duration(2 * 86_400_000 + 3_661_000).key,
        "exactDays"
    );
}

#[test]
fn locale_namespace_and_all_copy_are_pinned() {
    assert_eq!(SUBAGENT_NS, "subagent");
    assert_eq!(SUBAGENT_LOCALES.len(), 33);
    assert_eq!(
        SUBAGENT_LOCALES[1],
        (
            "diagnostic.unsupported",
            "子代理记录版本不受支持",
            "unsupported subagent record version"
        )
    );
    assert_eq!(
        SUBAGENT_LOCALES[32],
        (
            "readonly.body",
            "父会话当前不在线，重新打开父会话后即可继续发送消息。",
            "The parent session is offline; reopen it to continue sending messages."
        )
    );
}
