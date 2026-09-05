//! Workspace grouping, flat list, search, store, label, and time parity.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    ClientWorkspaceView, RuntimeSessionListState, RuntimeSessionSummary, SessionListPhase,
    StoreEnvironment, StoreFlushScheduler,
};
use seekdeep_client_ui_workspace::{
    FLAT_SESSION_ORDER_KEY, RelativeTime, RelativeTimeUnit, SearchResultSet, SessionGroupBy,
    SessionOrderBy, SessionSearchPage, SessionSearchResultItem, TreeView, UNGROUPED_KEY,
    UNGROUPED_LABEL, WORKSPACE_LOCALES, WORKSPACE_NS, WORKSPACE_VIEW_PERSIST_KEY,
    WorkspaceViewState, create_workspace_view_store, derive_flat, derive_groups,
    derive_search_results, relative_time, workspace_label,
};
use seekdeep_identity::{SessionId, WorkspaceId};
use serde_json::json;

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

fn summary(id: &str, updated_at: i64, cwd: Option<&str>) -> RuntimeSessionSummary {
    RuntimeSessionSummary {
        id: sid(id),
        title: Some(id.to_owned()),
        display_title: id.to_owned(),
        cwd: cwd.map(ToOwned::to_owned),
        agent_preset: None,
        parent_id: None,
        origin: None,
        running: false,
        pending_interaction: None,
        completed: false,
        blank: false,
        updated_at,
        projection_values: None,
    }
}

fn list(rows: Vec<RuntimeSessionSummary>) -> RuntimeSessionListState {
    RuntimeSessionListState {
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
    }
}

fn workspace(id: &str, session_ids: &[&str], title: Option<&str>) -> Rc<ClientWorkspaceView> {
    Rc::new(ClientWorkspaceView {
        workspace_id: WorkspaceId::new(id),
        path: format!("/projects/{id}"),
        title: title.unwrap_or(id).to_owned(),
        session_ids: session_ids.iter().map(|id| sid(id)).collect(),
        created_at: "2026-01-01T00:00:00.000Z".to_owned(),
        updated_at: "2026-01-01T00:00:00.000Z".to_owned(),
    })
}

fn view(expanded: &[&str], ungrouped: Option<&[&str]>) -> TreeView {
    TreeView {
        expanded_groups: expanded.iter().map(|key| (*key).to_owned()).collect(),
        ungrouped_order: ungrouped.map(|keys| keys.iter().map(|key| (*key).to_owned()).collect()),
    }
}

struct ImmediateScheduler;

impl StoreFlushScheduler for ImmediateScheduler {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        callback();
    }
}

#[test]
fn groups_preserve_accounts_apply_ungrouped_order_and_filter_blank_archived_rows() {
    let older = summary("older", 10, None);
    let newer = summary("newer", 20, None);
    let current_blank = RuntimeSessionSummary {
        blank: true,
        ..summary("current-blank", 5, None)
    };
    let stale_blank = RuntimeSessionSummary {
        blank: true,
        ..summary("stale-blank", 4, None)
    };
    let loose_one = summary("loose-one", 3, None);
    let loose_two = summary("loose-two", 2, None);
    let loose_new = summary("loose-new", 4, None);
    let archived = summary("archived", 30, None);
    let mut sessions = list(vec![
        older,
        newer,
        current_blank,
        stale_blank,
        loose_one,
        loose_two,
        loose_new,
        archived,
    ]);
    sessions.current = Some(sid("current-blank"));
    let groups = derive_groups(
        &sessions,
        &[
            workspace(
                "first",
                &["older", "newer", "current-blank", "stale-blank", "archived"],
                None,
            ),
            workspace("empty", &[], None),
        ],
        &[sid("archived")],
        &view(
            &["first", UNGROUPED_KEY],
            Some(&["loose-two", "stale", "loose-two"]),
        ),
    );
    assert_eq!(
        groups
            .iter()
            .map(|group| group.key.as_str())
            .collect::<Vec<_>>(),
        ["first", "empty", UNGROUPED_KEY]
    );
    assert_eq!(
        groups[0]
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        ["older", "newer", "current-blank"]
    );
    assert_eq!(groups[0].sessions[2].title, "New Session");
    assert!(groups[0].sessions[2].blank);
    assert_eq!(groups[0].session_count, 3);
    assert!(groups[0].contains_current);
    assert_eq!(groups[0].created_at, Some(1_767_225_600_000.0));
    assert_eq!(
        groups[2]
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        ["loose-two", "loose-new", "loose-one"]
    );
    assert!(!groups[2].contains_current);

    let mut loose_current = list(vec![summary("owned", 1, None), summary("loose", 2, None)]);
    loose_current.current = Some(sid("loose"));
    let loose_groups = derive_groups(
        &loose_current,
        &[workspace("project", &["owned"], None)],
        &[],
        &view(&[], None),
    );
    assert!(
        loose_groups
            .iter()
            .find(|group| group.key == UNGROUPED_KEY)
            .unwrap()
            .contains_current
    );
}

#[test]
fn grouping_and_flat_projection_hide_subagents_but_count_uninterrupted_descendants() {
    let parent = summary("parent", 1, None);
    let subagent = RuntimeSessionSummary {
        parent_id: Some(parent.id.clone()),
        origin: Some("subagent".to_owned()),
        running: true,
        ..summary("subagent", 3, None)
    };
    let grandchild = RuntimeSessionSummary {
        parent_id: Some(subagent.id.clone()),
        origin: Some("subagent".to_owned()),
        running: true,
        ..summary("grandchild", 4, None)
    };
    let fork = RuntimeSessionSummary {
        parent_id: Some(subagent.id.clone()),
        ..summary("fork", 2, None)
    };
    let fork_child = RuntimeSessionSummary {
        parent_id: Some(fork.id.clone()),
        origin: Some("subagent".to_owned()),
        running: true,
        ..summary("fork-child", 5, None)
    };
    let tie_b = RuntimeSessionSummary {
        parent_id: Some(parent.id.clone()),
        ..summary("tie-b", 20, None)
    };
    let tie_a = RuntimeSessionSummary {
        parent_id: Some(parent.id.clone()),
        ..summary("tie-a", 20, None)
    };
    let sessions = list(vec![
        parent, fork, subagent, grandchild, fork_child, tie_b, tie_a,
    ]);
    let groups = derive_groups(
        &sessions,
        &[workspace(
            "first",
            &[
                "parent",
                "fork",
                "subagent",
                "grandchild",
                "fork-child",
                "tie-b",
                "tie-a",
            ],
            None,
        )],
        &[],
        &view(&["first"], None),
    );
    assert_eq!(
        groups[0]
            .sessions
            .iter()
            .map(|node| (node.id.as_str(), node.running_subagent_count))
            .collect::<Vec<_>>(),
        [("parent", 2), ("fork", 1), ("tie-b", 0), ("tie-a", 0)]
    );
    assert_eq!(
        derive_flat(&sessions, &[])
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        ["tie-a", "tie-b", "fork", "parent"]
    );
}

#[test]
fn rows_project_pending_and_completion_and_tolerate_missing_summaries() {
    let awaiting = RuntimeSessionSummary {
        pending_interaction: Some(json!("plan-review")),
        running: true,
        completed: true,
        ..summary("awaiting", 10, None)
    };
    let present = summary("present", 1, None);
    let mut sessions = list(vec![awaiting, present]);
    Rc::make_mut(&mut sessions.ids).insert(0, sid("ghost"));
    let grouped = derive_groups(
        &sessions,
        &[workspace(
            "project",
            &["missing", "awaiting", "present"],
            None,
        )],
        &[],
        &view(&["project"], None),
    );
    assert_eq!(grouped[0].sessions.len(), 2);
    assert_eq!(
        grouped[0].sessions[0].pending_interaction,
        Some(json!("plan-review"))
    );
    assert!(grouped[0].sessions[0].running);
    assert!(grouped[0].sessions[0].completed);
    assert_eq!(derive_flat(&sessions, &[]).len(), 2);

    let current_blank = RuntimeSessionSummary {
        blank: true,
        ..summary("current-blank", 9, None)
    };
    let stale_blank = RuntimeSessionSummary {
        blank: true,
        ..summary("stale-blank", 8, None)
    };
    let archived = summary("archived", 7, None);
    let mut visibility = list(vec![
        summary("real", 1, None),
        current_blank,
        stale_blank,
        archived,
    ]);
    visibility.current = Some(sid("current-blank"));
    assert_eq!(
        derive_flat(&visibility, &[sid("archived")])
            .iter()
            .map(|row| (row.id.as_str(), row.title.as_str()))
            .collect::<Vec<_>>(),
        [("current-blank", "New Session"), ("real", "real")]
    );
}

#[test]
fn search_merges_local_hits_before_ranked_content_and_enriches_duplicates() {
    let mut title_hit = summary("title-hit", 30, Some("/projects/a"));
    title_hit.display_title = "Needle title".to_owned();
    title_hit.pending_interaction = Some(json!("plan-review"));
    title_hit.completed = true;
    let mut workspace_hit = summary("workspace-hit", 20, Some("/projects/b"));
    workspace_hit.display_title = "Ordinary title".to_owned();
    let content_hit = summary("content-hit", 10, Some("/projects/c"));
    let archived = RuntimeSessionSummary {
        display_title: "Needle archived".to_owned(),
        ..summary("archived", 40, None)
    };
    let blank = RuntimeSessionSummary {
        blank: true,
        display_title: "Needle blank".to_owned(),
        ..summary("blank", 50, None)
    };
    let sessions = list(vec![title_hit, workspace_hit, content_hit, archived, blank]);
    let result = derive_search_results(
        &sessions,
        &[
            workspace("a", &["title-hit"], Some("Alpha")),
            workspace("b", &["workspace-hit"], Some("Needle Workspace")),
            workspace("duplicate", &["title-hit"], Some("Ignored duplicate owner")),
        ],
        " \u{feff}NEEDLE ",
        &[sid("archived")],
        &SessionSearchPage {
            items: vec![
                SessionSearchResultItem {
                    session_id: sid("content-hit"),
                    snippet: "body needle excerpt".to_owned(),
                },
                SessionSearchResultItem {
                    session_id: sid("content-hit"),
                    snippet: "ignored duplicate".to_owned(),
                },
                SessionSearchResultItem {
                    session_id: sid("title-hit"),
                    snippet: "title body".to_owned(),
                },
                SessionSearchResultItem {
                    session_id: sid("blank"),
                    snippet: "blank body".to_owned(),
                },
                SessionSearchResultItem {
                    session_id: sid("unknown"),
                    snippet: "unknown body".to_owned(),
                },
            ],
            has_more: false,
        },
        10,
    );
    assert_eq!(
        result
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["title-hit", "workspace-hit", "content-hit"]
    );
    assert_eq!(result.items[0].workspace, "Alpha");
    assert_eq!(result.items[0].snippet.as_deref(), Some("title body"));
    assert_eq!(
        result.items[0].pending_interaction,
        Some(json!("plan-review"))
    );
    assert!(result.items[0].completed);
    assert_eq!(result.items[2].workspace, "c");
    assert_eq!(
        result.items[2].snippet.as_deref(),
        Some("body needle excerpt")
    );
    assert!(!result.has_more);
}

#[test]
fn search_cap_blank_query_and_backend_more_semantics_are_exact() {
    let rows = (0..5)
        .map(|index| RuntimeSessionSummary {
            display_title: format!("Needle {index}"),
            ..summary(&format!("s-{index:02}"), index, None)
        })
        .collect::<Vec<_>>();
    let sessions = list(rows);
    let overflow = derive_search_results(
        &sessions,
        &[],
        "needle",
        &[],
        &SessionSearchPage::default(),
        3,
    );
    assert_eq!(overflow.items.len(), 3);
    assert!(overflow.has_more);
    let backend_more = derive_search_results(
        &sessions,
        &[],
        "not-local",
        &[],
        &SessionSearchPage {
            items: vec![SessionSearchResultItem {
                session_id: sid("s-00"),
                snippet: "not-local body".to_owned(),
            }],
            has_more: true,
        },
        3,
    );
    assert_eq!(backend_more.items.len(), 1);
    assert!(backend_more.has_more);
    assert_eq!(
        derive_search_results(
            &sessions,
            &[],
            " \u{feff} ",
            &[],
            &SessionSearchPage {
                items: Vec::new(),
                has_more: true,
            },
            3,
        ),
        SearchResultSet::default()
    );
}

#[test]
fn store_and_locale_contracts_match_the_source() {
    assert_eq!(WORKSPACE_NS, "workspace");
    assert_eq!(WORKSPACE_LOCALES.len(), 62);
    assert_eq!(
        WORKSPACE_LOCALES[6],
        ("groupBy.workspace", "按工作区", "WorkSpace")
    );
    assert_eq!(WORKSPACE_LOCALES[61], ("time.ago", "{t}前", "{t} ago"));
    assert_eq!(FLAT_SESSION_ORDER_KEY, "__flat_session_order__");
    assert_eq!(WORKSPACE_VIEW_PERSIST_KEY, "dsh.workspace.view.v5");
    let mut state = WorkspaceViewState::default();
    assert_eq!(state.group_by, SessionGroupBy::Workspace);
    assert_eq!(state.order_by, SessionOrderBy::Updated);
    state.set_group_by(SessionGroupBy::Flat);
    state.set_order_by(SessionOrderBy::Manual);
    state.set_group_expanded("", true);
    state.set_group_expanded("alpha", true);
    state.set_group_expanded("deleted", true);
    state.sync_session_order_account(
        "alpha",
        vec!["two".to_owned(), "one".to_owned()],
        IndexMap::from([("one".to_owned(), 1), ("two".to_owned(), 2)]),
    );
    state.sync_session_order_account(
        "deleted",
        vec!["gone".to_owned()],
        IndexMap::from([("gone".to_owned(), 3)]),
    );
    state.set_session_order("alpha", vec!["one".to_owned(), "two".to_owned()]);
    state.retain_account_keys(&[String::new(), "alpha".to_owned()]);
    assert_eq!(state.group_by, SessionGroupBy::Flat);
    assert_eq!(state.order_by, SessionOrderBy::Manual);
    assert_eq!(
        state
            .group_expansion
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["", "alpha"]
    );
    assert_eq!(state.session_order_by_account["alpha"], ["one", "two"]);
    assert!(!state.session_order_by_account.contains_key("deleted"));

    let handle = create_workspace_view_store(StoreEnvironment {
        scheduler: Rc::new(ImmediateScheduler),
        persistence: None,
        logger: Rc::new(|_| {}),
    });
    let instance = handle.create_typed(None);
    instance.invoke("setGroupBy", &[json!("flat")]).unwrap();
    instance
        .invoke("setGroupExpanded", &[json!("alpha"), json!(true)])
        .unwrap();
    instance
        .invoke(
            "syncSessionOrderAccount",
            &[
                json!("alpha"),
                json!(["two", "one"]),
                json!({"one":1,"two":2}),
            ],
        )
        .unwrap();
    instance
        .invoke("setSessionOrder", &[json!("alpha"), json!(["one", "two"])])
        .unwrap();
    let snapshot = instance.store.snapshot();
    assert_eq!(snapshot.group_by, SessionGroupBy::Flat);
    assert!(snapshot.group_expansion["alpha"]);
    assert_eq!(snapshot.session_order_by_account["alpha"], ["one", "two"]);
    assert!(instance.invoke("setOrderBy", &[json!("future")]).is_err());
}

#[test]
fn workspace_labels_and_relative_time_boundaries_match_the_source() {
    assert_eq!(workspace_label(None), UNGROUPED_LABEL);
    assert_eq!(workspace_label(Some("")), UNGROUPED_LABEL);
    assert_eq!(workspace_label(Some("/projects/demo/")), "demo");
    assert_eq!(workspace_label(Some(r"C:\projects\demo\")), "demo");
    assert_eq!(workspace_label(Some("/")), "/");

    let now = 400_i64 * 24 * 60 * 60 * 1_000;
    for (updated_at, expected) in [
        (
            now,
            RelativeTime {
                unit: RelativeTimeUnit::Now,
                n: 0,
            },
        ),
        (
            now - 5 * 60_000,
            RelativeTime {
                unit: RelativeTimeUnit::Minutes,
                n: 5,
            },
        ),
        (
            now - 3 * 3_600_000,
            RelativeTime {
                unit: RelativeTimeUnit::Hours,
                n: 3,
            },
        ),
        (
            now - 2 * 86_400_000,
            RelativeTime {
                unit: RelativeTimeUnit::Days,
                n: 2,
            },
        ),
        (
            now - 60 * 86_400_000,
            RelativeTime {
                unit: RelativeTimeUnit::Months,
                n: 2,
            },
        ),
        (
            0,
            RelativeTime {
                unit: RelativeTimeUnit::Years,
                n: 1,
            },
        ),
        (
            now + 5_000,
            RelativeTime {
                unit: RelativeTimeUnit::Now,
                n: 0,
            },
        ),
    ] {
        assert_eq!(relative_time(updated_at, now), expected);
    }
}
