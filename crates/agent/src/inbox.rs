//! Incremental projection of durable agent inbox splices.

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use seekdeep_core::session::{AppendOptions, Session};
use seekdeep_llm::{MessageId, UserMessage};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One of the two ordered pending-message lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InboxTarget {
    /// Ordinary input, one message per turn.
    NextTurn,
    /// Steering/context consumed at the nearest step boundary.
    NextStep,
}

/// Live notifications committed after an inbox mutation.
pub trait InboxNotifications: Send + Sync + 'static {
    /// Publishes one inserted message.
    fn inserted(&self, message: &UserMessage);
    /// Publishes one discarded message.
    fn discarded(&self, message: &UserMessage);
    /// Publishes one claimed message inside its owning turn.
    fn claimed(&self, message: &UserMessage, turn: u64);
}

/// No-op notification sink for replay and isolated use.
#[derive(Debug, Default)]
pub struct NoopInboxNotifications;

impl InboxNotifications for NoopInboxNotifications {
    fn inserted(&self, _message: &UserMessage) {}
    fn discarded(&self, _message: &UserMessage) {}
    fn claimed(&self, _message: &UserMessage, _turn: u64) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum InboxOutcome {
    Canceled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InboxSplice {
    target: InboxTarget,
    start: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    removed_count: Option<u64>,
    inserted: Vec<UserMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<InboxOutcome>,
}

#[derive(Default)]
struct InboxState {
    next_turn: Vec<UserMessage>,
    next_step: Vec<UserMessage>,
}

struct MutationOutcome {
    removed: Vec<UserMessage>,
    inserted: Vec<UserMessage>,
}

impl InboxState {
    fn list(&self, target: InboxTarget) -> &Vec<UserMessage> {
        match target {
            InboxTarget::NextTurn => &self.next_turn,
            InboxTarget::NextStep => &self.next_step,
        }
    }

    fn list_mut(&mut self, target: InboxTarget) -> &mut Vec<UserMessage> {
        match target {
            InboxTarget::NextTurn => &mut self.next_turn,
            InboxTarget::NextStep => &mut self.next_step,
        }
    }

    fn locate(&self, id: &MessageId) -> Option<(InboxTarget, usize)> {
        [InboxTarget::NextTurn, InboxTarget::NextStep]
            .into_iter()
            .find_map(|target| {
                self.list(target)
                    .iter()
                    .position(|message| message.id() == id)
                    .map(|index| (target, index))
            })
    }
}

/// Inbox reconstruction or mutation failure.
#[derive(Debug, Error)]
pub enum InboxError {
    /// A persisted splice cannot apply to the preceding projection.
    #[error("invalid persisted inbox splice at session seq {seq}: {message}")]
    InvalidPersisted {
        /// Offending durable sequence.
        seq: u64,
        /// Underlying validation detail.
        message: String,
    },
    /// A requested splice is structurally invalid.
    #[error("invalid inbox splice")]
    InvalidSplice,
    /// A synchronous durable observer attempted a nested mutation.
    #[error("inbox mutation cannot reenter while another mutation is being published")]
    ReentrantMutation,
    /// One message identity would appear more than once across both lists.
    #[error("message {0:?} is already pending")]
    DuplicateMessage(String),
    /// Durable session append failed.
    #[error(transparent)]
    Session(#[from] seekdeep_core::session::SessionError),
    /// A committed event did not round-trip through its declared shape.
    #[error("committed inbox splice is invalid: {0}")]
    Committed(String),
}

/// Replay-once projection that incrementally commits later inbox splices.
pub struct Inbox {
    session: Arc<Session>,
    notifications: Arc<dyn InboxNotifications>,
    state: RwLock<InboxState>,
    mutation: Mutex<()>,
}

impl std::fmt::Debug for Inbox {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.read();
        formatter
            .debug_struct("Inbox")
            .field("next_turn", &state.next_turn.len())
            .field("next_step", &state.next_step.len())
            .finish_non_exhaustive()
    }
}

impl Inbox {
    /// Replays durable post-seed splices and begins live projection.
    ///
    /// # Errors
    ///
    /// Returns the first invalid persisted splice with its session sequence.
    pub fn new(
        session: Arc<Session>,
        notifications: Arc<dyn InboxNotifications>,
    ) -> Result<Self, InboxError> {
        let mut state = InboxState::default();
        let seed_length = session.header().seed_length.unwrap_or(0);
        for event in session
            .events()
            .into_iter()
            .skip(usize::try_from(seed_length).unwrap_or(usize::MAX))
        {
            if event.event_type != "agent/inbox/spliced" {
                continue;
            }
            let splice = serde_json::from_value::<InboxSplice>(event.data).map_err(|error| {
                InboxError::InvalidPersisted {
                    seq: event.seq,
                    message: error.to_string(),
                }
            })?;
            apply_splice(&mut state, &splice).map_err(|error| InboxError::InvalidPersisted {
                seq: event.seq,
                message: error.to_string(),
            })?;
        }
        Ok(Self {
            session,
            notifications,
            state: RwLock::new(state),
            mutation: Mutex::new(()),
        })
    }

    /// Prompts awaiting individual turns.
    #[must_use]
    pub fn next_turn(&self) -> Vec<UserMessage> {
        self.state.read().next_turn.clone()
    }

    /// Input awaiting the next step boundary.
    #[must_use]
    pub fn next_step(&self) -> Vec<UserMessage> {
        self.state.read().next_step.clone()
    }

    /// Whether either list contains work.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        let state = self.state.read();
        !state.next_turn.is_empty() || !state.next_step.is_empty()
    }

    /// Durably cancels all pending input, next-step before next-turn.
    ///
    /// # Errors
    ///
    /// Returns validation or durable append failures.
    pub fn clear(&self) -> Result<(), InboxError> {
        self.splice(InboxTarget::NextStep, 0.0, f64::INFINITY, Vec::new())?;
        self.splice(InboxTarget::NextTurn, 0.0, f64::INFINITY, Vec::new())?;
        Ok(())
    }

    /// Removes the complete batch proposed for one step and publishes claims.
    ///
    /// Next-step messages precede the optional queued turn.
    ///
    /// # Errors
    ///
    /// Returns validation or durable append failures.
    pub fn claim(&self, target: InboxTarget, turn: u64) -> Result<Vec<UserMessage>, InboxError> {
        let mut claimed =
            self.mutate(InboxTarget::NextStep, 0.0, f64::INFINITY, Vec::new(), false)?;
        if target == InboxTarget::NextTurn {
            claimed.extend(self.mutate(InboxTarget::NextTurn, 0.0, 1.0, Vec::new(), false)?);
        }
        for message in &claimed {
            self.notifications.claimed(message, turn);
        }
        Ok(claimed)
    }

    /// Appends one pending message.
    ///
    /// # Errors
    ///
    /// Returns for duplicate identity or durable append failure.
    pub fn append(&self, target: InboxTarget, message: UserMessage) -> Result<(), InboxError> {
        let outcome = {
            let _guard = self
                .mutation
                .try_lock()
                .ok_or(InboxError::ReentrantMutation)?;
            let start = self.state.read().list(target).len();
            self.mutate_locked(target, start, 0, vec![message], true)?
        };
        self.publish(outcome, true);
        Ok(())
    }

    /// Prepends one pending message.
    ///
    /// # Errors
    ///
    /// Returns for duplicate identity or durable append failure.
    pub fn prepend(&self, target: InboxTarget, message: UserMessage) -> Result<(), InboxError> {
        let outcome = {
            let _guard = self
                .mutation
                .try_lock()
                .ok_or(InboxError::ReentrantMutation)?;
            self.mutate_locked(target, 0, 0, vec![message], true)?
        };
        self.publish(outcome, true);
        Ok(())
    }

    /// Replaces one pending message in place.
    ///
    /// # Errors
    ///
    /// Returns for duplicate replacement identity or durable append failure.
    pub fn replace(
        &self,
        message_id: &MessageId,
        replacement: UserMessage,
    ) -> Result<bool, InboxError> {
        let outcome = {
            let _guard = self
                .mutation
                .try_lock()
                .ok_or(InboxError::ReentrantMutation)?;
            let Some((target, index)) = self.state.read().locate(message_id) else {
                return Ok(false);
            };
            self.mutate_locked(target, index, 1, vec![replacement], true)?
        };
        self.publish(outcome, true);
        Ok(true)
    }

    /// Removes one pending message as a durable cancellation.
    ///
    /// # Errors
    ///
    /// Returns validation or durable append failures.
    pub fn remove(&self, message_id: &MessageId) -> Result<bool, InboxError> {
        let outcome = {
            let _guard = self
                .mutation
                .try_lock()
                .ok_or(InboxError::ReentrantMutation)?;
            let Some((target, index)) = self.state.read().locate(message_id) else {
                return Ok(false);
            };
            self.mutate_locked(target, index, 1, Vec::new(), true)?
        };
        self.publish(outcome, true);
        Ok(true)
    }

    /// Applies JavaScript splice normalization and durably records the result.
    ///
    /// # Errors
    ///
    /// Returns duplicate identity, validation, or durable append failures.
    pub fn splice(
        &self,
        target: InboxTarget,
        start: f64,
        delete_count: f64,
        inserted: Vec<UserMessage>,
    ) -> Result<Vec<UserMessage>, InboxError> {
        self.mutate(target, start, delete_count, inserted, true)
    }

    fn mutate(
        &self,
        target: InboxTarget,
        start: f64,
        delete_count: f64,
        inserted: Vec<UserMessage>,
        discard_removed: bool,
    ) -> Result<Vec<UserMessage>, InboxError> {
        let outcome = {
            let _guard = self
                .mutation
                .try_lock()
                .ok_or(InboxError::ReentrantMutation)?;
            let len = self.state.read().list(target).len();
            let actual_start = normalize_start(start, len);
            let actual_delete = normalize_delete_count(delete_count, len - actual_start);
            self.mutate_locked(
                target,
                actual_start,
                actual_delete,
                inserted,
                discard_removed,
            )?
        };
        Ok(self.publish(outcome, discard_removed))
    }

    fn mutate_locked(
        &self,
        target: InboxTarget,
        start: usize,
        removed_count: usize,
        inserted: Vec<UserMessage>,
        discard_removed: bool,
    ) -> Result<MutationOutcome, InboxError> {
        if removed_count == 0 && inserted.is_empty() {
            return Ok(MutationOutcome {
                removed: Vec::new(),
                inserted: Vec::new(),
            });
        }
        let splice = InboxSplice {
            target,
            start: u64::try_from(start).map_err(|_| InboxError::InvalidSplice)?,
            removed_count: (removed_count != 0)
                .then(|| u64::try_from(removed_count).map_err(|_| InboxError::InvalidSplice))
                .transpose()?,
            inserted,
            outcome: (discard_removed && removed_count > 0).then_some(InboxOutcome::Canceled),
        };
        validate_splice(&self.state.read(), &splice)?;
        let event = self.session.append(
            "agent/inbox/spliced",
            serde_json::to_value(&splice)
                .map_err(|error| InboxError::Committed(error.to_string()))?,
            AppendOptions::default(),
        )?;
        let committed = serde_json::from_value::<InboxSplice>(event.data)
            .map_err(|error| InboxError::Committed(error.to_string()))?;
        let removed = apply_splice(&mut self.state.write(), &committed)?;
        Ok(MutationOutcome {
            removed,
            inserted: committed.inserted,
        })
    }

    fn publish(&self, outcome: MutationOutcome, discard_removed: bool) -> Vec<UserMessage> {
        if discard_removed {
            for message in &outcome.removed {
                self.notifications.discarded(message);
            }
        }
        for message in &outcome.inserted {
            self.notifications.inserted(message);
        }
        outcome.removed
    }
}

fn normalize_start(value: f64, length: usize) -> usize {
    let truncated = if value.is_nan() { 0.0 } else { value.trunc() };
    if truncated < 0.0 {
        length.saturating_sub(saturating_usize(-truncated))
    } else {
        saturating_usize(truncated).min(length)
    }
}

fn normalize_delete_count(value: f64, available: usize) -> usize {
    let truncated = if value.is_nan() { 0.0 } else { value.trunc() };
    if truncated <= 0.0 {
        0
    } else {
        saturating_usize(truncated).min(available)
    }
}

/// Rust float-to-integer casts saturate, exactly matching the clamp needed
/// after JavaScript `Math.trunc`; callers establish non-negativity first.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn saturating_usize(value: f64) -> usize {
    value as usize
}

fn apply_splice(
    state: &mut InboxState,
    splice: &InboxSplice,
) -> Result<Vec<UserMessage>, InboxError> {
    validate_splice(state, splice)?;
    let start = usize::try_from(splice.start).map_err(|_| InboxError::InvalidSplice)?;
    let removed = usize::try_from(splice.removed_count.unwrap_or(0))
        .map_err(|_| InboxError::InvalidSplice)?;
    Ok(state
        .list_mut(splice.target)
        .splice(start..start + removed, splice.inserted.clone())
        .collect())
}

fn validate_splice(state: &InboxState, splice: &InboxSplice) -> Result<(), InboxError> {
    let inbox = state.list(splice.target);
    let start = usize::try_from(splice.start).map_err(|_| InboxError::InvalidSplice)?;
    let removed = usize::try_from(splice.removed_count.unwrap_or(0))
        .map_err(|_| InboxError::InvalidSplice)?;
    if start > inbox.len() || removed > inbox.len() - start {
        return Err(InboxError::InvalidSplice);
    }
    let mut candidate = inbox.clone();
    candidate.splice(start..start + removed, splice.inserted.clone());
    let (first, second) = match splice.target {
        InboxTarget::NextTurn => (&candidate, &state.next_step),
        InboxTarget::NextStep => (&state.next_turn, &candidate),
    };
    let mut ids = std::collections::HashSet::new();
    for message in first.iter().chain(second) {
        let id = message.id().as_str().to_owned();
        if !ids.insert(id.clone()) {
            return Err(InboxError::DuplicateMessage(id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use seekdeep_core::session::{SessionHeader, SessionId};
    use seekdeep_llm::{ContentBlock, MessageSource};

    use super::*;

    #[derive(Default)]
    struct Notifications(Mutex<Vec<String>>);

    impl InboxNotifications for Notifications {
        fn inserted(&self, message: &UserMessage) {
            self.0.lock().push(format!("inserted:{}", message.id()));
        }

        fn discarded(&self, message: &UserMessage) {
            self.0.lock().push(format!("discarded:{}", message.id()));
        }

        fn claimed(&self, message: &UserMessage, turn: u64) {
            self.0
                .lock()
                .push(format!("claimed:{turn}:{}", message.id()));
        }
    }

    fn message(text: &str) -> UserMessage {
        UserMessage::new(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            MessageSource::user(),
        )
    }

    fn session() -> Arc<Session> {
        let id = SessionId::new("agent");
        Session::create(&id, None, Some(SessionHeader::new(id.clone()))).expect("session")
    }

    #[test]
    fn replacement_identity_and_clear_are_durable() {
        let session = session();
        let notifications = Arc::new(Notifications::default());
        let inbox = Inbox::new(session.clone(), notifications.clone()).expect("inbox");
        let first = message("first");
        let first_id = first.id().clone();
        let replacement = message("replacement");
        inbox.append(InboxTarget::NextTurn, first).expect("append");
        inbox
            .replace(&first_id, replacement.clone())
            .expect("replace");
        assert_eq!(inbox.next_turn(), [replacement]);
        inbox.clear().expect("clear");
        assert!(!inbox.has_pending());
        assert_eq!(
            session
                .events()
                .iter()
                .filter(|event| event.event_type == "agent/inbox/spliced")
                .count(),
            3
        );
        assert_eq!(notifications.0.lock().len(), 4);
    }

    #[test]
    fn splice_normalizes_coordinates_and_rejects_cross_list_duplicates() {
        let session = session();
        let inbox = Inbox::new(session, Arc::new(NoopInboxNotifications)).expect("inbox");
        let a = message("a");
        let b = message("b");
        inbox.append(InboxTarget::NextTurn, a.clone()).expect("a");
        inbox
            .splice(InboxTarget::NextTurn, f64::NAN, -4.0, vec![b.clone()])
            .expect("splice");
        assert_eq!(inbox.next_turn(), [b, a.clone()]);
        assert!(matches!(
            inbox.append(InboxTarget::NextStep, a),
            Err(InboxError::DuplicateMessage(_))
        ));
    }

    #[test]
    fn replay_rejects_invalid_splice() {
        let id = SessionId::new("bad");
        let session = Session::create(&id, None, None).expect("session");
        session
            .append(
                "agent/inbox/spliced",
                serde_json::json!({
                    "target": "next-turn",
                    "start": 1,
                    "inserted": []
                }),
                AppendOptions::default(),
            )
            .expect("append accepts opaque data");
        assert!(matches!(
            Inbox::new(session, Arc::new(NoopInboxNotifications)),
            Err(InboxError::InvalidPersisted { .. })
        ));
    }
}
