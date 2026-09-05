//! Subagent selector, trigger, token, duration, and locale parity.

use seekdeep_client_ui_subagent::{
    AddressedSubagentState, DurationValue, SUBAGENT_LOCALES, SUBAGENT_NS, SubagentActiveTiming,
    SubagentListSummary, SubagentMode, SubagentReadOnlyReason, SubagentTiming, TokenUsage,
    activity_duration, child_labels, format_duration, format_exact_duration, format_tokens,
    index_subagent_descendants, picked_reference, select_read_only_subagent, serialized_reference,
    split_duration, token_total,
};
use seekdeep_identity::SessionId;

fn summary(id: &str, parent: Option<&str>, running: bool, title: &str) -> SubagentListSummary {
    SubagentListSummary {
        id: SessionId::new(id),
        parent_id: parent.map(SessionId::new),
        subagent_origin: true,
        running,
        display_title: title.to_owned(),
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
    assert_eq!(
        child_labels(&rows, &SessionId::new("parent"), "work"),
        ["worker"]
    );
    assert_eq!(
        child_labels(&rows, &SessionId::new("parent"), ""),
        ["worker", "reviewer"]
    );
    assert_eq!(
        index_subagent_descendants(&rows)[&SessionId::new("parent")],
        seekdeep_client_ui_subagent::SubagentDescendantSummary {
            count: 3,
            running_count: 2,
        }
    );
    let lineage = vec![
        summary("child", Some("root"), false, "child"),
        SubagentListSummary {
            subagent_origin: false,
            ..summary("fork", Some("root"), false, "fork")
        },
        summary("fork-child", Some("fork"), true, "fork-child"),
    ];
    let indexed = index_subagent_descendants(&lineage);
    assert_eq!(
        indexed[&SessionId::new("root")],
        seekdeep_client_ui_subagent::SubagentDescendantSummary {
            count: 1,
            running_count: 0,
        }
    );
    assert_eq!(
        indexed[&SessionId::new("fork")],
        seekdeep_client_ui_subagent::SubagentDescendantSummary {
            count: 1,
            running_count: 1,
        }
    );
    assert_eq!(picked_reference("worker"), "@worker ");
    assert_eq!(serialized_reference("worker"), "@worker");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn portable_descendant_index_matches_the_runtime_oracle() {
    let rows = vec![
        summary("child", Some("root"), true, "child"),
        summary("grandchild", Some("child"), false, "grandchild"),
        SubagentListSummary {
            subagent_origin: false,
            ..summary("fork", Some("root"), false, "fork")
        },
        summary("fork-child", Some("fork"), true, "fork-child"),
    ];
    let runtime_rows = rows
        .iter()
        .map(|row| {
            (
                row.id.clone(),
                seekdeep_client_runtime::SubagentSessionSummary {
                    id: row.id.clone(),
                    parent_id: row.parent_id.clone(),
                    subagent_origin: row.subagent_origin,
                    running: row.running,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let expected = seekdeep_client_runtime::index_subagent_descendants(&runtime_rows)
        .into_iter()
        .map(|(id, value)| (id, (value.count, value.running_count)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let actual = index_subagent_descendants(&rows)
        .into_iter()
        .map(|(id, value)| (id, (value.count, value.running_count)))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(actual, expected);
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
        (123_000_000, "123M"),
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
            ("hours", DurationValue::Number(3)),
            ("minutes", DurationValue::Text("04".to_owned())),
            ("seconds", DurationValue::Text("05".to_owned()))
        ]
    );
}

#[test]
fn large_duration_models_preserve_decreasing_precision_and_value_types() {
    assert_eq!(
        format_duration(2 * 86_400_000),
        seekdeep_client_ui_subagent::DurationFormat {
            key: "days",
            values: vec![("days", DurationValue::Number(2))],
        }
    );
    assert_eq!(
        format_duration(2 * 86_400_000 + 3 * 3_600_000),
        seekdeep_client_ui_subagent::DurationFormat {
            key: "daysHours",
            values: vec![
                ("days", DurationValue::Number(2)),
                ("hours", DurationValue::Number(3)),
            ],
        }
    );
    assert_eq!(
        format_duration(192 * 86_400_000),
        seekdeep_client_ui_subagent::DurationFormat {
            key: "monthsDays",
            values: vec![
                ("months", DurationValue::Number(6)),
                ("days", DurationValue::Number(12)),
            ],
        }
    );
    assert_eq!(
        format_duration(30 * 86_400_000),
        seekdeep_client_ui_subagent::DurationFormat {
            key: "months",
            values: vec![("months", DurationValue::Number(1))],
        }
    );
    assert_eq!(
        format_duration(832 * 86_400_000),
        seekdeep_client_ui_subagent::DurationFormat {
            key: "yearsMonths",
            values: vec![
                ("years", DurationValue::Number(2)),
                ("months", DurationValue::Number(3)),
            ],
        }
    );
    assert_eq!(
        format_duration(365 * 86_400_000),
        seekdeep_client_ui_subagent::DurationFormat {
            key: "years",
            values: vec![("years", DurationValue::Number(1))],
        }
    );
    assert_eq!(
        format_exact_duration(2 * 86_400_000 + 3_661_000),
        seekdeep_client_ui_subagent::DurationFormat {
            key: "exactDays",
            values: vec![
                ("days", DurationValue::Number(2)),
                ("hours", DurationValue::Text("01".to_owned())),
                ("minutes", DurationValue::Text("01".to_owned())),
                ("seconds", DurationValue::Text("01".to_owned())),
            ],
        }
    );
}

#[test]
fn active_duration_samples_running_and_inactive_edges_exactly() {
    let active = SubagentTiming {
        settled_ms: 65_000,
        active: Some(SubagentActiveTiming {
            since: 1_995_000,
            through: 1_999_000,
        }),
    };
    assert_eq!(
        activity_duration(Some(active), true, 2_000_000),
        Some(70_000)
    );
    assert_eq!(
        activity_duration(Some(active), false, 2_100_000),
        Some(69_000)
    );
    assert_eq!(
        activity_duration(
            Some(SubagentTiming {
                settled_ms: 2_000,
                active: Some(SubagentActiveTiming {
                    since: 7_000,
                    through: 3_000,
                }),
            }),
            false,
            10_000,
        ),
        Some(2_000)
    );
    assert_eq!(activity_duration(None, true, 0), None);
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
