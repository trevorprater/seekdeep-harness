//! Session-query service error containment and model-safe translation.

use std::future::Future;

use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, HarnessError};
use seekdeep_session_query::{SessionQueryError, SessionQueryErrorCode};
use serde_json::json;

/// Stable tool-local authorization refusal.
pub fn unauthorized_target() -> anyhow::Error {
    HarnessError::new(
        "session target is outside the caller workspace",
        "SESSION_QUERY_TOOL_UNAUTHORIZED",
    )
    .into()
}

/// Invokes one service operation with cancellation precedence and sanitization.
///
/// # Errors
///
/// Returns caller cancellation or a fixed model-safe service failure.
pub async fn call<Value, Invoke, Fut>(
    context: &Context,
    signal: &AbortSignal,
    operation: &str,
    invoke: Invoke,
) -> anyhow::Result<Value>
where
    Invoke: FnOnce() -> Fut,
    Fut: Future<Output = anyhow::Result<Value>>,
{
    ensure_not_aborted(signal)?;
    match invoke().await {
        Ok(value) => {
            ensure_not_aborted(signal)?;
            Ok(value)
        }
        Err(error) => {
            ensure_not_aborted(signal)?;
            Err(sanitize_error(context, operation, &error))
        }
    }
}

/// Logs one full diagnostic and returns only a fixed model-safe failure.
pub fn sanitize_error(context: &Context, operation: &str, error: &anyhow::Error) -> anyhow::Error {
    context.logger(None).warn([json!(format!(
        "tool-session-query: {operation} failed: {error:#}"
    ))]);
    if let Some(error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SessionQueryError>())
    {
        return safe_query_failure(error.code);
    }
    if error
        .chain()
        .find_map(|cause| cause.downcast_ref::<HarnessError>())
        .is_some_and(|error| error.code() == "SESSION_QUERY_TOOL_UNAUTHORIZED")
    {
        return unauthorized_target();
    }
    generic_failure()
}

fn safe_query_failure(code: SessionQueryErrorCode) -> anyhow::Error {
    let (code, message) = match code {
        SessionQueryErrorCode::SessionQueryAborted => {
            ("SESSION_QUERY_ABORTED", "session query was cancelled")
        }
        SessionQueryErrorCode::SessionQueryCorruptSession => (
            "SESSION_QUERY_CORRUPT_SESSION",
            "session event history is corrupt",
        ),
        SessionQueryErrorCode::SessionQueryEventNotFound => (
            "SESSION_QUERY_EVENT_NOT_FOUND",
            "session event was not found",
        ),
        SessionQueryErrorCode::SessionQueryIndexFailed => (
            "SESSION_QUERY_INDEX_FAILED",
            "session search index is unavailable",
        ),
        SessionQueryErrorCode::SessionQueryInvalidConfig
        | SessionQueryErrorCode::SessionQuerySourceConflict => {
            return generic_failure();
        }
        SessionQueryErrorCode::SessionQueryInvalidCursor => (
            "SESSION_QUERY_INVALID_CURSOR",
            "session search continuation is invalid",
        ),
        SessionQueryErrorCode::SessionQueryInvalidFilter => (
            "SESSION_QUERY_INVALID_FILTER",
            "session query filters were rejected",
        ),
        SessionQueryErrorCode::SessionQueryInvalidLimit => (
            "SESSION_QUERY_INVALID_LIMIT",
            "session query result limit was rejected",
        ),
        SessionQueryErrorCode::SessionQueryInvalidQuery => {
            ("SESSION_QUERY_INVALID_QUERY", "session query was rejected")
        }
        SessionQueryErrorCode::SessionQueryInvalidLineage => (
            "SESSION_QUERY_INVALID_LINEAGE",
            "session lineage is invalid",
        ),
        SessionQueryErrorCode::SessionQueryInvalidSurface => (
            "SESSION_QUERY_INVALID_SURFACE",
            "session event history is invalid",
        ),
        SessionQueryErrorCode::SessionQueryInvalidWindow => (
            "SESSION_QUERY_INVALID_WINDOW",
            "session event window is invalid",
        ),
        SessionQueryErrorCode::SessionQueryPersistenceFailed => (
            "SESSION_QUERY_PERSISTENCE_FAILED",
            "session history storage is unavailable",
        ),
        SessionQueryErrorCode::SessionQuerySearchDisabled => (
            "SESSION_QUERY_SEARCH_DISABLED",
            "session search is disabled in this deployment",
        ),
        SessionQueryErrorCode::SessionQuerySessionNotFound => {
            ("SESSION_QUERY_SESSION_NOT_FOUND", "session was not found")
        }
        SessionQueryErrorCode::SessionQueryStaleCursor => (
            "SESSION_QUERY_STALE_CURSOR",
            "session history changed while paging; retry the complete search call",
        ),
    };
    HarnessError::named("SessionQueryError", message, code).into()
}

fn generic_failure() -> anyhow::Error {
    HarnessError::new(
        "session query operation failed",
        "SESSION_QUERY_TOOL_FAILED",
    )
    .into()
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
