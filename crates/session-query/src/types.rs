//! Public records for exact reads and relationship traces over the
//! live-preferred logical session corpus.

use seekdeep_core::session::{SessionEvent, SessionHeader, SessionId};
use seekdeep_session_title::SessionTitleSnapshot;
use serde::{Deserialize, Serialize};

use crate::cursor::SessionSearchCursor;

/// The event type discriminant (merge-extensible, so an ordinary string).
pub type SessionEventType = String;

/// Whether an event is current model context, replaced context, or raw-log-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionEventSurface {
    /// Current model context.
    Current,
    /// Replaced context.
    Shadowed,
    /// Raw-log-only.
    LogOnly,
}

/// Lightweight identity and source availability for one logical session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    /// Cloned header selected from the live-preferred corpus.
    pub header: SessionHeader,
    /// Whether the id currently exists in ctx.sessions.
    pub live: bool,
    /// Whether the active persistence backend currently materializes the id.
    pub persisted: bool,
}

/// One atomic live-preferred observation of a session's current model surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSurfaceSnapshot {
    /// Cloned header from the same corpus observation as events.
    pub session: SessionHeader,
    /// Highest raw-log seq included, or none for an empty log.
    pub captured_through_seq: Option<u64>,
    /// Cloned current surface events in model-history order.
    pub events: Vec<SessionEvent>,
}

/// One validated detached observation of a session's complete raw log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogSnapshot {
    /// Cloned header from the same observation as events.
    pub session: SessionHeader,
    /// Cloned contiguous raw events after repair and replay validation.
    pub events: Vec<SessionEvent>,
}

/// Lightweight metadata for one event within a logical session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventRecord {
    /// Session that owns the event.
    pub session_id: SessionId,
    /// Monotonic event seq within the session.
    pub seq: u64,
    /// Discriminant of the session event.
    #[serde(rename = "type")]
    pub event_type: SessionEventType,
    /// Event timestamp in Unix epoch milliseconds.
    pub time: i64,
    /// Event placement in the folded session surface.
    pub surface: SessionEventSurface,
}

/// Recursive descendant node in a session-lineage trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLineageNode {
    /// Detached logical-corpus record for this descendant.
    pub session: SessionRecord,
    /// Direct children, each carrying its own recursive descendants.
    pub descendants: Vec<SessionLineageNode>,
}

/// Known ancestry and descendants for one logical session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLineageTrace {
    /// Detached record for the traced session.
    pub target: SessionRecord,
    /// Known parents from the immediate parent outward.
    pub ancestors: Vec<SessionRecord>,
    /// Complete known descendant trees.
    pub descendants: Vec<SessionLineageNode>,
    /// Whether the complete parent chain is present.
    pub complete: bool,
    /// Detached record at the top of a complete lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<SessionRecord>,
    /// First parent id not present in a partial lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unresolved_parent_id: Option<SessionId>,
}

/// Request for relationships to cited source events around one event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventTraceRequest {
    /// Session that owns the target event.
    pub session_id: SessionId,
    /// Target event seq.
    pub seq: u64,
}

/// Direct surface replacements and relationships for one event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventTrace {
    /// Lightweight target record.
    pub target: SessionEventRecord,
    /// Immediate positional replacement event, when shadowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<u64>,
    /// Positional replacers from the immediate to the final replacement.
    pub replacement_chain: Vec<u64>,
    /// Surface nodes directly removed by a target replacement.
    pub replaced_event_seqs: Vec<u64>,
    /// Earlier events cited directly as sources.
    pub source_event_seqs: Vec<u64>,
    /// Later events that directly cite the target as a source.
    pub derived_event_seqs: Vec<u64>,
}

/// Event relationships bound to the same session-header observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventTraceObservation {
    /// The trace.
    #[serde(flatten)]
    pub trace: SessionEventTrace,
    /// Cloned header selected with the event log used.
    pub session: SessionHeader,
}

/// Request for one event plus raw neighboring log context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventReadRequest {
    /// Session that owns the target event.
    pub session_id: SessionId,
    /// Target event seq.
    pub seq: u64,
    /// Number of preceding raw events to include.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<u64>,
    /// Number of following raw events to include.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
}

/// Full target event and a bounded raw-log window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventWindow {
    /// Cloned header for the live-preferred source read.
    pub session: SessionHeader,
    /// Full cloned target event.
    pub target: SessionEvent,
    /// Full cloned events from `start_seq` through `end_seq`.
    pub events: Vec<SessionEvent>,
    /// First seq included in events.
    pub start_seq: u64,
    /// Last seq included in events.
    pub end_seq: u64,
}

/// Latest folded title bound to the same session-header observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTitleObservation {
    /// Cloned header selected with the event log used.
    pub session: SessionHeader,
    /// Latest title snapshot, absent when the log has no title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<SessionTitleSnapshot>,
}

/// Inclusive numeric interval used by time and sequence filters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResultRange {
    /// Inclusive lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<u64>,
    /// Inclusive upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<u64>,
}

/// Source availability predicates understood by logical-session filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionAvailability {
    /// Live session.
    Live,
    /// Persisted session.
    Persisted,
}

/// One logical-session predicate (`AND`ed across clauses, `OR`ed within a clause).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum SessionResultFilter {
    /// Id clause.
    Id {
        /// Session ids.
        values: Vec<SessionId>,
    },
    /// Cwd clause.
    Cwd {
        /// Working directories; none means a null cwd.
        values: Vec<Option<String>>,
    },
    /// Created-at range clause.
    CreatedAt {
        /// Inclusive lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<u64>,
        /// Inclusive upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<u64>,
    },
    /// Parent clause.
    Parent {
        /// Parent ids; none means a root session.
        values: Vec<Option<SessionId>>,
    },
    /// Availability clause.
    Availability {
        /// Availability predicates.
        values: Vec<SessionAvailability>,
    },
}

/// One event predicate (`AND`ed across clauses, `OR`ed within a clause).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum SessionEventResultFilter {
    /// Seq range clause.
    Seq {
        /// Inclusive lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<u64>,
        /// Inclusive upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<u64>,
    },
    /// Time range clause.
    Time {
        /// Inclusive lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<u64>,
        /// Inclusive upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<u64>,
    },
    /// Type clause.
    Type {
        /// Event type discriminants.
        values: Vec<SessionEventType>,
    },
    /// Surface clause.
    Surface {
        /// Surface placements.
        values: Vec<SessionEventSurface>,
    },
    /// Literal semantic-text scan clause.
    Text {
        /// Case-insensitive, whitespace-flexible scan text.
        text: String,
    },
}

/// Searchable semantic document derived from one session event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventSearchDocument {
    /// The event record.
    #[serde(flatten)]
    pub record: SessionEventRecord,
    /// First-party semantic text used by scan filters and full-text indexes.
    pub text: String,
}

/// One cursor-paginated result page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchPage<T> {
    /// Results for this page in contract-defined order.
    pub items: Vec<T>,
    /// Opaque continuation cursor, absent on the final page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<SessionSearchCursor>,
}

/// One event full-text search hit with a bounded plain-text excerpt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventSearchHit {
    /// The event record.
    #[serde(flatten)]
    pub record: SessionEventRecord,
    /// Plain text excerpt selected around the match.
    pub snippet: String,
}

/// One grouped cross-session hit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchHit {
    /// The session record.
    #[serde(flatten)]
    pub record: SessionRecord,
    /// Strongest matching event for this session.
    pub best_match: SessionEventSearchHit,
}
