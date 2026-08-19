//! Durable lifecycle-bound message feedback domain and its storage declaration.

use std::{collections::HashMap, sync::Arc};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_core::session::{
    SessionHeader, SessionId, derive_event_message, is_append_surface_event,
};
use seekdeep_core::session_store::{SESSIONS, SessionStore};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{MessageId, MessageRole};
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionInspection, SessionPersistence};
use seekdeep_storage_domain::{
    DomainFacility, DomainSpec, KvTable, STORAGE_DOMAIN, ValueSchema, define_domain, domain_table,
};
use serde::{Deserialize, Serialize};

/// Closed rating vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageFeedbackRating {
    /// The response was helpful.
    Positive,
    /// The response was not helpful.
    Negative,
}

/// Opaque item version token.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageFeedbackVersion(pub String);

impl std::fmt::Display for MessageFeedbackVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One current feedback item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFeedbackItem {
    /// Target assistant-message identity.
    pub message_id: MessageId,
    /// Overall judgment.
    pub rating: MessageFeedbackRating,
    /// Optional non-blank explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Equality-only token replaced by every material create or update.
    pub version: MessageFeedbackVersion,
    /// Host-assigned creation time in Unix epoch milliseconds.
    pub created_at: u64,
    /// Host-assigned time of the most recent material update.
    pub updated_at: u64,
}

/// Persisted Session fields that fence a sidecar row to one log lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFeedbackSessionIdentity {
    /// Session creation timestamp distinguishing one lifecycle.
    pub created_at: u64,
    /// Working directory, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// One whole-Session sidecar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageFeedbackRow {
    /// Log lifecycle the items belong to.
    pub session: MessageFeedbackSessionIdentity,
    /// Feedback items in first-creation order.
    pub items: Vec<MessageFeedbackItem>,
}

/// Validates one durable sidecar row: duplicate message ids or versions are
/// ambiguous and must be rejected before a write.
///
/// # Errors
///
/// Returns the first duplicate-message or duplicate-version diagnostic.
pub fn validate_message_feedback_row(row: &MessageFeedbackRow) -> anyhow::Result<()> {
    let mut message_ids = std::collections::HashSet::new();
    let mut versions = std::collections::HashSet::new();
    for item in &row.items {
        anyhow::ensure!(
            message_ids.insert(item.message_id.clone()),
            "duplicate message feedback id '{}'",
            item.message_id
        );
        anyhow::ensure!(
            versions.insert(item.version.clone()),
            "duplicate message feedback version '{}'",
            item.version
        );
        anyhow::ensure!(
            item.updated_at >= item.created_at,
            "message feedback updatedAt must not precede createdAt"
        );
    }
    Ok(())
}

/// The message-feedback domain declaration.
///
/// # Panics
///
/// Panics only on an invalid hard-coded domain declaration.
#[must_use]
pub fn message_feedback_domain_spec() -> DomainSpec {
    let spec = DomainSpec {
        name: "message_feedback".to_owned(),
        version: 0,
        global: None,
        tables: IndexMap::from([(
            "sessions".to_owned(),
            domain_table(ValueSchema::serde::<MessageFeedbackRow>()),
        )]),
    };
    define_domain(spec).expect("valid domain spec")
}

/// Projects a session header onto the identity fields a sidecar is bound to.
#[must_use]
pub fn identity_of(
    header: &seekdeep_core::session::SessionHeader,
) -> MessageFeedbackSessionIdentity {
    MessageFeedbackSessionIdentity {
        created_at: header.created_at,
        cwd: header.cwd.clone(),
    }
}

/// Stable business failure for the public message-feedback operations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "kebab-case")]
pub enum MessageFeedbackFailure {
    /// No persisted Session header exists for the requested id.
    #[error("session-not-found: {session_id}")]
    #[serde(rename_all = "camelCase")]
    SessionNotFound {
        /// Requested session id.
        session_id: SessionId,
    },
    /// The id does not name a derived append-origin assistant message.
    #[error("target-not-found: {session_id}/{message_id}")]
    #[serde(rename_all = "camelCase")]
    TargetNotFound {
        /// Owning session id.
        session_id: SessionId,
        /// Target message id.
        message_id: MessageId,
    },
    /// A material mutation did not match the addressed item's current version.
    #[error("version-conflict")]
    VersionConflict {
        /// Authoritative current item, or null when it does not exist.
        current: Option<MessageFeedbackItem>,
    },
    /// A supplied note contains no non-whitespace character.
    #[error("note-blank")]
    NoteBlank,
    /// A supplied note exceeds the configured UTF-8 byte limit.
    #[error("note-too-large: {actual_bytes} > {max_bytes}")]
    #[serde(rename_all = "camelCase")]
    NoteTooLarge {
        /// Configured byte ceiling.
        max_bytes: usize,
        /// Supplied byte length.
        actual_bytes: usize,
    },
}

/// Create or replace feedback for one assistant message.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFeedbackPutRequest {
    /// Owning session id.
    pub session_id: SessionId,
    /// Target message id.
    pub message_id: MessageId,
    /// Desired overall judgment.
    pub rating: MessageFeedbackRating,
    /// Optional non-blank explanation.
    #[serde(default)]
    pub note: Option<String>,
    /// Observed item version, or None to require that no item exists.
    #[serde(default)]
    pub if_version: Option<MessageFeedbackVersion>,
}

/// Delete feedback for one message after observing its current version.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFeedbackDeleteRequest {
    /// Owning session id.
    pub session_id: SessionId,
    /// Target message id.
    pub message_id: MessageId,
    /// Observed item version.
    pub if_version: MessageFeedbackVersion,
}

/// Lifecycle-bound message feedback service.
pub struct MessageFeedbackService {
    table: Arc<KvTable>,
    sessions: Arc<SessionStore>,
    persistence: Arc<dyn SessionPersistence>,
    max_note_bytes: usize,
    locks: Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for MessageFeedbackService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageFeedbackService")
            .field("max_note_bytes", &self.max_note_bytes)
            .finish_non_exhaustive()
    }
}

impl MessageFeedbackService {
    /// Opens the sidecar domain and constructs the service.
    ///
    /// # Errors
    ///
    /// Returns missing-service or domain-open failures.
    pub async fn install(context: &Context, max_note_bytes: usize) -> anyhow::Result<Arc<Self>> {
        let facility: Arc<DomainFacility> = context
            .get(STORAGE_DOMAIN)
            .ok_or_else(|| anyhow::anyhow!("message-feedback requires storageDomain"))?;
        let domain = facility.open(message_feedback_domain_spec()).await?;
        let table = domain.table("sessions")?;
        let sessions: Arc<SessionStore> = context
            .get(SESSIONS)
            .ok_or_else(|| anyhow::anyhow!("message-feedback requires sessions"))?;
        let persistence = context
            .get(SESSION_PERSISTENCE)
            .ok_or_else(|| anyhow::anyhow!("message-feedback requires sessionPersistence"))?
            .persistence();
        Ok(Arc::new(Self {
            table,
            sessions,
            persistence,
            max_note_bytes,
            locks: Mutex::new(HashMap::new()),
        }))
    }

    /// Reads feedback belonging to the current persisted Session lifecycle.
    ///
    /// # Errors
    ///
    /// Returns session-not-found or a persistence failure.
    pub async fn list(
        &self,
        session_id: &SessionId,
    ) -> anyhow::Result<Result<Vec<MessageFeedbackItem>, MessageFeedbackFailure>> {
        let known = match self.inspect_session(session_id).await? {
            Ok(known) => known,
            Err(failure) => return Ok(Err(failure)),
        };
        let items = self
            .table
            .get(session_id.as_str())?
            .and_then(|value| serde_json::from_value::<MessageFeedbackRow>(value).ok())
            .filter(|row| same_identity(&row.session, &identity_of(&known.meta)))
            .map_or_else(Vec::new, |row| row.items);
        Ok(Ok(items))
    }

    /// Creates or replaces feedback for one derived append-origin assistant
    /// message.
    ///
    /// # Errors
    ///
    /// Returns an explicit business failure or a persistence failure.
    pub async fn put(
        &self,
        request: MessageFeedbackPutRequest,
    ) -> anyhow::Result<Result<MessageFeedbackItem, MessageFeedbackFailure>> {
        let note = match self.resolve_note(request.note.as_deref()) {
            Ok(note) => note,
            Err(failure) => return Ok(Err(failure)),
        };
        let lock = self.lock_for(&request.session_id);
        let _guard = lock.lock().await;

        let known = match self.inspect_session(&request.session_id).await? {
            Ok(known) => known,
            Err(failure) => return Ok(Err(failure)),
        };
        if !has_feedback_target(&known, &request.message_id) {
            return Ok(Err(MessageFeedbackFailure::TargetNotFound {
                session_id: request.session_id,
                message_id: request.message_id,
            }));
        }

        let durable = self.ensure_target_durable(&known).await?;
        if !same_header_identity(&durable.meta, &known.meta)
            || !has_feedback_target(&durable, &request.message_id)
        {
            return Ok(Err(MessageFeedbackFailure::TargetNotFound {
                session_id: request.session_id,
                message_id: request.message_id,
            }));
        }

        let current: Option<MessageFeedbackRow> = self
            .table
            .get(request.session_id.as_str())?
            .and_then(|value| serde_json::from_value(value).ok())
            .filter(|row: &MessageFeedbackRow| {
                same_identity(&row.session, &identity_of(&durable.meta))
            });
        let mut items = current
            .as_ref()
            .map_or_else(Vec::new, |row| row.items.clone());
        let index = items
            .iter()
            .position(|item| item.message_id == request.message_id);
        let existing = index.map(|index| items[index].clone());
        let expected = existing.as_ref().map(|item| item.version.clone());
        if request.if_version != expected {
            return Ok(Err(MessageFeedbackFailure::VersionConflict {
                current: existing,
            }));
        }
        if let Some(existing) = &existing
            && existing.rating == request.rating
            && existing.note == note
        {
            return Ok(Ok(existing.clone()));
        }

        let now = now_millis();
        let item = MessageFeedbackItem {
            message_id: request.message_id,
            rating: request.rating,
            note,
            version: MessageFeedbackVersion(uuid::Uuid::new_v4().to_string()),
            created_at: existing.as_ref().map_or(now, |item| item.created_at),
            updated_at: existing
                .as_ref()
                .map_or(now, |item| now.max(item.updated_at)),
        };
        if let Some(index) = index {
            items[index] = item.clone();
        } else {
            items.push(item.clone());
        }
        let record = MessageFeedbackRow {
            session: identity_of(&durable.meta),
            items,
        };
        validate_message_feedback_row(&record)?;
        self.table
            .put(
                request.session_id.as_str().to_owned(),
                serde_json::to_value(record)?,
            )
            .await?;
        Ok(Ok(item))
    }

    /// Deletes one feedback item. Absence succeeds regardless of version.
    ///
    /// # Errors
    ///
    /// Returns an explicit business failure or a persistence failure.
    pub async fn delete(
        &self,
        request: MessageFeedbackDeleteRequest,
    ) -> anyhow::Result<Result<(), MessageFeedbackFailure>> {
        let lock = self.lock_for(&request.session_id);
        let _guard = lock.lock().await;

        let known = match self.inspect_session(&request.session_id).await? {
            Ok(known) => known,
            Err(failure) => return Ok(Err(failure)),
        };
        let current: Option<MessageFeedbackRow> = self
            .table
            .get(request.session_id.as_str())?
            .and_then(|value| serde_json::from_value(value).ok())
            .filter(|row: &MessageFeedbackRow| {
                same_identity(&row.session, &identity_of(&known.meta))
            });
        let items = current
            .as_ref()
            .map_or_else(Vec::new, |row| row.items.clone());
        let Some(existing) = items
            .iter()
            .find(|item| item.message_id == request.message_id)
        else {
            return Ok(Ok(()));
        };
        if request.if_version != existing.version {
            return Ok(Err(MessageFeedbackFailure::VersionConflict {
                current: Some(existing.clone()),
            }));
        }
        let retained: Vec<MessageFeedbackItem> = items
            .into_iter()
            .filter(|item| item.message_id != request.message_id)
            .collect();
        self.table
            .put(
                request.session_id.as_str().to_owned(),
                serde_json::to_value(MessageFeedbackRow {
                    session: identity_of(&known.meta),
                    items: retained,
                })?,
            )
            .await?;
        Ok(Ok(()))
    }

    fn lock_for(&self, id: &SessionId) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .entry(id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn inspect_session(
        &self,
        session_id: &SessionId,
    ) -> anyhow::Result<Result<SessionInspection, MessageFeedbackFailure>> {
        if self.sessions.get(session_id).is_none() {
            let snapshots = self.persistence.list_snapshots(None).await?;
            if !snapshots
                .iter()
                .any(|snapshot| snapshot.header.id == *session_id)
                && self.sessions.get(session_id).is_none()
            {
                return Ok(Err(MessageFeedbackFailure::SessionNotFound {
                    session_id: session_id.clone(),
                }));
            }
        }
        Ok(Ok(self.persistence.inspect(session_id, None).await?))
    }

    async fn ensure_target_durable(
        &self,
        inspection: &SessionInspection,
    ) -> anyhow::Result<SessionInspection> {
        if let Some(live) = self.sessions.get(&inspection.meta.id)
            && same_header_identity(live.header(), &inspection.meta)
        {
            anyhow::ensure!(
                self.sessions.flush(&live).await?,
                "message-feedback: no durability listener participated for live session '{}'",
                inspection.meta.id
            );
        }
        self.persistence
            .read_from(&inspection.meta.id, 0, None)
            .await
    }

    fn resolve_note(&self, note: Option<&str>) -> Result<Option<String>, MessageFeedbackFailure> {
        let Some(note) = note else {
            return Ok(None);
        };
        if note.trim().is_empty() {
            return Err(MessageFeedbackFailure::NoteBlank);
        }
        let actual_bytes = note.len();
        if actual_bytes > self.max_note_bytes {
            return Err(MessageFeedbackFailure::NoteTooLarge {
                max_bytes: self.max_note_bytes,
                actual_bytes,
            });
        }
        Ok(Some(note.to_owned()))
    }
}

fn same_identity(
    stored: &MessageFeedbackSessionIdentity,
    expected: &MessageFeedbackSessionIdentity,
) -> bool {
    stored.created_at == expected.created_at && stored.cwd == expected.cwd
}

fn same_header_identity(left: &SessionHeader, right: &SessionHeader) -> bool {
    left.id == right.id && left.created_at == right.created_at && left.cwd == right.cwd
}

fn has_feedback_target(inspection: &SessionInspection, message_id: &MessageId) -> bool {
    inspection.events.iter().any(|event| {
        if event.event_type != "assistant/message" || !is_append_surface_event(event) {
            return false;
        }
        derive_event_message(event).is_some_and(|message| {
            message.role() == MessageRole::Assistant && message.id() == message_id
        })
    })
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Registers the package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-message-feedback", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::Context;
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    fn item(id: &str, version: &str) -> MessageFeedbackItem {
        MessageFeedbackItem {
            message_id: MessageId::new(id),
            rating: MessageFeedbackRating::Positive,
            note: None,
            version: MessageFeedbackVersion(version.to_owned()),
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn row_validation_rejects_duplicate_message_ids_and_versions() {
        let ok = MessageFeedbackRow {
            session: MessageFeedbackSessionIdentity {
                created_at: 1,
                cwd: None,
            },
            items: vec![item("a", "v1"), item("b", "v2")],
        };
        assert!(validate_message_feedback_row(&ok).is_ok());

        let dup_id = MessageFeedbackRow {
            session: ok.session.clone(),
            items: vec![item("a", "v1"), item("a", "v2")],
        };
        assert!(validate_message_feedback_row(&dup_id).is_err());

        let dup_version = MessageFeedbackRow {
            session: ok.session.clone(),
            items: vec![item("a", "v1"), item("b", "v1")],
        };
        assert!(validate_message_feedback_row(&dup_version).is_err());
    }

    #[test]
    fn domain_spec_declares_one_sessions_table_at_version_zero() {
        let spec = message_feedback_domain_spec();
        assert_eq!(spec.name, "message_feedback");
        assert_eq!(spec.version, 0);
        assert!(spec.tables.contains_key("sessions"));
    }

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
        let registration = register_invariant(&registry).expect("register");
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.expect("dispose");
        register_invariant(&registry).expect("replacement");
    }
}
