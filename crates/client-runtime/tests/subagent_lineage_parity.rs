//! Subagent descendant count and boundary parity.

use std::collections::BTreeMap;

use seekdeep_client_runtime::*;
use seekdeep_identity::SessionId;

fn summary(
    id: &str,
    parent: Option<&str>,
    subagent_origin: bool,
    running: bool,
) -> SubagentSessionSummary {
    SubagentSessionSummary {
        id: SessionId::new(id),
        parent_id: parent.map(SessionId::new),
        subagent_origin,
        running,
    }
}

fn index(rows: Vec<SubagentSessionSummary>) -> BTreeMap<SessionId, SubagentDescendantSummary> {
    index_subagent_descendants(&rows.into_iter().map(|row| (row.id.clone(), row)).collect())
}

#[test]
fn nested_descendants_count_under_each_ancestor_with_exact_running_state() {
    let result = index(vec![
        summary("owner", None, false, false),
        summary("child", Some("owner"), true, false),
        summary("grandchild", Some("child"), true, true),
    ]);
    assert_eq!(
        result[&SessionId::new("owner")],
        SubagentDescendantSummary {
            count: 2,
            running_count: 1,
        }
    );
    assert_eq!(
        result[&SessionId::new("child")],
        SubagentDescendantSummary {
            count: 1,
            running_count: 1,
        }
    );
}

#[test]
fn ordinary_forks_stop_propagation_and_cycles_and_orphans_fail_soft() {
    let result = index(vec![
        summary("owner", None, false, false),
        summary("child", Some("owner"), true, true),
        summary("fork", Some("child"), false, false),
        summary("fork-child", Some("fork"), true, true),
        summary("orphan", Some("missing"), true, true),
        summary("cycle-a", Some("cycle-b"), true, false),
        summary("cycle-b", Some("cycle-a"), true, false),
    ]);
    assert_eq!(result[&SessionId::new("owner")].count, 1);
    assert_eq!(result[&SessionId::new("fork")].running_count, 1);
    assert_eq!(result[&SessionId::new("missing")].count, 1);
    assert_eq!(result[&SessionId::new("cycle-a")].count, 2);
    assert_eq!(result[&SessionId::new("cycle-b")].count, 2);
}
