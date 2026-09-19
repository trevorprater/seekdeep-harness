//! Durable steering identity reconstruction from agent inbox splices.

use std::collections::BTreeSet;

/// Host inbox target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxTarget {
    /// Queued for the next Turn.
    NextTurn,
    /// Claimed as mid-Turn steering for the next Step.
    NextStep,
}

/// Minimal durable pending identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingIdentity {
    /// Message identity.
    pub id: String,
}

/// One host-validated inbox splice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxSplice {
    /// Target inbox.
    pub target: InboxTarget,
    /// Start index.
    pub start: usize,
    /// Removed identity count.
    pub removed_count: usize,
    /// Inserted identities.
    pub inserted: Vec<PendingIdentity>,
    /// Whether the claim was canceled.
    pub canceled: bool,
}

/// Durable event relevant to steering reconstruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SteeringHistoryEvent {
    /// Inbox splice.
    Splice(InboxSplice),
    /// Durable user message and its source kind.
    UserMessage {
        /// Durable message identity.
        id: String,
        /// Durable source kind.
        source_kind: String,
    },
    /// Unrelated event.
    Other,
}

/// Incremental steering replay state.
#[derive(Default)]
pub struct SteeringHistory {
    next_turn: Vec<PendingIdentity>,
    next_step: Vec<PendingIdentity>,
    claimed_next_step: BTreeSet<String>,
}

impl SteeringHistory {
    /// Clears all replay state before rebuilding a window.
    pub fn reset(&mut self) {
        self.next_turn.clear();
        self.next_step.clear();
        self.claimed_next_step.clear();
    }

    /// Applies one event and returns true only for durable human steering.
    pub fn apply(&mut self, event: &SteeringHistoryEvent) -> bool {
        match event {
            SteeringHistoryEvent::Splice(splice) => {
                self.apply_splice(splice);
                false
            }
            SteeringHistoryEvent::UserMessage { id, source_kind } => {
                self.claimed_next_step.remove(id) && source_kind == "user"
            }
            SteeringHistoryEvent::Other => false,
        }
    }

    fn apply_splice(&mut self, splice: &InboxSplice) {
        let inbox = match splice.target {
            InboxTarget::NextTurn => &mut self.next_turn,
            InboxTarget::NextStep => &mut self.next_step,
        };
        let end = splice
            .start
            .saturating_add(splice.removed_count)
            .min(inbox.len());
        let removed = inbox
            .splice(splice.start.min(inbox.len())..end, splice.inserted.clone())
            .collect::<Vec<_>>();
        for identity in &splice.inserted {
            self.claimed_next_step.remove(&identity.id);
        }
        if splice.target != InboxTarget::NextStep || splice.canceled {
            return;
        }
        self.claimed_next_step
            .extend(removed.into_iter().map(|identity| identity.id));
    }
}
