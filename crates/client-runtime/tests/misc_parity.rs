//! Time-zone, ordered-baseline, context-source, and Session-lineage parity.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use seekdeep_client_runtime::*;
use seekdeep_identity::SessionId;
use serde_json::{Value, json};

struct Zone(Option<&'static str>);

impl ClientTimeZoneResolver for Zone {
    fn resolve(&self) -> Option<String> {
        self.0.map(str::to_owned)
    }
}

#[test]
fn time_zone_returns_runtime_value_and_fails_on_missing_or_empty() {
    assert_eq!(
        resolved_client_time_zone(&Zone(Some("America/New_York"))).unwrap(),
        "America/New_York"
    );
    assert_eq!(
        resolved_client_time_zone(&Zone(None))
            .unwrap_err()
            .to_string(),
        "browser time zone is unavailable"
    );
    assert!(resolved_client_time_zone(&Zone(Some(""))).is_err());
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    id: &'static str,
    value: i64,
}

#[test]
fn ordered_baseline_keeps_known_order_updates_values_inserts_relative_and_removes_absent() {
    let current = [
        Row { id: "b", value: 1 },
        Row { id: "d", value: 1 },
        Row {
            id: "gone",
            value: 1,
        },
    ];
    let baseline = [
        Row { id: "a", value: 2 },
        Row { id: "b", value: 2 },
        Row { id: "c", value: 2 },
        Row { id: "d", value: 2 },
        Row {
            id: "tail",
            value: 2,
        },
    ];
    let merged = merge_ordered_baseline(&current, &baseline, |row| row.id);
    assert_eq!(
        merged
            .iter()
            .map(|row| (row.id, row.value))
            .collect::<Vec<_>>(),
        [("a", 2), ("b", 2), ("c", 2), ("d", 2), ("tail", 2)]
    );
}

#[test]
fn context_provenance_names_known_unknown_and_unreadable_durable_sources() {
    assert_eq!(
        context_provenance(&json!({"kind":"plugin","plugin":"seekdeep-tool-skill"})),
        ContextProvenanceView {
            role: ContextRole::Inject,
            label: Some("seekdeep-tool-skill".to_owned()),
        }
    );
    assert_eq!(
        context_provenance(&json!({
            "kind":"agent-instructions",
            "changes":[
                {"path":"AGENTS.md"}, {"path":"sub/AGENTS.md"}, {"path":"AGENTS.md"}
            ]
        }))
        .label
        .as_deref(),
        Some("AGENTS.md, sub/AGENTS.md")
    );
    assert_eq!(
        context_provenance(&json!({
            "kind":"session-reference",
            "references":[{"label":"Refactor the loader"},{"label":"Fix CI"}]
        })),
        ContextProvenanceView {
            role: ContextRole::Recall,
            label: Some("Refactor the loader, Fix CI".to_owned()),
        }
    );
    assert_eq!(
        context_provenance(&json!({"kind":"subagent-report"}))
            .label
            .as_deref(),
        Some("subagent-report")
    );
    for source in [
        Value::Null,
        json!("plugin"),
        json!([{"kind":"plugin"}]),
        json!({"kind":42}),
    ] {
        assert_eq!(context_provenance(&source).label, None);
    }
    for source in [
        json!({"kind":"plugin"}),
        json!({"kind":"plugin","plugin":""}),
        json!({"kind":"plugin","plugin":7}),
    ] {
        assert_eq!(context_provenance(&source).label.as_deref(), Some("plugin"));
    }
}

#[test]
fn context_form_accepts_six_known_values_and_degrades_future_or_malformed_values() {
    for (name, expected) in [
        ("instructions", KnownContextForm::Instructions),
        ("catalog", KnownContextForm::Catalog),
        ("snapshot", KnownContextForm::Snapshot),
        ("notice", KnownContextForm::Notice),
        ("relay", KnownContextForm::Relay),
        ("recall", KnownContextForm::Recall),
    ] {
        assert_eq!(context_form(&json!({"form":name})), Some(expected));
    }
    for source in [
        json!({}),
        json!({"form":"future"}),
        json!({"form":""}),
        json!({"form":7}),
        Value::Null,
    ] {
        assert_eq!(context_form(&source), None);
    }
}

fn summary(id: &str, updated_at: i64, parent: Option<&str>) -> TitledSessionSummary {
    TitledSessionSummary {
        session_id: SessionId::new(id),
        title: None,
        updated_at,
        running: false,
        blank: false,
        parent_session_id: parent.map(SessionId::new),
        origin: None,
        cwd: None,
        agent_preset: None,
        projection_values: None,
    }
}

#[test]
fn lineage_keeps_root_sibling_order_and_expands_children_depth_first() {
    let silent: LineageLogger = Rc::new(|_| {});
    let rows = flatten_lineage(
        &[
            summary("old-root", 10, None),
            summary("new-root", 30, None),
            summary("kid-old", 11, Some("new-root")),
            summary("kid-new", 12, Some("new-root")),
            summary("grandkid", 5, Some("kid-new")),
        ],
        &BTreeMap::new(),
        &BTreeSet::new(),
        &silent,
    );
    assert_eq!(
        rows.iter()
            .map(|row| (row.summary.session_id.as_str(), row.depth))
            .collect::<Vec<_>>(),
        [
            ("old-root", 0),
            ("new-root", 0),
            ("kid-old", 1),
            ("kid-new", 1),
            ("grandkid", 2)
        ]
    );
}

#[test]
fn lineage_degrades_orphans_and_cycles_without_dropping_rows() {
    let warnings = Rc::new(RefCell::new(Vec::new()));
    let observed = warnings.clone();
    let warn: LineageLogger = Rc::new(move |id| observed.borrow_mut().push(id.clone()));
    let rows = flatten_lineage(
        &[
            summary("orphan", 20, Some("ghost")),
            summary("a", 20, Some("b")),
            summary("b", 10, Some("a")),
            summary("self", 5, Some("self")),
            summary("root", 30, None),
        ],
        &BTreeMap::new(),
        &BTreeSet::new(),
        &warn,
    );
    let ids = rows
        .iter()
        .map(|row| row.summary.session_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ids,
        ["a", "b", "orphan", "root", "self"].into_iter().collect()
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.summary.session_id.as_str() == "orphan")
            .unwrap()
            .depth,
        0
    );
    assert!(!warnings.borrow().is_empty());
}

#[test]
fn lineage_projects_pending_interactions_and_completion_reminders() {
    let pending = [(SessionId::new("a"), json!({"kind":"approval"}))]
        .into_iter()
        .collect();
    let completed = [SessionId::new("b")].into_iter().collect();
    let silent: LineageLogger = Rc::new(|_| {});
    let rows = flatten_lineage(
        &[summary("a", 10, None), summary("b", 20, None)],
        &pending,
        &completed,
        &silent,
    );
    assert_eq!(
        rows[0].pending_interaction,
        Some(json!({"kind":"approval"}))
    );
    assert!(!rows[0].completed);
    assert!(rows[1].completed);
}
