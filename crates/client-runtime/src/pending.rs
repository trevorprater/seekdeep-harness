//! Carrier-owned pending Host interaction envelope.

use std::{cell::Cell, rc::Rc};

use futures::future::LocalBoxFuture;
use seekdeep_identity::{RpcId, SessionId};
use serde_json::Value;

/// Pending interaction domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingKind {
    /// Tool approval request.
    Approval,
    /// Structured user question.
    Question,
}

/// Session-list summary of the interaction blocking progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingInteractionStatus {
    /// Tool approval.
    Approval,
    /// Plan review approval.
    PlanReview,
    /// Structured question.
    Question,
}

/// Client-response envelope with the request correlation id backfilled.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingClientResponse {
    /// Initiator-minted correlation identity.
    pub rpc_id: RpcId,
    /// Domain-encoded result shell.
    pub result: Value,
}

/// Pending carrier response future.
pub type PendingResponseFuture = LocalBoxFuture<'static, Result<Value, String>>;
/// Injected response carrier.
pub type PendingResponder = Rc<dyn Fn(PendingClientResponse) -> PendingResponseFuture>;

/// One immutable pending render face plus private response carrier.
pub struct PendingWait {
    /// Interaction kind.
    pub kind: PendingKind,
    /// Stable `<prefix>:<rpcId>` render key.
    pub key: String,
    /// Owning Session.
    pub session_id: SessionId,
    /// Requested frame domain fields.
    pub payload: Value,
    rpc_id: RpcId,
    responder: PendingResponder,
    settled: Cell<bool>,
}

impl std::fmt::Debug for PendingWait {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingWait")
            .field("kind", &self.kind)
            .field("key", &self.key)
            .field("session_id", &self.session_id)
            .field("payload", &self.payload)
            .field("settled", &self.settled.get())
            .finish_non_exhaustive()
    }
}

/// Response after authoritative settlement.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("pending wait {key} is already settled")]
pub struct PendingWaitSettled {
    /// Stable wait key.
    pub key: String,
}

impl PendingWait {
    /// Mints one pending wait from a requested carrier frame.
    #[must_use]
    pub fn new(
        kind: PendingKind,
        rpc_id: RpcId,
        session_id: SessionId,
        payload: Value,
        responder: PendingResponder,
    ) -> Self {
        let prefix = match kind {
            PendingKind::Approval => 'a',
            PendingKind::Question => 'q',
        };
        Self {
            kind,
            key: format!("{prefix}:{rpc_id}"),
            session_id,
            payload,
            rpc_id,
            responder,
            settled: Cell::new(false),
        }
    }

    /// Sends one domain-encoded result with the private `rpcId` restored.
    ///
    /// # Errors
    ///
    /// Returns synchronously once the authoritative resolved frame settled this wait.
    pub fn respond(&self, result: Value) -> Result<PendingResponseFuture, PendingWaitSettled> {
        if self.settled.get() {
            return Err(PendingWaitSettled {
                key: self.key.clone(),
            });
        }
        Ok((self.responder)(PendingClientResponse {
            rpc_id: self.rpc_id.clone(),
            result,
        }))
    }

    /// Marks authoritative settlement; every later response fails synchronously.
    pub fn mark_settled(&self) {
        self.settled.set(true);
    }
}
