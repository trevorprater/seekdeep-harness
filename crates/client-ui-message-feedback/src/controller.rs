//! Target-portable per-Session feedback controller.

use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

use futures::{
    FutureExt as _,
    future::{LocalBoxFuture, Shared},
    lock::Mutex,
};
use indexmap::IndexMap;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{DeserializeOwned, Error as _},
    ser::SerializeMap as _,
};
use serde_json::{Map as JsonMap, Value as JsonValue};

/// Browser-side Session identity crossing the Remote boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeedbackSessionId(String);

impl FeedbackSessionId {
    /// Creates an identity with its exact wire spelling.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Browser-side assistant-message identity crossing the Remote boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeedbackMessageId(String);

impl FeedbackMessageId {
    /// Creates an identity with its exact wire spelling.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Closed rating vocabulary crossing the browser Remote boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageFeedbackRating {
    /// Helpful response.
    Positive,
    /// Problematic response.
    Negative,
}

/// Opaque compare-and-set version.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageFeedbackVersion(pub String);

/// One current item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFeedbackItem {
    /// Target assistant message.
    pub message_id: FeedbackMessageId,
    /// Judgment.
    pub rating: MessageFeedbackRating,
    /// Optional explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// CAS version.
    pub version: MessageFeedbackVersion,
    /// Host creation time in Unix milliseconds.
    pub created_at: u64,
    /// Host update time in Unix milliseconds.
    pub updated_at: u64,
}

/// Stable business failure wire shape.
#[derive(Clone, Debug, PartialEq)]
pub enum MessageFeedbackFailure {
    /// Session sidecar is unavailable.
    SessionNotFound {
        /// Requested Session.
        session_id: FeedbackSessionId,
    },
    /// Target is not a persisted assistant message.
    TargetNotFound {
        /// Owning Session.
        session_id: FeedbackSessionId,
        /// Target message.
        message_id: FeedbackMessageId,
    },
    /// CAS lost; current carries the authoritative row or absence.
    VersionConflict {
        /// Authoritative item.
        current: Option<MessageFeedbackItem>,
    },
    /// Supplied note is blank.
    NoteBlank,
    /// Supplied note exceeds the byte policy.
    NoteTooLarge {
        /// Configured ceiling.
        max_bytes: usize,
        /// Supplied byte length.
        actual_bytes: usize,
    },
    /// Forward-compatible failure retaining every unrecognized wire field.
    Unknown {
        /// Original failure code.
        code: String,
        /// Original fields other than `code`.
        fields: JsonMap<String, JsonValue>,
    },
}

impl Serialize for MessageFeedbackFailure {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let extra = match self {
            Self::Unknown { fields, .. } => fields.len(),
            Self::SessionNotFound { .. } | Self::VersionConflict { .. } => 1,
            Self::TargetNotFound { .. } | Self::NoteTooLarge { .. } => 2,
            Self::NoteBlank => 0,
        };
        let mut map = serializer.serialize_map(Some(extra + 1))?;
        match self {
            Self::SessionNotFound { session_id } => {
                map.serialize_entry("code", "session-not-found")?;
                map.serialize_entry("sessionId", session_id)?;
            }
            Self::TargetNotFound {
                session_id,
                message_id,
            } => {
                map.serialize_entry("code", "target-not-found")?;
                map.serialize_entry("sessionId", session_id)?;
                map.serialize_entry("messageId", message_id)?;
            }
            Self::VersionConflict { current } => {
                map.serialize_entry("code", "version-conflict")?;
                map.serialize_entry("current", current)?;
            }
            Self::NoteBlank => map.serialize_entry("code", "note-blank")?,
            Self::NoteTooLarge {
                max_bytes,
                actual_bytes,
            } => {
                map.serialize_entry("code", "note-too-large")?;
                map.serialize_entry("maxBytes", max_bytes)?;
                map.serialize_entry("actualBytes", actual_bytes)?;
            }
            Self::Unknown { code, fields } => {
                map.serialize_entry("code", code)?;
                for (key, value) in fields {
                    map.serialize_entry(key, value)?;
                }
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for MessageFeedbackFailure {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let JsonValue::Object(mut fields) = JsonValue::deserialize(deserializer)? else {
            return Err(D::Error::custom(
                "message feedback failure must be an object",
            ));
        };
        let code = take_field::<String, D::Error>(&mut fields, "code")?;
        match code.as_str() {
            "session-not-found" => Ok(Self::SessionNotFound {
                session_id: take_field(&mut fields, "sessionId")?,
            }),
            "target-not-found" => Ok(Self::TargetNotFound {
                session_id: take_field(&mut fields, "sessionId")?,
                message_id: take_field(&mut fields, "messageId")?,
            }),
            "version-conflict" => Ok(Self::VersionConflict {
                current: take_field(&mut fields, "current")?,
            }),
            "note-blank" => Ok(Self::NoteBlank),
            "note-too-large" => Ok(Self::NoteTooLarge {
                max_bytes: take_field(&mut fields, "maxBytes")?,
                actual_bytes: take_field(&mut fields, "actualBytes")?,
            }),
            _ => Ok(Self::Unknown { code, fields }),
        }
    }
}

fn take_field<T, E>(fields: &mut JsonMap<String, JsonValue>, key: &str) -> Result<T, E>
where
    T: DeserializeOwned,
    E: serde::de::Error,
{
    let value = fields
        .remove(key)
        .ok_or_else(|| E::custom(format!("message feedback failure omitted {key}")))?;
    serde_json::from_value(value).map_err(E::custom)
}

/// Carrier-layer failure already normalized by the generated Remote face.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackCarrierFailure {
    /// Stable failure code.
    pub code: String,
    /// Host-supplied display message.
    pub message: String,
}

/// One Remote method settles to a carrier envelope containing a business result.
pub type FeedbackRemoteResult<T> =
    Result<Result<T, MessageFeedbackFailure>, FeedbackCarrierFailure>;

/// Browser transport seam for the three generated message-feedback methods.
pub trait MessageFeedbackRemote {
    /// Lists the Session sidecar.
    fn list(
        &self,
        session_id: FeedbackSessionId,
    ) -> LocalBoxFuture<'static, Result<FeedbackRemoteResult<Vec<MessageFeedbackItem>>, String>>;

    /// Creates or replaces one item with compare-and-set.
    fn put(
        &self,
        session_id: FeedbackSessionId,
        message_id: FeedbackMessageId,
        rating: MessageFeedbackRating,
        note: Option<String>,
        if_version: Option<MessageFeedbackVersion>,
    ) -> LocalBoxFuture<'static, Result<FeedbackRemoteResult<MessageFeedbackItem>, String>>;

    /// Deletes one item with compare-and-set.
    fn delete(
        &self,
        session_id: FeedbackSessionId,
        message_id: FeedbackMessageId,
        if_version: MessageFeedbackVersion,
    ) -> LocalBoxFuture<'static, Result<FeedbackRemoteResult<()>, String>>;
}

/// Lazy list status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageFeedbackStatus {
    /// No read has been requested.
    #[default]
    Cold,
    /// One shared list call is in flight.
    Loading,
    /// Authoritative list is available.
    Ready,
    /// Last list attempt failed and remains retryable.
    Error,
}

/// Immutable view published to message controls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageFeedbackView {
    /// Lazy load state.
    pub status: MessageFeedbackStatus,
    /// Current item per message in first-creation order.
    pub items: IndexMap<FeedbackMessageId, MessageFeedbackItem>,
    /// Last list error, if any.
    pub error: Option<String>,
}

impl Default for MessageFeedbackView {
    fn default() -> Self {
        Self {
            status: MessageFeedbackStatus::Cold,
            items: IndexMap::new(),
            error: None,
        }
    }
}

/// Settled controller action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageFeedbackActionResult {
    /// Operation completed or was already in the requested state.
    Ok,
    /// Carrier or business failure.
    Error {
        /// Stable failure code.
        code: String,
        /// Human-readable explanation.
        message: String,
    },
}

impl MessageFeedbackActionResult {
    /// Whether the operation succeeded.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
        }
    }
}

type Listener = Rc<dyn Fn()>;
type SharedLoad = Shared<LocalBoxFuture<'static, MessageFeedbackActionResult>>;

struct State {
    view: Rc<MessageFeedbackView>,
    listeners: IndexMap<u128, Listener>,
    next_listener: u128,
    load: Option<(u64, SharedLoad)>,
    next_load: u64,
    disposed: bool,
}

/// Subscription handle.
pub struct FeedbackSubscription {
    state: Rc<RefCell<State>>,
    id: u128,
}

impl Drop for FeedbackSubscription {
    fn drop(&mut self) {
        self.state.borrow_mut().listeners.shift_remove(&self.id);
    }
}

/// Per-session feedback object layer.
#[derive(Clone)]
pub struct MessageFeedbackController {
    remote: Rc<dyn MessageFeedbackRemote>,
    session_id: FeedbackSessionId,
    state: Rc<RefCell<State>>,
    operations: Rc<Mutex<()>>,
}

impl std::fmt::Debug for MessageFeedbackController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageFeedbackController")
            .field("session_id", &self.session_id)
            .field("view", &self.state.borrow().view)
            .finish_non_exhaustive()
    }
}

impl MessageFeedbackController {
    /// Creates a cold controller.
    #[must_use]
    pub fn new(remote: Rc<dyn MessageFeedbackRemote>, session_id: FeedbackSessionId) -> Self {
        Self {
            remote,
            session_id,
            state: Rc::new(RefCell::new(State {
                view: Rc::new(MessageFeedbackView::default()),
                listeners: IndexMap::new(),
                next_listener: 0,
                load: None,
                next_load: 0,
                disposed: false,
            })),
            operations: Rc::new(Mutex::new(())),
        }
    }

    /// Stable view reference until the next publish.
    #[must_use]
    pub fn snapshot(&self) -> Rc<MessageFeedbackView> {
        self.state.borrow().view.clone()
    }

    /// Subscribes to view replacement.
    pub fn subscribe(&self, listener: Listener) -> FeedbackSubscription {
        let id = {
            let mut state = self.state.borrow_mut();
            let id = state.next_listener;
            state.next_listener += 1;
            state.listeners.insert(id, listener);
            id
        };
        FeedbackSubscription {
            state: self.state.clone(),
            id,
        }
    }

    /// Loads only until ready; failed loads remain retryable.
    pub async fn ensure(&self) -> MessageFeedbackActionResult {
        if self.snapshot().status == MessageFeedbackStatus::Ready {
            return MessageFeedbackActionResult::Ok;
        }
        self.refresh().await
    }

    /// Collapses concurrent list calls onto one shared future.
    pub async fn refresh(&self) -> MessageFeedbackActionResult {
        let existing = self
            .state
            .borrow()
            .load
            .as_ref()
            .map(|(_, shared)| shared.clone());
        if let Some(existing) = existing {
            return existing.await;
        }
        let shared = {
            let mut state = self.state.borrow_mut();
            let token = state.next_load;
            state.next_load = state.next_load.wrapping_add(1);
            let current = state.view.clone();
            drop(state);
            self.publish(MessageFeedbackView {
                status: MessageFeedbackStatus::Loading,
                items: current.items.clone(),
                error: None,
            });
            let controller = self.clone();
            let future = async move {
                let result = controller.load().await;
                let mut state = controller.state.borrow_mut();
                if state
                    .load
                    .as_ref()
                    .is_some_and(|(current, _)| *current == token)
                {
                    state.load = None;
                }
                result
            }
            .boxed_local()
            .shared();
            self.state.borrow_mut().load = Some((token, future.clone()));
            future
        };
        shared.await
    }

    /// Re-reads behind all previously admitted mutations.
    pub async fn resync(&self) -> MessageFeedbackActionResult {
        let _guard = self.operations.lock().await;
        if self.is_disposed() {
            return disposed();
        }
        self.refresh().await
    }

    /// Creates/replaces a rating, preserving the committed note when omitted.
    pub async fn rate(
        &self,
        message_id: FeedbackMessageId,
        rating: MessageFeedbackRating,
        note: Option<String>,
    ) -> MessageFeedbackActionResult {
        self.mutate(true, move |controller| {
            async move {
                let observed = controller.snapshot().items.get(&message_id).cloned();
                let note = note.or_else(|| observed.as_ref().and_then(|item| item.note.clone()));
                controller
                    .put_committed(message_id, rating, note, observed)
                    .await
            }
            .boxed_local()
        })
        .await
    }

    /// Retracts a matching committed rating, otherwise replaces it.
    pub async fn toggle(
        &self,
        message_id: FeedbackMessageId,
        rating: MessageFeedbackRating,
    ) -> MessageFeedbackActionResult {
        self.mutate(true, move |controller| {
            async move {
                let observed = controller.snapshot().items.get(&message_id).cloned();
                match observed {
                    Some(observed) if observed.rating == rating => {
                        controller.delete_committed(message_id, observed).await
                    }
                    observed => {
                        let note = observed.as_ref().and_then(|item| item.note.clone());
                        controller
                            .put_committed(message_id, rating, note, observed)
                            .await
                    }
                }
            }
            .boxed_local()
        })
        .await
    }

    /// Removes only the note, preserving the committed rating.
    pub async fn clear_note(&self, message_id: FeedbackMessageId) -> MessageFeedbackActionResult {
        self.mutate(true, move |controller| {
            async move {
                let observed = controller.snapshot().items.get(&message_id).cloned();
                let Some(observed) = observed else {
                    return MessageFeedbackActionResult::Ok;
                };
                if observed.note.is_none() {
                    return MessageFeedbackActionResult::Ok;
                }
                controller
                    .put_committed(message_id, observed.rating, None, Some(observed))
                    .await
            }
            .boxed_local()
        })
        .await
    }

    /// Removes the whole item, with absent feedback as a no-op.
    pub async fn clear(&self, message_id: FeedbackMessageId) -> MessageFeedbackActionResult {
        self.mutate(true, move |controller| {
            async move {
                let observed = controller.snapshot().items.get(&message_id).cloned();
                let Some(observed) = observed else {
                    return MessageFeedbackActionResult::Ok;
                };
                controller.delete_committed(message_id, observed).await
            }
            .boxed_local()
        })
        .await
    }

    /// Drops subscribers and refuses newly admitted work.
    pub fn dispose(&self) {
        let mut state = self.state.borrow_mut();
        state.disposed = true;
        state.listeners.clear();
    }

    async fn mutate(
        &self,
        seed: bool,
        operation: impl FnOnce(Self) -> LocalBoxFuture<'static, MessageFeedbackActionResult>,
    ) -> MessageFeedbackActionResult {
        let _guard = self.operations.lock().await;
        if self.is_disposed() {
            return disposed();
        }
        if seed {
            let loaded = self.ensure().await;
            if !loaded.is_ok() {
                return loaded;
            }
            if self.is_disposed() {
                return disposed();
            }
        }
        operation(self.clone()).await
    }

    async fn load(&self) -> MessageFeedbackActionResult {
        let carried = self.remote.list(self.session_id.clone()).await;
        if self.is_disposed() {
            return MessageFeedbackActionResult::Ok;
        }
        match carried {
            Err(message) => {
                self.publish_error(message.clone());
                MessageFeedbackActionResult::error("transport", message)
            }
            Ok(Err(error)) => {
                self.publish_error(error.message.clone());
                carrier_failure(error)
            }
            Ok(Ok(Err(error))) => {
                let result = business_failure(&error);
                let message = match &result {
                    MessageFeedbackActionResult::Error { message, .. } => message.clone(),
                    MessageFeedbackActionResult::Ok => unreachable!(),
                };
                self.publish_error(message);
                result
            }
            Ok(Ok(Ok(items))) => {
                self.publish(MessageFeedbackView {
                    status: MessageFeedbackStatus::Ready,
                    items: items
                        .into_iter()
                        .map(|item| (item.message_id.clone(), item))
                        .collect(),
                    error: None,
                });
                MessageFeedbackActionResult::Ok
            }
        }
    }

    async fn put_committed(
        &self,
        message_id: FeedbackMessageId,
        rating: MessageFeedbackRating,
        note: Option<String>,
        observed: Option<MessageFeedbackItem>,
    ) -> MessageFeedbackActionResult {
        let carried = self
            .remote
            .put(
                self.session_id.clone(),
                message_id.clone(),
                rating,
                note,
                observed.map(|item| item.version),
            )
            .await;
        match carried {
            Err(message) => MessageFeedbackActionResult::error("transport", message),
            Ok(Err(error)) => carrier_failure(error),
            Ok(Ok(Ok(item))) => {
                self.commit(message_id, Some(item));
                MessageFeedbackActionResult::Ok
            }
            Ok(Ok(Err(error))) => {
                if let MessageFeedbackFailure::VersionConflict { current } = &error {
                    self.commit(message_id, current.clone());
                }
                business_failure(&error)
            }
        }
    }

    async fn delete_committed(
        &self,
        message_id: FeedbackMessageId,
        observed: MessageFeedbackItem,
    ) -> MessageFeedbackActionResult {
        let carried = self
            .remote
            .delete(
                self.session_id.clone(),
                message_id.clone(),
                observed.version,
            )
            .await;
        match carried {
            Err(message) => MessageFeedbackActionResult::error("transport", message),
            Ok(Err(error)) => carrier_failure(error),
            Ok(Ok(Ok(()))) => {
                self.commit(message_id, None);
                MessageFeedbackActionResult::Ok
            }
            Ok(Ok(Err(error))) => {
                if let MessageFeedbackFailure::VersionConflict { current } = &error {
                    self.commit(message_id, current.clone());
                }
                business_failure(&error)
            }
        }
    }

    fn commit(&self, message_id: FeedbackMessageId, item: Option<MessageFeedbackItem>) {
        let mut items = self.snapshot().items.clone();
        match item {
            Some(item) => {
                items.insert(message_id, item);
            }
            None => {
                items.shift_remove(&message_id);
            }
        }
        self.publish(MessageFeedbackView {
            status: MessageFeedbackStatus::Ready,
            items,
            error: None,
        });
    }

    fn publish_error(&self, message: String) {
        let current = self.snapshot();
        self.publish(MessageFeedbackView {
            status: MessageFeedbackStatus::Error,
            items: current.items.clone(),
            error: Some(message),
        });
    }

    fn publish(&self, view: MessageFeedbackView) {
        self.state.borrow_mut().view = Rc::new(view);
        let mut cursor = None;
        loop {
            let next = self
                .state
                .borrow()
                .listeners
                .iter()
                .find(|(id, _)| cursor.is_none_or(|cursor| **id > cursor))
                .map(|(id, listener)| (*id, listener.clone()));
            let Some((id, listener)) = next else {
                break;
            };
            cursor = Some(id);
            let _ = catch_unwind(AssertUnwindSafe(|| listener()));
        }
    }

    fn is_disposed(&self) -> bool {
        self.state.borrow().disposed
    }
}

fn carrier_failure(error: FeedbackCarrierFailure) -> MessageFeedbackActionResult {
    MessageFeedbackActionResult::error(error.code, error.message)
}

fn business_failure(error: &MessageFeedbackFailure) -> MessageFeedbackActionResult {
    let code = match error {
        MessageFeedbackFailure::SessionNotFound { .. } => "session-not-found",
        MessageFeedbackFailure::TargetNotFound { .. } => "target-not-found",
        MessageFeedbackFailure::VersionConflict { .. } => "version-conflict",
        MessageFeedbackFailure::NoteBlank => "note-blank",
        MessageFeedbackFailure::NoteTooLarge { .. } => "note-too-large",
        MessageFeedbackFailure::Unknown { code, .. } => code,
    };
    MessageFeedbackActionResult::error(code, describe(code))
}

/// Human-readable source-compatible business failure text.
#[must_use]
pub fn describe(code: &str) -> String {
    match code {
        "session-not-found" => "this session is no longer persisted",
        "target-not-found" => "this message is not a persisted assistant message",
        "version-conflict" => "feedback changed elsewhere",
        "note-blank" => "a note must contain a non-whitespace character",
        "note-too-large" => "the note is too long",
        other => other,
    }
    .to_owned()
}

fn disposed() -> MessageFeedbackActionResult {
    MessageFeedbackActionResult::error("disposed", "feedback controller is disposed")
}
