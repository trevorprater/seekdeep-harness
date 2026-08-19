//! Session-query domain: configuration, failure taxonomy, pagination cursor, and
//! source-header compatibility. The corpus and query service are ported separately.

pub mod config;
pub mod cursor;
pub mod sources;

pub use config::{
    Config, SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY, SESSION_QUERY_READ_WINDOW_MAX,
    SessionQueryError, SessionQueryErrorCode,
};
pub use cursor::SessionSearchCursor;
pub use sources::assert_session_headers_compatible;
