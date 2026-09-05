//! Session-query domain: configuration, failure taxonomy, pagination cursor,
//! source-header compatibility, semantic text extraction, provider-independent
//! filters, document projection, lineage tracing, and the public record
//! vocabulary. The corpus and query service are ported separately.

pub mod config;
pub mod corpus;
pub mod cursor;
pub mod documents;
pub mod extraction;
pub mod filters;
pub mod index;
pub mod invariant;
pub mod sources;
pub mod tracing;
pub mod types;

pub use config::{
    Config, SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY, SESSION_QUERY_READ_WINDOW_MAX,
    SessionQueryError, SessionQueryErrorCode, normalize_session_query_whitespace,
};
pub use corpus::{LogicalProjectionResult, LogicalSession, LogicalSessionSource, SessionCorpus};
pub use cursor::SessionSearchCursor;
pub use documents::{build_session_event_records, build_session_event_search_documents};
pub use extraction::extract_session_event_text;
pub use filters::{
    compile_session_text_filter, filter_session_event_documents, filter_session_results,
    materialize_session_event_result_filters, materialize_session_result_filters,
};
pub use index::{SESSION_QUERY, SessionQueryEngine, SessionQueryService, resolve_config};
pub use sources::assert_session_headers_compatible;
pub use tracing::{current_surface_events, event_records, trace_event, trace_session};
pub use types::{
    SessionAvailability, SessionEventReadRequest, SessionEventRecord, SessionEventResultFilter,
    SessionEventSearchDocument, SessionEventSearchHit, SessionEventSurface, SessionEventTrace,
    SessionEventTraceObservation, SessionEventTraceRequest, SessionEventWindow, SessionLineageNode,
    SessionLineageTrace, SessionLogSnapshot, SessionRecord, SessionResultBound,
    SessionResultFilter, SessionResultRange, SessionSearchHit, SessionSearchPage,
    SessionSurfaceSnapshot, SessionTitleObservation,
};
