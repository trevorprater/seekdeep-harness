//! Caller identity, workspace authorization, and visible lineage projection.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
};

use seekdeep_cordis::Context;
use seekdeep_core::session::{SessionEvent, SessionHeader, SessionId};
use seekdeep_llm::{AbortSignal, HarnessError};
use seekdeep_session_query::{
    LogicalProjectionResult, SESSION_QUERY, SessionLineageNode, SessionRecord, SessionResultFilter,
};
use seekdeep_tools::ToolRunContext;

use crate::service_boundary;

/// Detached caller authority captured before asynchronous work.
#[derive(Clone, Debug)]
pub struct Caller {
    /// Calling session id.
    pub id: SessionId,
    /// Calling session header.
    pub header: SessionHeader,
    /// Calling session events used for the current-step boundary.
    pub events: Vec<SessionEvent>,
}

/// Model-safe title view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleView {
    /// Visible title or fallback.
    pub text: String,
    /// Fixed reason when title projection failed.
    pub unavailable_code: Option<String>,
}

/// One authorized descendant or a redacted subtree marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedDescendant {
    /// Visible session record.
    pub record: SessionRecord,
    /// Visible children or pruned-subtree markers.
    pub descendants: Vec<Option<AuthorizedDescendant>>,
}

/// Iterative preorder visit item.
pub struct DescendantVisit<'a> {
    /// Visible node or boundary marker.
    pub node: Option<&'a AuthorizedDescendant>,
    /// Tree depth.
    pub depth: usize,
}

/// Requires an exact live agent caller rather than a synthetic session fallback.
///
/// # Errors
///
/// Returns `SESSION_QUERY_TOOL_MISSING_AGENT` without an exact live agent.
pub fn caller_of(run: &ToolRunContext) -> anyhow::Result<Caller> {
    let agent = run.agent.as_ref().ok_or_else(|| {
        HarnessError::new(
            "session query tools require an agent-bound caller",
            "SESSION_QUERY_TOOL_MISSING_AGENT",
        )
    })?;
    let session = agent.session();
    Ok(Caller {
        id: session.id().clone(),
        header: session.header().clone(),
        events: session.events(),
    })
}

/// Resolves an explicit target or defaults to the captured caller.
pub fn target_id(value: Option<&str>, caller: &Caller) -> SessionId {
    value.map_or_else(|| caller.id.clone(), SessionId::new)
}

/// Authorizes a direct target without revealing existence across workspaces.
///
/// # Errors
///
/// Returns cancellation, a sanitized service failure, or the fixed authority refusal.
pub async fn authorize_target(
    context: &Context,
    caller: &Caller,
    target: &SessionId,
    signal: &AbortSignal,
) -> anyhow::Result<()> {
    if target == &caller.id {
        return Ok(());
    }
    let cwd = caller
        .header
        .cwd
        .as_ref()
        .ok_or_else(service_boundary::unauthorized_target)?;
    let query = query_service(context)?;
    let filters = [
        SessionResultFilter::Id {
            values: vec![target.clone()],
        },
        SessionResultFilter::Cwd {
            values: vec![Some(cwd.clone())],
        },
    ];
    let records = service_boundary::call(context, signal, "target authorization", || {
        query.filter_sessions(&filters, Some(signal.clone()))
    })
    .await?;
    if records.len() != 1 {
        return Err(service_boundary::unauthorized_target());
    }
    Ok(())
}

/// Whether one record remains inside the captured workspace.
pub fn record_authorized(record: &SessionRecord, caller: &Caller) -> bool {
    header_authorized(&record.header, caller)
}

fn header_authorized(header: &SessionHeader, caller: &Caller) -> bool {
    if header.id == caller.id {
        return header.cwd == caller.header.cwd;
    }
    caller.header.cwd.is_some() && header.cwd == caller.header.cwd
}

/// Rechecks the target/header observation returned after preauthorization.
///
/// # Errors
///
/// Returns the fixed authority refusal if identity or workspace moved.
pub fn assert_observed_target_authorized(
    caller: &Caller,
    target: &SessionId,
    observed: &SessionHeader,
) -> anyhow::Result<()> {
    if &observed.id != target || !header_authorized(observed, caller) {
        return Err(service_boundary::unauthorized_target());
    }
    Ok(())
}

/// Bulk-authorizes requested ids without revealing missing/hidden identities.
///
/// # Errors
///
/// Returns cancellation or a sanitized service failure.
pub async fn authorize_session_ids(
    context: &Context,
    caller: &Caller,
    ids: &[SessionId],
    signal: &AbortSignal,
) -> anyhow::Result<BTreeSet<SessionId>> {
    let mut seen = HashSet::new();
    let unique = ids
        .iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect::<Vec<_>>();
    let mut authorized = BTreeSet::new();
    if unique.contains(&caller.id) {
        authorized.insert(caller.id.clone());
    }
    let Some(cwd) = &caller.header.cwd else {
        return Ok(authorized);
    };
    let other = unique
        .iter()
        .filter(|id| *id != &caller.id)
        .cloned()
        .collect::<Vec<_>>();
    if other.is_empty() {
        return Ok(authorized);
    }
    let query = query_service(context)?;
    let filters = [
        SessionResultFilter::Id {
            values: other.clone(),
        },
        SessionResultFilter::Cwd {
            values: vec![Some(cwd.clone())],
        },
    ];
    let records = service_boundary::call(context, signal, "session-id authorization", || {
        query.filter_sessions(&filters, Some(signal.clone()))
    })
    .await?;
    let requested = other.into_iter().collect::<BTreeSet<_>>();
    for record in records {
        if requested.contains(&record.header.id) && record_authorized(&record, caller) {
            authorized.insert(record.header.id);
        }
    }
    Ok(authorized)
}

/// Reads titles in one batch while isolating per-session failures.
///
/// # Errors
///
/// Returns cancellation, sanitized batch failures, or moved-target authority failures.
pub async fn read_titles(
    context: &Context,
    caller: &Caller,
    ids: &[SessionId],
    signal: &AbortSignal,
) -> anyhow::Result<BTreeMap<SessionId, TitleView>> {
    let query = query_service(context)?;
    let observations = service_boundary::call(context, signal, "title observation", || {
        query.read_title_snapshots(ids, Some(signal.clone()))
    })
    .await?;
    let mut result = BTreeMap::new();
    for observation in observations {
        match observation {
            LogicalProjectionResult::Fulfilled { session_id, value } => {
                assert_observed_target_authorized(caller, &session_id, &value.session)?;
                result.insert(
                    session_id,
                    TitleView {
                        text: value
                            .title
                            .map_or_else(|| "untitled".to_owned(), |title| title.event.title),
                        unavailable_code: None,
                    },
                );
            }
            LogicalProjectionResult::Rejected { session_id, reason } => {
                result.insert(session_id, unavailable_title(context, reason.as_ref())?);
            }
        }
    }
    Ok(result)
}

/// Reads one title through the same batch boundary.
///
/// # Errors
///
/// Returns the batch failure or a missing-observation invariant failure.
pub async fn read_title(
    context: &Context,
    caller: &Caller,
    id: &SessionId,
    signal: &AbortSignal,
) -> anyhow::Result<TitleView> {
    read_titles(context, caller, std::slice::from_ref(id), signal)
        .await?
        .remove(id)
        .ok_or_else(|| anyhow::anyhow!("title observation omitted session {id}"))
}

fn unavailable_title(context: &Context, error: &anyhow::Error) -> anyhow::Result<TitleView> {
    let sanitized = service_boundary::sanitize_error(context, "title observation item", error);
    if sanitized
        .chain()
        .find_map(|cause| cause.downcast_ref::<HarnessError>())
        .is_some_and(|error| error.code() == "SESSION_QUERY_TOOL_UNAUTHORIZED")
    {
        return Err(sanitized);
    }
    let code = sanitized
        .chain()
        .find_map(|cause| cause.downcast_ref::<HarnessError>())
        .map_or("SESSION_QUERY_TOOL_FAILED", HarnessError::code)
        .to_owned();
    Ok(TitleView {
        text: "untitled".to_owned(),
        unavailable_code: Some(code),
    })
}

/// Projects descendants iteratively, pruning unauthorized subtrees to markers.
pub fn authorize_descendants(
    nodes: &[SessionLineageNode],
    caller: &Caller,
) -> Vec<Option<AuthorizedDescendant>> {
    struct ArenaNode {
        record: Option<SessionRecord>,
        children: Vec<usize>,
    }
    let mut arena = Vec::<ArenaNode>::new();
    let mut roots = Vec::new();
    let mut pending = nodes
        .iter()
        .rev()
        .map(|node| (node, None::<usize>))
        .collect::<Vec<_>>();
    while let Some((node, parent)) = pending.pop() {
        let index = arena.len();
        arena.push(ArenaNode {
            record: record_authorized(&node.session, caller).then(|| node.session.clone()),
            children: Vec::new(),
        });
        if let Some(parent) = parent {
            arena[parent].children.push(index);
        } else {
            roots.push(index);
        }
        if arena[index].record.is_none() {
            continue;
        }
        for child in node.descendants.iter().rev() {
            pending.push((child, Some(index)));
        }
    }
    let mut built = vec![None; arena.len()];
    for index in (0..arena.len()).rev() {
        built[index] = Some(arena[index].record.take().map(|record| {
            AuthorizedDescendant {
                record,
                descendants: arena[index]
                    .children
                    .iter()
                    .map(|child| built[*child].take().unwrap_or(None))
                    .collect(),
            }
        }));
    }
    roots
        .into_iter()
        .map(|root| built[root].take().unwrap_or(None))
        .collect()
}

/// Visits authorized descendants and boundary markers in source preorder.
pub fn visit_descendants(nodes: &[Option<AuthorizedDescendant>]) -> Vec<DescendantVisit<'_>> {
    let mut output = Vec::new();
    let mut pending = nodes
        .iter()
        .rev()
        .map(|node| (node.as_ref(), 0_usize))
        .collect::<Vec<_>>();
    while let Some((node, depth)) = pending.pop() {
        output.push(DescendantVisit { node, depth });
        if let Some(node) = node {
            pending.extend(
                node.descendants
                    .iter()
                    .rev()
                    .map(|child| (child.as_ref(), depth + 1)),
            );
        }
    }
    output
}

/// Flattens every visible descendant id.
pub fn descendant_ids(nodes: &[Option<AuthorizedDescendant>]) -> Vec<SessionId> {
    visit_descendants(nodes)
        .into_iter()
        .filter_map(|visit| visit.node.map(|node| node.record.header.id.clone()))
        .collect()
}

/// Renders one title and its fixed unavailable annotation.
pub fn title_text(view: Option<&TitleView>) -> String {
    let Some(view) = view else {
        return "untitled (title unavailable: SESSION_QUERY_TOOL_FAILED)".to_owned();
    };
    view.unavailable_code.as_ref().map_or_else(
        || view.text.clone(),
        |code| format!("{} (title unavailable: {code})", view.text),
    )
}

fn query_service(
    context: &Context,
) -> anyhow::Result<Arc<seekdeep_session_query::SessionQueryService>> {
    context
        .get(SESSION_QUERY)
        .ok_or_else(|| anyhow::anyhow!("tool-session-query requires sessionQuery"))
}
