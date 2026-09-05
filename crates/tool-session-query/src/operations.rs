//! Tool operation orchestration over session-query service capabilities.

use std::{
    collections::{BTreeSet, HashSet},
    future::Future,
};

use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, HarnessError};
use seekdeep_session_query::{
    SESSION_QUERY, SessionEventSearchHit, SessionRecord, SessionSearchCursor, SessionSearchHit,
    types::{
        SessionEventReadRequest, SessionEventSearchRequest, SessionEventTraceRequest,
        SessionSearchExecContext, SessionSearchRequest,
    },
};
use seekdeep_tools::ToolRunContext;

use crate::{
    input::{
        self, EventFilterInput, EventReadArgs, EventSearchArgs, EventTargetArgs, SessionSearchArgs,
        SessionTargetArgs,
    },
    presentation, service_boundary, workspace_access,
};

/// One collected model-facing search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchCollection<T> {
    /// Accepted items up to the configured cap.
    pub items: Vec<T>,
    /// Whether another accepted item existed beyond the cap.
    pub capped: bool,
}

/// Executes cross-session search with workspace and parent authority.
///
/// # Errors
///
/// Returns validation, caller authority, cancellation, or sanitized service failures.
#[allow(clippy::too_many_lines)]
pub async fn execute_session_search(
    context: &Context,
    args: &SessionSearchArgs,
    run: &ToolRunContext,
    max_results: usize,
) -> anyhow::Result<String> {
    let caller = workspace_access::caller_of(run)?;
    let cwd = caller.header.cwd.clone().ok_or_else(|| {
        HarnessError::new(
            "cross-session search is unavailable because the caller session has no workspace",
            "SESSION_QUERY_TOOL_UNAUTHORIZED",
        )
    })?;
    let query = input::normalize_query(&args.query)?;
    let mut session_filters = input::build_session_filters(args)?;
    let event_filters = input::build_event_filters(EventFilterInput {
        seq_from: args.event_seq_from,
        seq_to: args.event_seq_to,
        time_from: args.event_time_from.as_deref(),
        time_to: args.event_time_to.as_deref(),
        event_types: args.event_types.as_deref(),
        surfaces: args.event_surfaces.as_deref(),
    })?;
    let requested_parents =
        input::materialize_parent_session_ids(args.parent_session_ids.as_deref())?;
    let signal = run.signal();
    if requested_parents.is_some() || args.include_root_sessions == Some(true) {
        let authorized = match &requested_parents {
            Some(ids) => {
                workspace_access::authorize_session_ids(context, &caller, ids, &signal).await?
            }
            None => BTreeSet::new(),
        };
        let mut values = requested_parents
            .as_ref()
            .into_iter()
            .flatten()
            .filter(|id| authorized.contains(*id))
            .cloned()
            .map(Some)
            .collect::<Vec<_>>();
        if args.include_root_sessions == Some(true) {
            values.push(None);
        }
        if values.is_empty() {
            return Ok(presentation::format_empty_session_search().to_owned());
        }
        session_filters.push(seekdeep_session_query::SessionResultFilter::Parent { values });
    }
    session_filters.push(seekdeep_session_query::SessionResultFilter::Cwd {
        values: vec![Some(cwd)],
    });
    let service = query_service(context)?;
    let request_service = service.clone();
    let request_context = context.clone();
    let request_signal = signal.clone();
    let collected = collect_pages(
        max_results,
        &signal,
        move |cursor| {
            let service = request_service.clone();
            let context = request_context.clone();
            let signal = request_signal.clone();
            let query = query.clone();
            let session_filters = session_filters.clone();
            let event_filters = event_filters.clone();
            async move {
                service_boundary::call(&context, &signal, "session search", || {
                    service.search_sessions(
                        SessionSearchRequest {
                            query,
                            session_filters: Some(session_filters),
                            event_filters: Some(event_filters),
                            limit: None,
                            cursor,
                        },
                        Some(SessionSearchExecContext {
                            signal: Some(signal.clone()),
                        }),
                    )
                })
                .await
            }
        },
        |hit: &SessionSearchHit| {
            hit.record.header.id != caller.id
                && workspace_access::record_authorized(&hit.record, &caller)
        },
    )
    .await?;
    let parent_ids = collected
        .items
        .iter()
        .filter_map(|hit| hit.record.header.parent_session.clone())
        .collect::<Vec<_>>();
    let authorized_parents =
        workspace_access::authorize_session_ids(context, &caller, &parent_ids, &signal).await?;
    let ids = collected
        .items
        .iter()
        .map(|hit| hit.record.header.id.clone())
        .collect::<Vec<_>>();
    let titles = workspace_access::read_titles(context, &caller, &ids, &signal).await?;
    Ok(presentation::format_session_search(
        &collected,
        &titles,
        &authorized_parents,
    ))
}

/// Executes within-session event search with current-step exclusion.
///
/// # Errors
///
/// Returns validation, caller authority, cancellation, or sanitized service failures.
#[allow(clippy::too_many_lines)]
pub async fn execute_event_search(
    context: &Context,
    args: &EventSearchArgs,
    run: &ToolRunContext,
    max_results: usize,
) -> anyhow::Result<String> {
    let caller = workspace_access::caller_of(run)?;
    let session_id = workspace_access::target_id(args.session_id.as_deref(), &caller);
    let signal = run.signal();
    workspace_access::authorize_target(context, &caller, &session_id, &signal).await?;
    let query = input::normalize_query(&args.query)?;
    let seq_from_value = args
        .seq_from
        .map(|value| input::non_negative_safe("sequence lower bound", value))
        .transpose()?;
    let seq_to_value = args
        .seq_to
        .map(|value| input::non_negative_safe("sequence upper bound", value))
        .transpose()?;
    if seq_from_value
        .zip(seq_to_value)
        .is_some_and(|(from, to)| from > to)
    {
        return Err(seekdeep_session_query::SessionQueryError::new(
            "session sequence range from must be less than or equal to to",
            seekdeep_session_query::SessionQueryErrorCode::SessionQueryInvalidFilter,
        )
        .into());
    }
    let mut seq_to = args.seq_to;
    if session_id == caller.id {
        let step = caller
            .events
            .iter()
            .rev()
            .find(|event| event.event_type == "step/start")
            .ok_or_else(|| {
                HarnessError::new(
                    "current-session search requires an active step boundary",
                    "SESSION_QUERY_TOOL_NO_CURRENT_STEP",
                )
            })?;
        let boundary = i64::try_from(step.seq).unwrap_or(i64::MAX) - 1;
        seq_to = Some(seq_to.map_or(boundary, |value| value.min(boundary)));
    }
    let title = workspace_access::read_title(context, &caller, &session_id, &signal).await?;
    if args
        .seq_from
        .zip(seq_to)
        .is_some_and(|(from, to)| from > to)
    {
        return Ok(presentation::format_event_search(
            &session_id,
            &title,
            &SearchCollection {
                items: Vec::new(),
                capped: false,
            },
        ));
    }
    let filters = input::build_event_filters(EventFilterInput {
        seq_from: args.seq_from,
        seq_to,
        time_from: args.time_from.as_deref(),
        time_to: args.time_to.as_deref(),
        event_types: args.event_types.as_deref(),
        surfaces: args.surfaces.as_deref(),
    })?;
    let service = query_service(context)?;
    let request_service = service.clone();
    let request_context = context.clone();
    let request_signal = signal.clone();
    let request_id = session_id.clone();
    let observed_caller = caller.clone();
    let collected = collect_pages(
        max_results,
        &signal,
        move |cursor| {
            let service = request_service.clone();
            let context = request_context.clone();
            let signal = request_signal.clone();
            let session_id = request_id.clone();
            let caller = observed_caller.clone();
            let query = query.clone();
            let filters = filters.clone();
            async move {
                let page = service_boundary::call(&context, &signal, "event search", || {
                    service.search_events(
                        SessionEventSearchRequest {
                            session_id: session_id.clone(),
                            query,
                            filters: Some(filters),
                            limit: None,
                            cursor,
                        },
                        Some(SessionSearchExecContext {
                            signal: Some(signal.clone()),
                        }),
                    )
                })
                .await?;
                workspace_access::assert_observed_target_authorized(
                    &caller,
                    &session_id,
                    &page.session,
                )?;
                Ok(page.page)
            }
        },
        |_hit: &SessionEventSearchHit| true,
    )
    .await?;
    Ok(presentation::format_event_search(
        &session_id,
        &title,
        &collected,
    ))
}

/// Executes one workspace-redacted lineage trace.
///
/// # Errors
///
/// Returns caller authority, cancellation, lineage, title, or sanitized service failures.
pub async fn execute_session_trace(
    context: &Context,
    args: &SessionTargetArgs,
    run: &ToolRunContext,
) -> anyhow::Result<String> {
    let caller = workspace_access::caller_of(run)?;
    let session_id = workspace_access::target_id(args.session_id.as_deref(), &caller);
    let signal = run.signal();
    workspace_access::authorize_target(context, &caller, &session_id, &signal).await?;
    let service = query_service(context)?;
    let trace = service_boundary::call(context, &signal, "session lineage trace", || {
        service.trace_session(session_id.clone(), Some(signal.clone()))
    })
    .await?;
    workspace_access::assert_observed_target_authorized(
        &caller,
        &session_id,
        &trace.target.header,
    )?;
    let mut ancestors = Vec::<SessionRecord>::new();
    let mut ancestor_boundary = false;
    for ancestor in &trace.ancestors {
        if !workspace_access::record_authorized(ancestor, &caller) {
            ancestor_boundary = true;
            break;
        }
        ancestors.push(ancestor.clone());
    }
    if ancestors.len() == trace.ancestors.len() && !trace.complete {
        ancestor_boundary = true;
    }
    let descendants = workspace_access::authorize_descendants(&trace.descendants, &caller);
    let mut visible_ids = vec![trace.target.header.id.clone()];
    visible_ids.extend(ancestors.iter().map(|record| record.header.id.clone()));
    visible_ids.extend(workspace_access::descendant_ids(&descendants));
    let titles = workspace_access::read_titles(context, &caller, &visible_ids, &signal).await?;
    Ok(presentation::format_session_trace(
        &trace,
        &ancestors,
        ancestor_boundary,
        &descendants,
        &titles,
    ))
}

/// Executes one event relationship trace.
///
/// # Errors
///
/// Returns validation, caller authority, cancellation, title, or service failures.
pub async fn execute_event_trace(
    context: &Context,
    args: &EventTargetArgs,
    run: &ToolRunContext,
) -> anyhow::Result<String> {
    let seq = input::non_negative_safe("seq", args.seq)?;
    let caller = workspace_access::caller_of(run)?;
    let session_id = workspace_access::target_id(args.session_id.as_deref(), &caller);
    let signal = run.signal();
    workspace_access::authorize_target(context, &caller, &session_id, &signal).await?;
    let service = query_service(context)?;
    let trace = service_boundary::call(context, &signal, "event trace", || {
        service.trace_event(
            SessionEventTraceRequest {
                session_id: session_id.clone(),
                seq,
            },
            Some(signal.clone()),
        )
    })
    .await?;
    workspace_access::assert_observed_target_authorized(&caller, &session_id, &trace.session)?;
    let title = workspace_access::read_title(context, &caller, &session_id, &signal).await?;
    Ok(presentation::format_event_trace(
        &session_id,
        &title,
        &trace,
    ))
}

/// Executes one exact event read plus optional neighbors.
///
/// # Errors
///
/// Returns validation, caller authority, cancellation, serialization, or service failures.
pub async fn execute_event_read(
    context: &Context,
    args: &EventReadArgs,
    run: &ToolRunContext,
) -> anyhow::Result<String> {
    let seq = input::non_negative_safe("seq", args.seq)?;
    let before = args
        .before
        .map(|value| input::non_negative_safe("before", value))
        .transpose()?;
    let after = args
        .after
        .map(|value| input::non_negative_safe("after", value))
        .transpose()?;
    let caller = workspace_access::caller_of(run)?;
    let session_id = workspace_access::target_id(args.session_id.as_deref(), &caller);
    let signal = run.signal();
    workspace_access::authorize_target(context, &caller, &session_id, &signal).await?;
    let service = query_service(context)?;
    let window = service_boundary::call(context, &signal, "event read", || {
        service.read_event(
            SessionEventReadRequest {
                session_id: session_id.clone(),
                seq,
                before,
                after,
            },
            Some(signal.clone()),
        )
    })
    .await?;
    workspace_access::assert_observed_target_authorized(&caller, &session_id, &window.session)?;
    let title = workspace_access::read_title(context, &caller, &session_id, &signal).await?;
    presentation::format_event_read(&session_id, &title, &window)
}

async fn collect_pages<T, Request, RequestFuture, Accept>(
    max_results: usize,
    signal: &AbortSignal,
    mut request: Request,
    mut accept: Accept,
) -> anyhow::Result<SearchCollection<T>>
where
    Request: FnMut(Option<SessionSearchCursor>) -> RequestFuture,
    RequestFuture: Future<Output = anyhow::Result<seekdeep_session_query::SessionSearchPage<T>>>,
    Accept: FnMut(&T) -> bool,
{
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = None;
    loop {
        ensure_not_aborted(signal)?;
        let page = request(cursor).await?;
        ensure_not_aborted(signal)?;
        for item in page.items {
            if !accept(&item) {
                continue;
            }
            if items.len() == max_results {
                return Ok(SearchCollection {
                    items,
                    capped: true,
                });
            }
            items.push(item);
        }
        let Some(next) = page.next_cursor else {
            return Ok(SearchCollection {
                items,
                capped: false,
            });
        };
        if !seen.insert(next.clone()) {
            return Err(seekdeep_session_query::SessionQueryError::new(
                "session-search provider repeated a continuation cursor",
                seekdeep_session_query::SessionQueryErrorCode::SessionQueryInvalidCursor,
            )
            .into());
        }
        cursor = Some(next);
    }
}

fn ensure_not_aborted(signal: &AbortSignal) -> anyhow::Result<()> {
    if signal.is_aborted() {
        return Err(HarnessError::named(
            "AbortError",
            "session query was cancelled",
            "SESSION_QUERY_ABORTED",
        )
        .into());
    }
    Ok(())
}

fn query_service(
    context: &Context,
) -> anyhow::Result<std::sync::Arc<seekdeep_session_query::SessionQueryService>> {
    context
        .get(SESSION_QUERY)
        .ok_or_else(|| anyhow::anyhow!("tool-session-query requires sessionQuery"))
}
