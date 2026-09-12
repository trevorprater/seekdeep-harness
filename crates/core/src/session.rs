//! Event-sourced sessions and canonical model-visible surface projection.

use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::{Mutex, ReentrantMutex};
pub use seekdeep_llm::SessionId;
use seekdeep_llm::{ContentBlock, Message, ModelId, ProviderId};
use seekdeep_lossless_json::JsonToken;
pub use seekdeep_lossless_json::{JsonRef, JsonValue};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;
use thiserror::Error;

use crate::request_header::{EpochHeader, fold_request_header};

/// Current on-disk session format version.
pub const SESSION_FORMAT_VERSION: u32 = 0;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Coarse durable origin classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionOrigin {
    /// Session was created as a child agent.
    Subagent,
}

/// Why an active agent driver was cancelled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AgentCancelCause {
    /// Direct user cancellation.
    User,
    /// Owning parent stopped the child.
    Parent,
    /// A lifecycle hook cancelled with a diagnostic reason.
    Hook {
        /// Hook-authored reason.
        reason: String,
    },
    /// Agent lifecycle teardown.
    Disposed,
}

/// Immutable storage metadata kept outside the conversation log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionHeader {
    /// Structural storage format version.
    pub version: u32,
    /// Session identity.
    pub id: SessionId,
    /// Unix epoch milliseconds at creation.
    pub created_at: u64,
    /// Absolute working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Fork parent identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<SessionId>,
    /// Count of inherited seed events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_length: Option<u64>,
    /// Product origin classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOrigin>,
    /// Persisted delegation depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_depth: Option<u64>,
    /// Composition preset used for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
}

impl SessionHeader {
    /// Creates minimal current-version metadata.
    #[must_use]
    pub fn new(id: SessionId) -> Self {
        Self::new_with_created_at(id, now_millis())
    }

    /// Creates minimal current-version metadata stamped at `created_at`.
    #[must_use]
    pub fn new_with_created_at(id: SessionId, created_at: u64) -> Self {
        Self {
            version: SESSION_FORMAT_VERSION,
            id,
            created_at,
            cwd: None,
            parent_session: None,
            seed_length: None,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }

    fn validate(&self, expected_id: &SessionId) -> Result<(), SessionError> {
        if self.version != SESSION_FORMAT_VERSION {
            return Err(SessionError::InvalidHeader(format!(
                "session header version must be {SESSION_FORMAT_VERSION}, got {}",
                self.version
            )));
        }
        if &self.id != expected_id {
            return Err(SessionError::InvalidHeader(format!(
                "session header id \"{}\" does not match session id \"{expected_id}\"",
                self.id
            )));
        }
        if self.created_at > MAX_SAFE_INTEGER {
            return Err(SessionError::InvalidHeader(
                "session header createdAt must be a non-negative safe integer".to_owned(),
            ));
        }
        if let Some(cwd) = &self.cwd
            && !Path::new(cwd).is_absolute()
        {
            return Err(SessionError::InvalidHeader(format!(
                "session header cwd must be an absolute path, got \"{cwd}\""
            )));
        }
        if self
            .seed_length
            .is_some_and(|value| value > MAX_SAFE_INTEGER)
        {
            return Err(SessionError::InvalidHeader(
                "session header seedLength must be a non-negative safe integer".to_owned(),
            ));
        }
        if self
            .delegation_depth
            .is_some_and(|value| value > MAX_SAFE_INTEGER)
        {
            return Err(SessionError::InvalidHeader(
                "session header delegationDepth must be a non-negative safe integer".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Positional replacement operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceReplace {
    /// Must be the literal `replace`.
    pub op: String,
    /// First current surface sequence replaced.
    pub start: u64,
    /// Last current surface sequence replaced.
    pub end: u64,
}

/// How a message-producing event joined the ordered surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SurfaceOp {
    /// Literal `append` marker.
    Marker(String),
    /// Inclusive positional replacement.
    Replace(SurfaceReplace),
}

/// One positional replacement observed while folding a surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceFoldReplacement {
    /// Sequence of the replacing event.
    pub seq: u64,
    /// Declared inclusive start sequence.
    pub start: u64,
    /// Declared inclusive end sequence.
    pub end: u64,
    /// Actual removed surface nodes in order.
    pub shadowed_seqs: Vec<u64>,
}

/// Complete result of replaying all surface operations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SurfaceFoldResult {
    /// Current model-visible event sequences.
    pub nodes: Vec<u64>,
    /// Replacement history in event order.
    pub replacements: Vec<SurfaceFoldReplacement>,
}

impl SurfaceOp {
    /// Canonical append marker.
    #[must_use]
    pub fn append() -> Self {
        Self::Marker("append".to_owned())
    }

    /// Canonical positional replacement.
    #[must_use]
    pub fn replace(start: u64, end: u64) -> Self {
        Self::Replace(SurfaceReplace {
            op: "replace".to_owned(),
            start,
            end,
        })
    }
}

/// One immutable durable log entry.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionEvent {
    /// Merge-extensible event type.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Contiguous sequence number.
    pub seq: u64,
    /// Unix epoch milliseconds.
    pub time: i64,
    /// Event-specific lossless JSON.
    pub data: JsonValue,
    /// Earlier event sequences cited as sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_seqs: Option<Vec<u64>>,
    /// Model-surface transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_op: Option<SurfaceOp>,
    /// True only when an older reader may safely skip the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignorable: Option<bool>,
}

impl<'de> Deserialize<'de> for SessionEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        const FIELDS: &[&str] = &[
            "type",
            "seq",
            "time",
            "data",
            "sourceEventSeqs",
            "surfaceOp",
            "ignorable",
        ];
        let value = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let entries = value
            .object_entries()
            .ok_or_else(|| D::Error::custom("session event must be an object"))?;
        for (key, _) in entries {
            let key = key.deserialize::<String>().map_err(D::Error::custom)?;
            if !FIELDS.contains(&key.as_str()) {
                return Err(D::Error::unknown_field(&key, FIELDS));
            }
        }
        let required =
            |name: &'static str| value.get(name).ok_or_else(|| D::Error::missing_field(name));
        Ok(Self {
            event_type: required("type")?.deserialize().map_err(D::Error::custom)?,
            seq: required("seq")?.deserialize().map_err(D::Error::custom)?,
            time: required("time")?.deserialize().map_err(D::Error::custom)?,
            data: required("data")?.to_owned(),
            source_event_seqs: value
                .get("sourceEventSeqs")
                .map(JsonRef::deserialize::<Option<Vec<u64>>>)
                .transpose()
                .map_err(D::Error::custom)?
                .flatten(),
            surface_op: value
                .get("surfaceOp")
                .map(|field| {
                    field
                        .deserialize::<Value>()
                        .and_then(serde_json::from_value::<Option<SurfaceOp>>)
                })
                .transpose()
                .map_err(D::Error::custom)?
                .flatten(),
            ignorable: value
                .get("ignorable")
                .map(JsonRef::deserialize::<Option<bool>>)
                .transpose()
                .map_err(D::Error::custom)?
                .flatten(),
        })
    }
}

/// Metadata accepted by an append operation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppendOptions {
    /// Required for message-producing events and forbidden otherwise.
    pub surface_op: Option<SurfaceOp>,
    /// Earlier source events.
    pub source_event_seqs: Option<Vec<u64>>,
    /// Marks a plugin event as semantically skippable by unknown readers.
    pub ignorable: bool,
}

/// Registration-bound metadata for one resolved model route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestContext {
    /// Registered provider route.
    pub provider: ProviderId,
    /// Provider-owned model id.
    pub model: ModelId,
    /// Advertised combined input/output token capacity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// Session construction or append rejection.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Storage metadata violates the current format.
    #[error("{0}")]
    InvalidHeader(String),
    /// Event envelope or surface transition is invalid.
    #[error("{0}")]
    InvalidEvent(String),
    /// Another append is inside its acceptance/publication section.
    #[error("session append cannot reenter while another append is being accepted")]
    ReentrantAppend,
}

#[derive(Clone, Debug, Default)]
struct SurfaceState {
    nodes: Vec<u64>,
    replace_generation: u64,
}

impl SurfaceState {
    fn apply(
        &mut self,
        event: &SessionEvent,
        log: &[SessionEvent],
    ) -> Result<Option<SurfaceFoldReplacement>, SessionError> {
        let expected_seq = u64::try_from(log.len()).expect("session log length fits u64");
        if event.seq != expected_seq {
            return Err(invalid(format!(
                "session event seq {} is not contiguous; expected {expected_seq}",
                event.seq
            )));
        }
        let eligible = is_surface_eligible_type(&event.event_type);
        if !eligible {
            if event.surface_op.is_some() {
                return Err(invalid(format!(
                    "session event \"{}\" is not surface-eligible and cannot carry surfaceOp",
                    event.event_type
                )));
            }
            if event.source_event_seqs.is_some() {
                return Err(invalid(format!(
                    "session event \"{}\" is not surface-eligible and cannot carry sourceEventSeqs",
                    event.event_type
                )));
            }
            return Ok(None);
        }

        let Some(operation) = &event.surface_op else {
            return Err(invalid(format!(
                "session event \"{}\" is surface-eligible and requires a surfaceOp marker",
                event.event_type
            )));
        };
        validate_sources(event, &[])?;
        match operation {
            SurfaceOp::Marker(marker) if marker == "append" => {
                self.nodes.push(event.seq);
                Ok(None)
            }
            SurfaceOp::Marker(_) => Err(invalid(format!(
                "session event \"{}\" carries an invalid surfaceOp",
                event.event_type
            ))),
            SurfaceOp::Replace(replacement) => {
                if replacement.op != "replace"
                    || replacement.start > MAX_SAFE_INTEGER
                    || replacement.end > MAX_SAFE_INTEGER
                {
                    return Err(invalid(format!(
                        "session event \"{}\" carries an invalid replace surfaceOp",
                        event.event_type
                    )));
                }
                let Some(start_index) = self.nodes.iter().position(|seq| *seq == replacement.start)
                else {
                    return Err(invalid(format!(
                        "surface replace: start seq {} not found in surface",
                        replacement.start
                    )));
                };
                let Some(end_index) = self.nodes.iter().position(|seq| *seq == replacement.end)
                else {
                    return Err(invalid(format!(
                        "surface replace: end seq {} not found in surface",
                        replacement.end
                    )));
                };
                if start_index > end_index {
                    return Err(invalid(format!(
                        "surface replace: start seq {} (index {start_index}) is after end seq {} (index {end_index})",
                        replacement.start, replacement.end
                    )));
                }
                let shadowed = self.nodes[start_index..=end_index].to_vec();
                validate_sources(event, &shadowed)?;
                validate_tool_result_rewrite(event, &shadowed, log)?;
                self.nodes.splice(start_index..=end_index, [event.seq]);
                self.replace_generation += 1;
                Ok(Some(SurfaceFoldReplacement {
                    seq: event.seq,
                    start: replacement.start,
                    end: replacement.end,
                    shadowed_seqs: shadowed,
                }))
            }
        }
    }
}

struct SessionInner {
    /// Shared so a snapshot is one reference count; an append copies the log only while a
    /// snapshot is still alive.
    log: Arc<Vec<SessionEvent>>,
    surface: SurfaceState,
    appending: bool,
}

/// Durable timestamp source for a session's header and appended events.
pub type SessionClock = Arc<dyn Fn() -> u64 + Send + Sync + 'static>;

/// An event-sourced append-only agent session.
pub struct Session {
    header: SessionHeader,
    clock: SessionClock,
    first_live_seq: u64,
    inner: Mutex<SessionInner>,
    append_gate: ReentrantMutex<()>,
    publisher: Mutex<Option<Weak<dyn SessionPublisher>>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("id", &self.header.id)
            .field("first_live_seq", &self.first_live_seq)
            .field("seq", &self.seq())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Creates a detached session and validates an optional durable seed.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] for invalid metadata, event envelopes, or surface transitions.
    pub fn create(
        id: &SessionId,
        seed: Option<Vec<SessionEvent>>,
        header: Option<SessionHeader>,
    ) -> Result<Arc<Self>, SessionError> {
        Self::create_with_clock(id, seed, header, Arc::new(now_millis))
    }

    /// Creates a detached session whose durable timestamps come from `clock`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] for invalid metadata, event envelopes, or surface transitions.
    pub fn create_with_clock(
        id: &SessionId,
        seed: Option<Vec<SessionEvent>>,
        header: Option<SessionHeader>,
        clock: SessionClock,
    ) -> Result<Arc<Self>, SessionError> {
        let header =
            header.unwrap_or_else(|| SessionHeader::new_with_created_at(id.clone(), clock()));
        header.validate(id)?;
        let first_live_seq = u64::try_from(seed.as_ref().map_or(0, Vec::len)).map_err(|_| {
            invalid("session seed length exceeds the supported event sequence range")
        })?;
        let mut inner = SessionInner {
            log: Arc::new(Vec::new()),
            surface: SurfaceState::default(),
            appending: false,
        };
        if let Some(seed) = seed {
            for event in seed {
                validate_envelope(&event, inner.log.len())?;
                let mut next_surface = inner.surface.clone();
                next_surface.apply(&event, &inner.log)?;
                Arc::make_mut(&mut inner.log).push(event);
                inner.surface = next_surface;
            }
            if inner
                .log
                .last()
                .is_none_or(|event| event.event_type != "session/end-seed")
            {
                let event = SessionEvent {
                    event_type: "session/end-seed".to_owned(),
                    seq: u64::try_from(inner.log.len()).map_err(|_| {
                        invalid("session log length exceeds the supported event sequence range")
                    })?,
                    time: i64::try_from(clock()).unwrap_or(i64::MAX),
                    data: Value::Object(serde_json::Map::new()).into(),
                    source_event_seqs: None,
                    surface_op: None,
                    ignorable: None,
                };
                let mut next_surface = inner.surface.clone();
                next_surface.apply(&event, &inner.log)?;
                Arc::make_mut(&mut inner.log).push(event);
                inner.surface = next_surface;
            }
        }
        Ok(Arc::new(Self {
            header,
            clock,
            first_live_seq,
            inner: Mutex::new(inner),
            append_gate: ReentrantMutex::new(()),
            publisher: Mutex::new(None),
        }))
    }

    /// Durable metadata.
    #[must_use]
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// Session identity.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.header.id
    }

    /// First sequence supplied by this process rather than its constructor seed.
    #[must_use]
    pub fn first_live_seq(&self) -> u64 {
        self.first_live_seq
    }

    /// Next event sequence.
    #[must_use]
    pub fn seq(&self) -> u64 {
        u64::try_from(self.inner.lock().log.len()).unwrap_or(u64::MAX)
    }

    /// Detached immutable snapshot of the append-only log.
    #[must_use]
    pub fn events(&self) -> Vec<SessionEvent> {
        (*self.inner.lock().log).clone()
    }

    /// Immutable snapshot of the append-only log that shares the live storage.
    ///
    /// Taking it costs one reference count; readers that run on every event (listeners,
    /// transport views) use this instead of [`Session::events`] so a long session is not
    /// copied per streamed chunk.
    #[must_use]
    pub fn events_shared(&self) -> Arc<Vec<SessionEvent>> {
        self.inner.lock().log.clone()
    }

    /// Number of committed events.
    #[must_use]
    pub fn events_len(&self) -> usize {
        self.inner.lock().log.len()
    }

    /// Current model-visible surface sequence numbers.
    #[must_use]
    pub fn surface_nodes(&self) -> Vec<u64> {
        self.inner.lock().surface.nodes.clone()
    }

    /// Monotonic number of committed positional replacements.
    #[must_use]
    pub fn replace_generation(&self) -> u64 {
        self.inner.lock().surface.replace_generation
    }

    /// Appends one event after validating its envelope and surface transition.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when JSON, envelope, provenance, or surface invariants fail.
    pub fn append(
        &self,
        event_type: impl Into<String>,
        data: Value,
        options: AppendOptions,
    ) -> Result<SessionEvent, SessionError> {
        self.append_json(event_type, data.into(), options)
    }

    /// Appends an event whose strings and keys may contain any UTF-16 code unit.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when envelope, provenance, or surface invariants fail.
    pub fn append_json(
        &self,
        event_type: impl Into<String>,
        data: JsonValue,
        options: AppendOptions,
    ) -> Result<SessionEvent, SessionError> {
        validate_lossless_json(&data)?;
        // A synchronous nested append on this thread remains an invariant
        // violation, while independent executor threads queue in commit order.
        // JavaScript serialized these callers on one event loop; the Rust
        // runtime must not turn harmless task interleaving into a false
        // reentrancy failure while publication temporarily releases `inner`.
        let _append_gate = self.append_gate.lock();
        let mut inner = self.inner.lock();
        if inner.appending {
            return Err(SessionError::ReentrantAppend);
        }
        inner.appending = true;
        let planned = (|| {
            let event = SessionEvent {
                event_type: event_type.into(),
                seq: u64::try_from(inner.log.len()).map_err(|_| {
                    invalid("session log length exceeds the supported event sequence range")
                })?,
                time: i64::try_from((self.clock)()).unwrap_or(i64::MAX),
                data,
                source_event_seqs: options.source_event_seqs,
                surface_op: options.surface_op,
                ignorable: options.ignorable.then_some(true),
            };
            validate_envelope(&event, inner.log.len())?;
            let mut next_surface = inner.surface.clone();
            next_surface.apply(&event, &inner.log)?;
            Ok((event, next_surface))
        })();
        let (event, next_surface) = match planned {
            Ok(planned) => planned,
            Err(error) => {
                inner.appending = false;
                return Err(error);
            }
        };
        let publisher = self.publisher.lock().as_ref().and_then(Weak::upgrade);
        drop(inner);
        let publication = match publisher {
            Some(publisher) => match publisher.prepare_publish(&event) {
                Ok(publication) => Some(publication),
                Err(error) => {
                    self.inner.lock().appending = false;
                    return Err(error);
                }
            },
            None => None,
        };
        let mut inner = self.inner.lock();
        Arc::make_mut(&mut inner.log).push(event.clone());
        inner.surface = next_surface;
        inner.appending = false;
        drop(inner);
        if let Some(publication) = publication {
            publication.publish();
        }
        Ok(event)
    }

    /// Projects the ordered surface into provider message values.
    #[must_use]
    pub fn derive_messages(&self) -> Vec<Message> {
        let inner = self.inner.lock();
        inner
            .surface
            .nodes
            .iter()
            .filter_map(|seq| inner.log.get(usize::try_from(*seq).ok()?))
            .filter_map(derive_event_message)
            .collect()
    }

    /// Returns the canonical request header in force after the latest snapshot.
    #[must_use]
    pub fn request_header(&self) -> Option<EpochHeader> {
        fold_request_header(&self.inner.lock().log, None)
    }

    /// Returns the latest resolved route metadata.
    #[must_use]
    pub fn request_context(&self) -> Option<RequestContext> {
        self.inner
            .lock()
            .log
            .iter()
            .rev()
            .find(|event| event.event_type == "request/context")
            .and_then(|event| event.data.deserialize().ok())
    }
}

pub(crate) trait SessionPublisher: Send + Sync {
    fn prepare_publish(
        self: Arc<Self>,
        event: &SessionEvent,
    ) -> Result<Box<dyn PreparedSessionPublication>, SessionError>;
}

pub(crate) trait PreparedSessionPublication {
    fn publish(self: Box<Self>);
}

impl Session {
    pub(crate) fn attach_publisher(
        &self,
        publisher: Weak<dyn SessionPublisher>,
    ) -> Result<(), SessionError> {
        let mut current = self.publisher.lock();
        if current.as_ref().and_then(Weak::upgrade).is_some() {
            return Err(invalid(format!(
                "session \"{}\" is already attached to a store",
                self.id()
            )));
        }
        *current = Some(publisher);
        Ok(())
    }

    pub(crate) fn detach_publisher(&self) {
        *self.publisher.lock() = None;
    }
}

/// Returns whether an event type may appear on the model-visible surface.
#[must_use]
pub fn is_surface_eligible_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "user/message" | "assistant/message" | "tool/result"
    )
}

/// Whether an event is surface-eligible and carries its required marker.
#[must_use]
pub fn is_surface_event(event: &SessionEvent) -> bool {
    is_surface_eligible_type(&event.event_type) && event.surface_op.is_some()
}

/// Whether an event entered the surface at its own log position.
#[must_use]
pub fn is_append_surface_event(event: &SessionEvent) -> bool {
    matches!(&event.surface_op, Some(SurfaceOp::Marker(marker)) if marker == "append")
        && is_surface_eligible_type(&event.event_type)
}

/// Whether an event replaced a current surface range.
#[must_use]
pub fn is_replacement_surface_event(event: &SessionEvent) -> bool {
    matches!(event.surface_op, Some(SurfaceOp::Replace(_)))
        && is_surface_eligible_type(&event.event_type)
}

/// Replays surface operations over a complete log.
///
/// # Errors
///
/// Returns [`SessionError`] at the first invalid envelope or surface transition.
pub fn fold_surface(events: &[SessionEvent]) -> Result<SurfaceFoldResult, SessionError> {
    let mut state = SurfaceState::default();
    let mut accepted = Vec::with_capacity(events.len());
    let mut replacements = Vec::new();
    for event in events {
        if let Some(replacement) = state.apply(event, &accepted)? {
            replacements.push(replacement);
        }
        accepted.push(event.clone());
    }
    Ok(SurfaceFoldResult {
        nodes: state.nodes,
        replacements,
    })
}

fn validate_envelope(event: &SessionEvent, index: usize) -> Result<(), SessionError> {
    if event.event_type == "request/header-delta" {
        return Err(invalid(format!(
            "seed event at index {index} uses unsupported legacy request/header-delta format"
        )));
    }
    if event.seq != u64::try_from(index).unwrap_or(u64::MAX) {
        return Err(invalid(format!(
            "seed event at index {index} has seq {} (expected {index}); seed must be contiguous from 0",
            event.seq
        )));
    }
    if event.seq > MAX_SAFE_INTEGER || event.time.unsigned_abs() > MAX_SAFE_INTEGER {
        return Err(invalid(format!(
            "seed event at index {index} has an invalid event envelope"
        )));
    }
    if event.ignorable.is_some_and(|value| !value) {
        return Err(invalid(format!(
            "seed event at index {index} has an invalid event envelope"
        )));
    }
    validate_lossless_json(&event.data)?;
    validate_request_header(event, index)?;
    validate_message_event(
        event,
        &format!("seed {} at index {index}", event.event_type),
    )?;
    Ok(())
}

fn validate_request_header(event: &SessionEvent, index: usize) -> Result<(), SessionError> {
    if event.event_type != "request/header" {
        return Ok(());
    }
    if event
        .data
        .get("reason")
        .and_then(|value| value.deserialize::<String>().ok())
        .as_deref()
        == Some("fallback")
    {
        return Err(invalid(format!(
            "seed event at index {index} uses unsupported legacy request/header reason \"fallback\""
        )));
    }
    let header = event
        .data
        .get("header")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            invalid(format!(
                "seed request/header at index {index} lacks provider/model"
            ))
        })?;
    let config = header
        .get("config")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            invalid(format!(
                "seed request/header at index {index} lacks provider/model"
            ))
        })?;
    if ["provider", "model"].iter().any(|name| {
        config
            .get(name)
            .and_then(JsonRef::to_utf16)
            .is_none_or(|value| value.is_empty())
    }) {
        return Err(invalid(format!(
            "seed request/header at index {index} lacks provider/model"
        )));
    }
    if config
        .get("reasoningEffort")
        .is_some_and(|value| value.to_utf16().is_none_or(|value| value.is_empty()))
    {
        return Err(invalid(format!(
            "seed request/header at index {index} has an invalid reasoningEffort"
        )));
    }
    if let Some(defaults) = header.get("adapterDefaults") {
        let invalid_marker = defaults.object_entries().is_none_or(|defaults| {
            defaults.iter().any(|(name, marker)| {
                let name = name.deserialize::<String>().ok();
                !matches!(name.as_deref(), Some("reasoningEffort" | "maxTokens"))
                    || marker.as_bool() != Some(true)
                    || name.is_none_or(|name| config.get(&name).is_none())
            })
        });
        if invalid_marker {
            return Err(invalid(format!(
                "seed request/header at index {index} has invalid adapterDefaults"
            )));
        }
    }
    Ok(())
}

enum JsonValidationFrame {
    Array(Option<&'static str>),
    Object {
        key: Option<Vec<u16>>,
        fields: indexmap::IndexMap<Vec<u16>, Option<&'static str>>,
    },
}

fn validate_lossless_json(value: &JsonValue) -> Result<(), SessionError> {
    let mut frames = Vec::new();
    for token in value.tokens() {
        let error = match token {
            JsonToken::ArrayStart => {
                frames.push(JsonValidationFrame::Array(None));
                continue;
            }
            JsonToken::ObjectStart => {
                frames.push(JsonValidationFrame::Object {
                    key: None,
                    fields: indexmap::IndexMap::new(),
                });
                continue;
            }
            JsonToken::Key(value) => {
                let Some(JsonValidationFrame::Object { key, .. }) = frames.last_mut() else {
                    unreachable!("validated object keys have an owning object")
                };
                *key = value.to_utf16();
                continue;
            }
            JsonToken::Scalar(value) => json_number_error(value),
            JsonToken::ArrayEnd | JsonToken::ObjectEnd => {
                match frames
                    .pop()
                    .expect("validated JSON containers are balanced")
                {
                    JsonValidationFrame::Array(error) => error,
                    JsonValidationFrame::Object { fields, .. } => {
                        fields.into_values().find_map(|error| error)
                    }
                }
            }
        };
        match frames.last_mut() {
            Some(JsonValidationFrame::Array(first_error)) => {
                *first_error = first_error.or(error);
            }
            Some(JsonValidationFrame::Object { key, fields }) => {
                fields.insert(
                    key.take().expect("validated object values have keys"),
                    error,
                );
            }
            None => return error.map_or(Ok(()), |error| Err(invalid(error))),
        }
    }
    Ok(())
}

fn json_number_error(value: JsonRef<'_>) -> Option<&'static str> {
    if !matches!(value.as_raw().as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
        return None;
    }
    let Some(number) = value.as_f64().filter(|number| number.is_finite()) else {
        return Some("session data contains a non-finite number");
    };
    (number == 0.0 && number.is_sign_negative()).then_some("session data contains negative zero")
}

fn validate_sources(event: &SessionEvent, shadowed: &[u64]) -> Result<(), SessionError> {
    let mut sources = HashSet::new();
    if let Some(raw) = &event.source_event_seqs {
        if raw.is_empty() && event.event_type != "assistant/message" {
            return Err(invalid(
                "sourceEventSeqs must not be empty except on assistant/message",
            ));
        }
        for source in raw {
            if *source > MAX_SAFE_INTEGER {
                return Err(invalid(format!(
                    "session event \"{}\" sourceEventSeqs must densely contain non-negative safe integers",
                    event.event_type
                )));
            }
            if !sources.insert(*source) {
                return Err(invalid("sourceEventSeqs must not contain duplicates"));
            }
            if *source >= event.seq {
                return Err(invalid(format!(
                    "sourceEventSeqs must reference earlier events: {source} >= current seq {}",
                    event.seq
                )));
            }
        }
    }
    let missing = shadowed
        .iter()
        .filter(|seq| !sources.contains(seq))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(invalid(format!(
            "surface replace: sourceEventSeqs must include every shadowed surface node; missing {}",
            missing
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
}

fn validate_tool_result_rewrite(
    event: &SessionEvent,
    shadowed: &[u64],
    log: &[SessionEvent],
) -> Result<(), SessionError> {
    if event.event_type != "tool/result" {
        return Ok(());
    }
    if shadowed.len() != 1 {
        return Err(invalid(
            "tool/result surface replacement must rewrite exactly one current node",
        ));
    }
    let original = log
        .get(usize::try_from(shadowed[0]).expect("surface seq fits usize"))
        .filter(|event| event.event_type == "tool/result")
        .ok_or_else(|| {
            invalid("tool/result surface replacement must target a current tool/result")
        })?;
    let original_data = erase_tool_result_text(&original.data)?;
    let replacement_data = erase_tool_result_text(&event.data)?;
    if original_data == replacement_data {
        Ok(())
    } else {
        Err(invalid(
            "tool/result surface replacement may change only content",
        ))
    }
}

fn erase_tool_result_text(value: &JsonValue) -> Result<JsonValue, SessionError> {
    replace_json_member(value.as_ref(), &["message", "content", "0", "content"])
}

fn replace_json_member(value: JsonRef<'_>, path: &[&str]) -> Result<JsonValue, SessionError> {
    let Some((field, rest)) = path.split_first() else {
        return Ok(Value::Null.into());
    };
    if let Some(items) = value.array_items() {
        let index: usize = field
            .parse()
            .map_err(|_| invalid("tool/result carries an invalid message"))?;
        if index >= items.len() {
            return Err(invalid("tool/result carries an invalid message"));
        }
        let items = items
            .into_iter()
            .enumerate()
            .map(|(position, item)| {
                if position == index {
                    replace_json_member(item, rest)
                } else {
                    Ok(item.to_owned())
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(JsonValue::array(&items));
    }
    let entries = value
        .object_entries()
        .ok_or_else(|| invalid("tool/result carries an invalid message"))?;
    let mut found = false;
    let mut encoded = String::from("{");
    for (index, (key, item)) in entries.into_iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(key.as_raw());
        encoded.push(':');
        if key
            .to_utf16()
            .is_some_and(|units| units.iter().copied().eq(field.encode_utf16()))
        {
            found = true;
            encoded.push_str(replace_json_member(item, rest)?.as_raw());
        } else {
            encoded.push_str(item.as_raw());
        }
    }
    if !found {
        return Err(invalid("tool/result carries an invalid message"));
    }
    encoded.push('}');
    JsonValue::parse(encoded).map_err(|error| invalid(error.to_string()))
}

fn validate_message_event(event: &SessionEvent, subject: &str) -> Result<(), SessionError> {
    if !is_surface_eligible_type(&event.event_type) {
        return Ok(());
    }
    let message_value = if event.event_type == "user/message" {
        event.data.as_ref()
    } else {
        event
            .data
            .get("message")
            .ok_or_else(|| invalid(format!("{subject} lacks an identified message")))?
    };
    if message_value
        .get("id")
        .and_then(JsonRef::to_utf16)
        .is_none_or(|value| value.is_empty())
    {
        return Err(invalid(format!("{subject} lacks an identified message")));
    }
    let role = if event.event_type == "assistant/message" {
        "assistant"
    } else {
        "user"
    };
    if message_value
        .get("role")
        .and_then(|value| value.deserialize::<String>().ok())
        .as_deref()
        != Some(role)
    {
        return Err(invalid(format!(
            "{subject} message must have role \"{role}\""
        )));
    }
    if message_value
        .get("source")
        .and_then(|value| value.get("kind"))
        .and_then(JsonRef::to_utf16)
        .is_none_or(|value| value.is_empty())
    {
        return Err(invalid(format!("{subject} message has invalid source")));
    }
    if message_value
        .get("content")
        .is_none_or(|value| !value.is_array())
    {
        return Err(invalid(format!("{subject} message has invalid content")));
    }
    let message: Message = message_value
        .deserialize()
        .map_err(|_| invalid(format!("{subject} lacks an identified message")))?;
    if message.source().kind.is_empty() {
        return Err(invalid(format!("{subject} message has invalid source")));
    }
    if event.event_type == "assistant/message" {
        let provider = message
            .source()
            .fields
            .get("provider")
            .and_then(JsonValue::as_str);
        let model = message
            .source()
            .fields
            .get("model")
            .and_then(JsonValue::as_str);
        if message.source().kind != "model"
            || provider.is_none_or(str::is_empty)
            || model.is_none_or(str::is_empty)
        {
            return Err(invalid(format!("{subject} message must have model source")));
        }
    }
    if event.event_type == "tool/result" {
        let source_call_id = message
            .source()
            .fields
            .get("callId")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty());
        if message.source().kind != "tool" || source_call_id.is_none() {
            return Err(invalid(format!("{subject} message must have tool source")));
        }
        let [
            ContentBlock::ToolResult {
                tool_call_id,
                content: _,
                is_error: _,
            },
        ] = message.content()
        else {
            return Err(invalid(format!(
                "{subject} message must contain one tool-result block"
            )));
        };
        if Some(tool_call_id.as_str()) != source_call_id {
            return Err(invalid(format!(
                "{subject} message has mismatched tool call ids"
            )));
        }
    }
    Ok(())
}

/// Projects one message-producing event into provider history.
#[must_use]
pub fn derive_event_message(event: &SessionEvent) -> Option<Message> {
    match event.event_type.as_str() {
        "user/message" => event.data.deserialize().ok(),
        "assistant/message" => {
            let message = event.data.get("message")?;
            let message: Message = message.deserialize().ok()?;
            if message.content().is_empty() {
                None
            } else {
                Some(message)
            }
        }
        "tool/result" => event.data.get("message")?.deserialize().ok(),
        _ => None,
    }
}

fn invalid(message: impl Into<String>) -> SessionError {
    SessionError::InvalidEvent(message.into())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use seekdeep_llm::MessageSource;
    use serde_json::json;

    use super::*;

    fn message(event_type: &str, seq: u64) -> SessionEvent {
        let message = if event_type == "user/message" {
            Message::user(Vec::new(), seekdeep_llm::MessageSource::user())
        } else {
            Message::assistant(
                vec![ContentBlock::Text { text: "x".into() }],
                "mock",
                "mock",
            )
        };
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: i64::try_from(seq).expect("test seq fits i64"),
            data: if event_type == "user/message" {
                serde_json::to_value(message)
                    .expect("serialize message")
                    .into()
            } else {
                json!({"message": message}).into()
            },
            source_event_seqs: None,
            surface_op: Some(SurfaceOp::append()),
            ignorable: None,
        }
    }

    #[test]
    fn code_dispatch_payloads_and_tool_text_survive_durable_event_reconstruction() {
        let session = Session::create(&SessionId::new("lossless-code"), None, None).unwrap();
        let dispatch = JsonValue::parse(r#"{"rootCallId":"lossless-call","parentCallId":"lossless-call","subCallId":"lossless-call:code:1","name":"echo","arguments":{"payload":{"\ud800":["\udfff","😀","\\ud800"]}}}"#.to_owned()).unwrap();
        let started = session
            .append_json(
                "tool/code-dispatch-start",
                dispatch.clone(),
                AppendOptions::default(),
            )
            .unwrap();
        assert_eq!(started.data.as_raw(), dispatch.as_raw());
        let tool_result = JsonValue::parse(r#"{"message":{"source":{"kind":"tool","callId":"lossless-call"},"content":[{"type":"tool-result","toolCallId":"lossless-call","content":[{"type":"text","text":"\ud800"}]}],"role":"user","id":"lossless-result"}}"#.to_owned()).unwrap();
        session
            .append_json(
                "tool/result",
                tool_result,
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .unwrap();

        let encoded = serde_json::to_string(&session.events()).unwrap();
        assert!(!encoded.contains('\u{fffd}'));
        let decoded: Vec<SessionEvent> = serde_json::from_str(&encoded).unwrap();
        let restored = Session::create(session.id(), Some(decoded), None).unwrap();
        assert_eq!(restored.events()[0].data, dispatch);
        let keys = restored.events()[0]
            .data
            .pointer("/arguments/payload")
            .unwrap()
            .object_entries()
            .unwrap()
            .into_iter()
            .map(|(key, _)| key.to_utf16().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(keys, [vec![0xd800]]);
        assert_eq!(restored.derive_messages(), session.derive_messages());
        let messages = JsonValue::from_serialize(&restored.derive_messages()).unwrap();
        let text = messages.pointer("/0/content/0/content/0/text").unwrap();
        assert_eq!(text.to_utf16().unwrap(), [0xd800]);
        assert_eq!(text.as_raw(), r#""\ud800""#);
    }

    #[test]
    fn tool_result_surface_rewrites_compare_all_raw_metadata() {
        let session = Session::create(&SessionId::new("lossless-rewrite"), None, None).unwrap();
        let original = r#"{"metadata":{"\ud800":"\udfff"},"message":{"source":{"kind":"tool","callId":"call"},"content":[{"type":"tool-result","toolCallId":"call","content":[{"type":"text","text":"\ud800"}]}],"role":"user","id":"result"}}"#;
        session
            .append_json(
                "tool/result",
                JsonValue::parse(original.to_owned()).unwrap(),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .unwrap();
        let replacement = original.replace(r#""text":"\ud800""#, r#""text":"\udc00""#);
        session
            .append_json(
                "tool/result",
                JsonValue::parse(replacement.clone()).unwrap(),
                AppendOptions {
                    surface_op: Some(SurfaceOp::replace(0, 0)),
                    source_event_seqs: Some(vec![0]),
                    ..AppendOptions::default()
                },
            )
            .unwrap();
        let corrupted = replacement.replace(r#""\ud800":"\udfff""#, r#""\ud800":"\udffe""#);
        let failure = session
            .append_json(
                "tool/result",
                JsonValue::parse(corrupted).unwrap(),
                AppendOptions {
                    surface_op: Some(SurfaceOp::replace(1, 1)),
                    source_event_seqs: Some(vec![1]),
                    ..AppendOptions::default()
                },
            )
            .unwrap_err();
        assert!(failure.to_string().contains("may change only content"));
        let messages = JsonValue::from_serialize(&session.derive_messages()).unwrap();
        assert_eq!(
            messages
                .pointer("/0/content/0/content/0/text")
                .unwrap()
                .to_utf16()
                .unwrap(),
            [0xdc00]
        );
    }

    #[test]
    fn raw_append_retains_the_negative_zero_validation() {
        let session = Session::create(&SessionId::new("lossless-numbers"), None, None).unwrap();
        assert!(
            session
                .append(
                    "tool/code-dispatch-start",
                    json!({"value": -0.0}),
                    AppendOptions::default(),
                )
                .unwrap_err()
                .to_string()
                .contains("negative zero")
        );
        for raw in [r#"{"\ud800":-0}"#, r#"{"value":[-0.0]}"#] {
            assert!(
                session
                    .append_json(
                        "tool/code-dispatch-start",
                        JsonValue::parse(raw.to_owned()).unwrap(),
                        AppendOptions::default(),
                    )
                    .unwrap_err()
                    .to_string()
                    .contains("negative zero")
            );
        }
        assert!(
            session
                .append_json(
                    "tool/code-dispatch-start",
                    JsonValue::parse(r#"{"value":1e9999}"#.to_owned()).unwrap(),
                    AppendOptions::default(),
                )
                .unwrap_err()
                .to_string()
                .contains("non-finite number")
        );
    }

    #[test]
    fn raw_append_validation_observes_last_duplicate_properties() {
        let session = Session::create(&SessionId::new("duplicate-properties"), None, None).unwrap();
        for raw in [
            r#"{"value":-0,"value":1}"#,
            r#"{"\ud800":[-0],"\uD800":[1]}"#,
            r#"{"nested":{"x":1e999},"nested":{"x":"\ud800"}}"#,
        ] {
            session
                .append_json(
                    "tool/code-dispatch-start",
                    JsonValue::parse(raw.to_owned()).unwrap(),
                    AppendOptions::default(),
                )
                .unwrap();
        }
        for raw in [
            r#"{"value":1,"value":-0}"#,
            r#"{"\ud800":[1],"\uD800":[-0]}"#,
            r#"{"nested":{"x":"\ud800"},"nested":{"x":1e999}}"#,
        ] {
            assert!(
                session
                    .append_json(
                        "tool/code-dispatch-start",
                        JsonValue::parse(raw.to_owned()).unwrap(),
                        AppendOptions::default()
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn stored_envelopes_keep_last_duplicate_fields_and_exact_data() {
        let raw = r#"{"type":false,"type":"tool/code-dispatch-start","seq":99,"seq":0,"time":false,"time":1,"data":{"discarded":"\ud800"},"d\u0061ta":{"payload":{"\ud800":"\udfff"}},"ignorable":false,"ignorable":true}"#;
        let event: SessionEvent = serde_json::from_str(raw).unwrap();
        assert_eq!(event.event_type, "tool/code-dispatch-start");
        assert_eq!(event.seq, 0);
        assert_eq!(event.time, 1);
        assert_eq!(event.ignorable, Some(true));
        assert_eq!(event.data.as_raw(), r#"{"payload":{"\ud800":"\udfff"}}"#);
        let session = Session::create(
            &SessionId::new("duplicate-envelope"),
            Some(vec![event]),
            None,
        )
        .unwrap();
        let stored = JsonValue::from_serialize(&session.events()[0]).unwrap();
        assert_eq!(
            stored.get("data").unwrap().as_raw(),
            r#"{"payload":{"\ud800":"\udfff"}}"#
        );

        let metadata: SessionEvent = serde_json::from_str(r#"{"type":"user/message","seq":0,"time":1,"data":{},"surfaceOp":{"op":"replace","start":9,"start":0,"end":0},"sourceEventSeqs":[9],"sourceEventSeqs":[0]}"#).unwrap();
        assert_eq!(metadata.surface_op, Some(SurfaceOp::replace(0, 0)));
        assert_eq!(metadata.source_event_seqs, Some(vec![0]));
    }

    #[test]
    fn seeded_sessions_validate_contiguity_and_mark_seed_end() {
        let session = Session::create(
            &SessionId::new("s"),
            Some(vec![message("user/message", 0)]),
            None,
        )
        .expect("valid seed");
        assert_eq!(session.first_live_seq(), 1);
        assert_eq!(session.seq(), 2);
        assert_eq!(session.events()[1].event_type, "session/end-seed");
    }

    #[test]
    fn replacement_requires_complete_provenance() {
        let events = vec![
            message("user/message", 0),
            message("user/message", 1),
            SessionEvent {
                source_event_seqs: Some(vec![0]),
                surface_op: Some(SurfaceOp::replace(0, 1)),
                ..message("user/message", 2)
            },
        ];
        assert!(fold_surface(&events).is_err_and(|error| error.to_string().contains("missing 1")));
    }

    #[test]
    fn surface_replacement_is_atomic() {
        let session = Session::create(&SessionId::new("s"), None, None).expect("new session");
        session
            .append(
                "user/message",
                serde_json::to_value(Message::user(
                    Vec::new(),
                    seekdeep_llm::MessageSource::user(),
                ))
                .expect("serialize message"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("append");
        let before = session.events();
        let result = session.append(
            "user/message",
            serde_json::to_value(Message::user(
                Vec::new(),
                seekdeep_llm::MessageSource::user(),
            ))
            .expect("serialize message"),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(0, 0)),
                source_event_seqs: Some(vec![]),
                ..AppendOptions::default()
            },
        );
        assert!(result.is_err());
        assert_eq!(session.events(), before);
    }

    fn turn_start_event(seq: u64) -> SessionEvent {
        SessionEvent {
            event_type: "turn/start".to_owned(),
            seq,
            time: 1,
            data: json!({"turn": 1}).into(),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    fn context_event(
        seq: u64,
        provider: &str,
        model: &str,
        context_window: Option<u64>,
    ) -> SessionEvent {
        let mut data = serde_json::Map::new();
        data.insert("provider".to_owned(), json!(provider));
        data.insert("model".to_owned(), json!(model));
        if let Some(window) = context_window {
            data.insert("contextWindow".to_owned(), json!(window));
        }
        SessionEvent {
            event_type: "request/context".to_owned(),
            seq,
            time: 1,
            data: Value::Object(data).into(),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    fn capacity_session(id: &str, records: &[(&str, &str, Option<u64>)]) -> Arc<Session> {
        let mut seed = vec![turn_start_event(0)];
        for (index, (provider, model, window)) in records.iter().enumerate() {
            seed.push(context_event(
                u64::try_from(index + 1).expect("seq"),
                provider,
                model,
                *window,
            ));
        }
        Session::create(&SessionId::new(id), Some(seed), None).expect("seeded capacity session")
    }

    #[test]
    fn request_context_is_none_before_any_record_exists() {
        let session = Session::create(&SessionId::new("no-capacity"), None, None).expect("new");
        assert_eq!(session.request_context(), None);
    }

    #[test]
    fn request_context_folds_a_seeded_log_taking_the_last_record() {
        let session = capacity_session(
            "seeded-capacity",
            &[
                ("mock", "m", Some(128_000)),
                ("mock", "later", Some(256_000)),
            ],
        );
        assert_eq!(
            session.request_context(),
            Some(RequestContext {
                provider: ProviderId::new("mock"),
                model: ModelId::new("later"),
                context_window: Some(256_000),
            })
        );
    }

    #[test]
    fn request_context_advances_incrementally_and_skips_unrelated_events() {
        let session = capacity_session("incremental-capacity", &[("mock", "m", Some(128_000))]);
        assert_eq!(
            session.request_context(),
            Some(RequestContext {
                provider: ProviderId::new("mock"),
                model: ModelId::new("m"),
                context_window: Some(128_000),
            })
        );
        session
            .append("todo/write", json!({"todos": []}), AppendOptions::default())
            .expect("unrelated append");
        assert_eq!(
            session.request_context().as_ref().map(|c| c.model.clone()),
            Some(ModelId::new("m"))
        );
        session
            .append(
                "request/context",
                json!({"provider": "mock", "model": "next", "contextWindow": 64_000}),
                AppendOptions::default(),
            )
            .expect("capacity append");
        assert_eq!(
            session.request_context(),
            Some(RequestContext {
                provider: ProviderId::new("mock"),
                model: ModelId::new("next"),
                context_window: Some(64_000),
            })
        );
        session
            .append(
                "request/context",
                json!({"provider": "mock", "model": "unknown"}),
                AppendOptions::default(),
            )
            .expect("capacity append without window");
        assert_eq!(
            session.request_context(),
            Some(RequestContext {
                provider: ProviderId::new("mock"),
                model: ModelId::new("unknown"),
                context_window: None,
            })
        );
    }

    #[test]
    fn request_context_folds_a_batch_appended_between_reads() {
        let session = capacity_session("batched-capacity", &[("mock", "m", Some(128_000))]);
        assert_eq!(
            session.request_context().as_ref().map(|c| c.context_window),
            Some(Some(128_000))
        );
        session
            .append(
                "request/context",
                json!({"provider": "mock", "model": "m", "contextWindow": 200_000}),
                AppendOptions::default(),
            )
            .expect("capacity append");
        session
            .append("todo/write", json!({"todos": []}), AppendOptions::default())
            .expect("unrelated append");
        session
            .append(
                "request/context",
                json!({"provider": "mock", "model": "m", "contextWindow": 300_000}),
                AppendOptions::default(),
            )
            .expect("capacity append");
        assert_eq!(
            session.request_context().as_ref().map(|c| c.context_window),
            Some(Some(300_000))
        );
    }

    #[test]
    fn request_context_returns_a_detached_record() {
        let session = capacity_session("frozen-capacity", &[("mock", "m", Some(128_000))]);
        let held = session.request_context().expect("folded capacity record");
        // Mutating the returned owned clone cannot desync the session's state.
        let mut mutated = held.clone();
        mutated.context_window = Some(1);
        assert_eq!(
            session.request_context().as_ref().map(|c| c.context_window),
            Some(Some(128_000))
        );
    }

    fn user_text(session: &Session, text: &str) -> SessionEvent {
        let message = Message::user(
            vec![ContentBlock::Text { text: text.into() }],
            MessageSource::user(),
        );
        session
            .append(
                "user/message",
                serde_json::to_value(message).expect("serialize user message"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("user message")
    }

    fn assistant_text(session: &Session, text: &str, step: i64) -> SessionEvent {
        let message = Message::assistant(
            vec![ContentBlock::Text { text: text.into() }],
            "mock",
            "mock",
        );
        session
            .append(
                "assistant/message",
                json!({"turn": 1, "step": step, "message": message}),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("assistant message")
    }

    fn scratch(session: &Arc<Session>) -> Vec<Message> {
        Session::create(
            &SessionId::new(format!(
                "{}-scratch-{}",
                session.id().as_str(),
                session.seq()
            )),
            Some(session.events()),
            None,
        )
        .expect("scratch session")
        .derive_messages()
    }

    #[test]
    fn derive_messages_stays_value_equal_to_scratch_replay_as_the_log_grows() {
        let session = Session::create(&SessionId::new("cache-grow"), None, None).expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
        user_text(&session, "one");
        assert_eq!(session.derive_messages(), scratch(&session));

        user_text(&session, "two");
        assistant_text(&session, "reply", 1);
        assert_eq!(session.derive_messages(), scratch(&session));

        session
            .append(
                "assistant/message",
                json!({"turn": 1, "step": 2, "message": Message::assistant(vec![], "mock", "mock")}),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("empty assistant");
        assert_eq!(session.derive_messages(), scratch(&session));
    }

    #[test]
    fn derive_messages_rebuilds_on_a_surface_replace() {
        let session =
            Session::create(&SessionId::new("cache-replace"), None, None).expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
        user_text(&session, "one");
        user_text(&session, "two");
        let before_replace = session.derive_messages();
        assert_eq!(before_replace.len(), 2);

        let nodes = session.surface_nodes();
        let summary = Message::user(
            vec![ContentBlock::Text {
                text: "summary".into(),
            }],
            MessageSource::plugin("compact"),
        );
        session
            .append(
                "user/message",
                serde_json::to_value(summary).expect("serialize summary"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::replace(nodes[0], nodes[1])),
                    source_event_seqs: Some(nodes.clone()),
                    ..AppendOptions::default()
                },
            )
            .expect("replace");

        assert_eq!(session.derive_messages().len(), 1);
        assert_eq!(session.derive_messages(), scratch(&session));
        assert_eq!(before_replace.len(), 2);
    }

    #[test]
    fn derive_messages_returns_a_fresh_array_per_call() {
        let session =
            Session::create(&SessionId::new("cache-snapshot"), None, None).expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
        user_text(&session, "one");
        let first = session.derive_messages();
        user_text(&session, "two");
        let second = session.derive_messages();
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 2);
    }

    #[test]
    fn derive_event_message_matches_the_full_derivation_projection() {
        let session = Session::create(&SessionId::new("per-event"), None, None).expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
        let event = user_text(&session, "hi");
        let projected = derive_event_message(&event).expect("projected");
        let full = session.derive_messages();
        assert_eq!(projected, full[full.len() - 1].clone());
    }

    #[test]
    fn derive_event_message_projects_none_for_boundaries_and_empty_assistant() {
        let session =
            Session::create(&SessionId::new("per-event-null"), None, None).expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
        let boundary = session
            .append(
                "step/start",
                json!({"turn": 1, "step": 1}),
                AppendOptions::default(),
            )
            .expect("step start");
        assert!(derive_event_message(&boundary).is_none());

        let empty = session
            .append(
                "assistant/message",
                json!({"turn": 1, "step": 1, "message": Message::assistant(vec![], "mock", "mock")}),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("empty assistant");
        assert!(derive_event_message(&empty).is_none());
    }
}
