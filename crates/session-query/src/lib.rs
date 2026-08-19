//! Session-query domain: configuration, failure taxonomy, pagination cursor,
//! source-header compatibility, semantic text extraction, and the public record
//! vocabulary. The corpus and query service are ported separately.

pub mod config;
pub mod cursor;
pub mod extraction;
pub mod invariant;
pub mod sources;
pub mod types;

pub use config::{
    Config, SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY, SESSION_QUERY_READ_WINDOW_MAX,
    SessionQueryError, SessionQueryErrorCode,
};
pub use cursor::SessionSearchCursor;
pub use extraction::extract_session_event_text;
pub use sources::assert_session_headers_compatible;
pub use types::{
    SessionAvailability, SessionEventReadRequest, SessionEventRecord, SessionEventResultFilter,
    SessionEventSearchDocument, SessionEventSearchHit, SessionEventSurface, SessionEventTrace,
    SessionEventTraceObservation, SessionEventTraceRequest, SessionEventWindow, SessionLineageNode,
    SessionLineageTrace, SessionLogSnapshot, SessionRecord, SessionResultFilter,
    SessionResultRange, SessionSearchHit, SessionSearchPage, SessionSurfaceSnapshot,
    SessionTitleObservation,
};
