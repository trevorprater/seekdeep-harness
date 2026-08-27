//! Pure Workspace browser grouping, flat-list, search, and relative-time projection.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use seekdeep_client_runtime::{
    ClientWorkspaceView, RuntimeSessionListState, RuntimeSessionSummary, SubagentSessionSummary,
    index_subagent_descendants,
};
use seekdeep_identity::{SessionId, WorkspaceId};
use serde_json::Value;

/// Group key for Sessions outside every Workspace.
pub const UNGROUPED_KEY: &str = "";
/// Display label for the ungrouped bucket row.
pub const UNGROUPED_LABEL: &str = "Ungrouped";

/// One top-level Session row in a group or flat list.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionNode {
    /// Stable Session identity.
    pub id: SessionId,
    /// Stored or canonical blank title.
    pub title: String,
    /// Whether this is the provisional blank Session.
    pub blank: bool,
    /// Current blocking interaction.
    pub pending_interaction: Option<Value>,
    /// Exact running bit.
    pub running: bool,
    /// Running descendants through uninterrupted subagent lineage.
    pub running_subagent_count: usize,
    /// Unread completion reminder.
    pub completed: bool,
    /// Last activity epoch milliseconds.
    pub updated_at: i64,
}

/// One Workspace group header plus optionally expanded Session rows.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupNode {
    /// Workspace identity or [`UNGROUPED_KEY`].
    pub key: String,
    /// Backing Workspace, absent for Ungrouped.
    pub workspace_id: Option<WorkspaceId>,
    /// Workspace directory.
    pub cwd: Option<String>,
    /// Parsed creation epoch milliseconds.
    pub created_at: Option<f64>,
    /// Workspace title or Ungrouped.
    pub label: String,
    /// Total visible Sessions even while folded.
    pub session_count: usize,
    /// Current local expansion bit.
    pub expanded: bool,
    /// Whether the selected Session belongs to this group.
    pub contains_current: bool,
    /// Visible rows, empty while folded.
    pub sessions: Vec<SessionNode>,
}

/// One flat search row combining list metadata and optional content match.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchResultNode {
    /// Stable Session identity.
    pub id: SessionId,
    /// Display title.
    pub title: String,
    /// Workspace title or cwd basename.
    pub workspace: String,
    /// Current blocking interaction.
    pub pending_interaction: Option<Value>,
    /// Exact running bit.
    pub running: bool,
    /// Running subagent descendants.
    pub running_subagent_count: usize,
    /// Unread completion reminder.
    pub completed: bool,
    /// Ranked Host content excerpt.
    pub snippet: Option<String>,
}

/// One Host-ranked content search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSearchResultItem {
    /// Matching Session.
    pub session_id: SessionId,
    /// Backend-authored excerpt.
    pub snippet: String,
}

/// Host content-search page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionSearchPage {
    /// Ranked results.
    pub items: Vec<SessionSearchResultItem>,
    /// Whether the Host capped the result.
    pub has_more: bool,
}

/// Bounded merged search projection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchResultSet {
    /// Local-first merged rows.
    pub items: Vec<SearchResultNode>,
    /// Query-refinement hint.
    pub has_more: bool,
}

/// Local group expansion and optional Ungrouped order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeView {
    /// Expanded Workspace group identities.
    pub expanded_groups: Vec<String>,
    /// Browser-local order for unaccounted Sessions.
    pub ungrouped_order: Option<Vec<String>>,
}

struct Group {
    key: String,
    workspace_id: Option<WorkspaceId>,
    cwd: Option<String>,
    created_at: Option<f64>,
    label: String,
    sessions: Vec<Rc<RuntimeSessionSummary>>,
}

/// Directory display label accepting POSIX and Windows separators.
#[must_use]
pub fn workspace_label(cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
        return UNGROUPED_LABEL.to_owned();
    };
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or_default();
    if base.is_empty() {
        cwd.to_owned()
    } else {
        base.to_owned()
    }
}

fn by_recency(left: &RuntimeSessionSummary, right: &RuntimeSessionSummary) -> Ordering {
    right
        .updated_at
        .cmp(&left.updated_at)
        .then_with(|| left.id.cmp(&right.id))
}

fn session_visible(
    session: &RuntimeSessionSummary,
    current: Option<&SessionId>,
    archived: &BTreeSet<SessionId>,
) -> bool {
    session.origin.as_deref() != Some("subagent")
        && !archived.contains(&session.id)
        && (!session.blank || current == Some(&session.id))
}

fn session_title(session: &RuntimeSessionSummary) -> String {
    if session.blank {
        "New Session".to_owned()
    } else {
        session.display_title.clone()
    }
}

fn parsed_date(value: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(value).map_or(f64::NAN, |date| {
        #[allow(clippy::cast_precision_loss)]
        {
            date.timestamp_millis() as f64
        }
    })
}

fn ordered_ungrouped(
    members: &[Rc<RuntimeSessionSummary>],
    stored: &[String],
) -> Vec<Rc<RuntimeSessionSummary>> {
    let by_id = members
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect::<BTreeMap<_, _>>();
    let mut included = BTreeSet::new();
    let mut ordered = Vec::new();
    for key in stored {
        let Some(session) = by_id.get(key.as_str()) else {
            continue;
        };
        if included.insert(key.clone()) {
            ordered.push((*session).clone());
        }
    }
    let mut rest = members.to_vec();
    rest.sort_by(|left, right| by_recency(left, right));
    for session in rest {
        if included.insert(session.id.as_str().to_owned()) {
            ordered.push(session);
        }
    }
    ordered
}

fn group_by_workspace(
    list: &RuntimeSessionListState,
    workspaces: &[Rc<ClientWorkspaceView>],
    archived: &BTreeSet<SessionId>,
    ungrouped_order: Option<&[String]>,
) -> Vec<Group> {
    let mut groups = Vec::new();
    let mut accounted = BTreeSet::new();
    for workspace in workspaces {
        let mut members = Vec::new();
        for id in &workspace.session_ids {
            let Some(summary) = list.by_id.get(id) else {
                continue;
            };
            accounted.insert(id.clone());
            if session_visible(summary, list.current.as_ref(), archived) {
                members.push(summary.clone());
            }
        }
        groups.push(Group {
            key: workspace.workspace_id.as_str().to_owned(),
            workspace_id: Some(workspace.workspace_id.clone()),
            cwd: Some(workspace.path.clone()),
            created_at: Some(parsed_date(&workspace.created_at)),
            label: workspace.title.clone(),
            sessions: members,
        });
    }
    let mut stray = list
        .ids
        .iter()
        .filter_map(|id| list.by_id.get(id))
        .filter(|summary| {
            !accounted.contains(&summary.id)
                && session_visible(summary, list.current.as_ref(), archived)
        })
        .cloned()
        .collect::<Vec<_>>();
    if !stray.is_empty() {
        stray = match ungrouped_order {
            None => {
                stray.sort_by(|left, right| by_recency(left, right));
                stray
            }
            Some(stored) => ordered_ungrouped(&stray, stored),
        };
        groups.push(Group {
            key: UNGROUPED_KEY.to_owned(),
            workspace_id: None,
            cwd: None,
            created_at: None,
            label: UNGROUPED_LABEL.to_owned(),
            sessions: stray,
        });
    }
    groups
}

fn descendant_index(
    list: &RuntimeSessionListState,
) -> BTreeMap<SessionId, seekdeep_client_runtime::SubagentDescendantSummary> {
    let summaries = list
        .by_id
        .values()
        .map(|summary| {
            (
                summary.id.clone(),
                SubagentSessionSummary {
                    id: summary.id.clone(),
                    parent_id: summary.parent_id.clone(),
                    subagent_origin: summary.origin.as_deref() == Some("subagent"),
                    running: summary.running,
                },
            )
        })
        .collect();
    index_subagent_descendants(&summaries)
}

fn session_node(
    session: &RuntimeSessionSummary,
    descendants: &BTreeMap<SessionId, seekdeep_client_runtime::SubagentDescendantSummary>,
) -> SessionNode {
    SessionNode {
        id: session.id.clone(),
        title: session_title(session),
        blank: session.blank,
        pending_interaction: session.pending_interaction.clone(),
        running: session.running,
        running_subagent_count: descendants
            .get(&session.id)
            .map_or(0, |summary| summary.running_count),
        completed: session.completed,
        updated_at: session.updated_at,
    }
}

/// Derives Workspace groups in Host order with local expansion and Ungrouped ordering.
#[must_use]
pub fn derive_groups(
    list: &RuntimeSessionListState,
    workspaces: &[Rc<ClientWorkspaceView>],
    archived_session_ids: &[SessionId],
    view: &TreeView,
) -> Vec<GroupNode> {
    let archived = archived_session_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let expanded = view.expanded_groups.iter().collect::<BTreeSet<_>>();
    let descendants = descendant_index(list);
    let current_group = list.current.as_ref().map(|current| {
        workspaces
            .iter()
            .find(|workspace| workspace.session_ids.contains(current))
            .map_or_else(
                || UNGROUPED_KEY.to_owned(),
                |workspace| workspace.workspace_id.as_str().to_owned(),
            )
    });
    group_by_workspace(list, workspaces, &archived, view.ungrouped_order.as_deref())
        .into_iter()
        .map(|group| {
            let is_expanded = expanded.contains(&group.key);
            GroupNode {
                key: group.key.clone(),
                workspace_id: group.workspace_id,
                cwd: group.cwd,
                created_at: group.created_at,
                label: group.label,
                session_count: group.sessions.len(),
                expanded: is_expanded,
                contains_current: current_group.as_ref() == Some(&group.key),
                sessions: if is_expanded {
                    group
                        .sessions
                        .iter()
                        .map(|session| session_node(session, &descendants))
                        .collect()
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

/// Derives the hierarchy-free newest-first Session list.
#[must_use]
pub fn derive_flat(
    list: &RuntimeSessionListState,
    archived_session_ids: &[SessionId],
) -> Vec<SessionNode> {
    let archived = archived_session_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let descendants = descendant_index(list);
    let mut rows = list
        .ids
        .iter()
        .filter_map(|id| list.by_id.get(id))
        .filter(|summary| session_visible(summary, list.current.as_ref(), &archived))
        .cloned()
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| by_recency(left, right));
    rows.iter()
        .map(|session| session_node(session, &descendants))
        .collect()
}

fn js_trim(value: &str) -> &str {
    value.trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
}

/// Merges local title/Workspace substring matches before ranked Host content hits.
#[must_use]
pub fn derive_search_results(
    list: &RuntimeSessionListState,
    workspaces: &[Rc<ClientWorkspaceView>],
    query: &str,
    archived_session_ids: &[SessionId],
    content: &SessionSearchPage,
    limit: usize,
) -> SearchResultSet {
    let query = js_trim(query).to_lowercase();
    if query.is_empty() {
        return SearchResultSet::default();
    }
    let archived = archived_session_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let descendants = descendant_index(list);
    let mut workspace_by_session = BTreeMap::new();
    for workspace in workspaces {
        for session_id in &workspace.session_ids {
            workspace_by_session
                .entry(session_id.clone())
                .or_insert_with(|| workspace.title.clone());
        }
    }
    let label_of = |summary: &RuntimeSessionSummary| {
        workspace_by_session
            .get(&summary.id)
            .cloned()
            .unwrap_or_else(|| workspace_label(summary.cwd.as_deref()))
    };
    let mut content_by_session = BTreeMap::new();
    for item in &content.items {
        content_by_session
            .entry(item.session_id.clone())
            .or_insert(item);
    }
    let mut local = list
        .ids
        .iter()
        .filter_map(|id| list.by_id.get(id))
        .filter(|summary| {
            !summary.blank
                && session_visible(summary, list.current.as_ref(), &archived)
                && (session_title(summary).to_lowercase().contains(&query)
                    || label_of(summary).to_lowercase().contains(&query))
        })
        .cloned()
        .collect::<Vec<_>>();
    local.sort_by(|left, right| by_recency(left, right));
    let mut ordered = Vec::new();
    let mut included = BTreeSet::new();
    for summary in local {
        if included.insert(summary.id.clone()) {
            ordered.push(summary);
        }
    }
    for item in &content.items {
        let Some(summary) = list.by_id.get(&item.session_id) else {
            continue;
        };
        if !summary.blank
            && session_visible(summary, list.current.as_ref(), &archived)
            && included.insert(summary.id.clone())
        {
            ordered.push(summary.clone());
        }
    }
    SearchResultSet {
        has_more: content.has_more || ordered.len() > limit,
        items: ordered
            .into_iter()
            .take(limit)
            .map(|summary| SearchResultNode {
                id: summary.id.clone(),
                title: session_title(&summary),
                workspace: label_of(&summary),
                pending_interaction: summary.pending_interaction.clone(),
                running: summary.running,
                running_subagent_count: descendants
                    .get(&summary.id)
                    .map_or(0, |entry| entry.running_count),
                completed: summary.completed,
                snippet: content_by_session
                    .get(&summary.id)
                    .map(|item| item.snippet.clone()),
            })
            .collect(),
    }
}

/// Relative-time bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativeTimeUnit {
    /// Less than one minute.
    Now,
    /// Whole minutes.
    Minutes,
    /// Whole hours.
    Hours,
    /// Whole days.
    Days,
    /// Thirty-day months.
    Months,
    /// 365-day years.
    Years,
}

/// Structured relative time with zero only for [`RelativeTimeUnit::Now`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelativeTime {
    /// Time bucket.
    pub unit: RelativeTimeUnit,
    /// Whole bucket magnitude.
    pub n: u64,
}

/// Derives compact relative-time buckets from injected epoch milliseconds.
#[must_use]
pub fn relative_time(updated_at: i64, now: i64) -> RelativeTime {
    const MINUTE: i128 = 60_000;
    const HOUR: i128 = 3_600_000;
    const DAY: i128 = 86_400_000;
    let diff = (i128::from(now) - i128::from(updated_at)).max(0);
    let (unit, divisor) = if diff < MINUTE {
        (RelativeTimeUnit::Now, None)
    } else if diff < HOUR {
        (RelativeTimeUnit::Minutes, Some(MINUTE))
    } else if diff < DAY {
        (RelativeTimeUnit::Hours, Some(HOUR))
    } else if diff < 30 * DAY {
        (RelativeTimeUnit::Days, Some(DAY))
    } else if diff < 365 * DAY {
        (RelativeTimeUnit::Months, Some(30 * DAY))
    } else {
        (RelativeTimeUnit::Years, Some(365 * DAY))
    };
    RelativeTime {
        unit,
        n: divisor.map_or(0, |divisor| {
            u64::try_from(diff / divisor).unwrap_or(u64::MAX)
        }),
    }
}
