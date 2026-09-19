//! Cross-session snapshot references and durable untrusted model context.

pub mod config;
pub mod index;
pub mod invariant;
pub mod projection;
pub mod serialization;
pub mod types;
pub mod uri;

pub use config::{
    Config, DEFAULT_CANDIDATE_LIMIT, DEFAULT_MAX_REFERENCE_BYTES, MAX_REFERENCES,
    SessionReferenceError, SessionReferenceErrorCode,
};
pub use index::{SessionReferenceResolver, config_schema, plugin};
pub use projection::{
    ReferenceRetentionStats, ReferencedSessionData, RetainedReferencedSession,
    retain_referenced_session,
};
pub use serialization::stringify_tag_safe_json;
pub use types::{
    PreparedReferencedMessage, ReferencedConversationItem, ReferencedConversationRole,
    SESSION_REFERENCE_SOURCE_KIND, SessionReferenceCandidate, SessionReferenceFact,
    SessionReferenceInput, SessionReferenceSource,
};
pub use uri::{
    ParsedSessionReferenceText, SESSION_REFERENCE_SCHEME, decode_session_reference_uri,
    encode_session_reference_uri, format_session_reference_mention, parse_session_reference_text,
};
