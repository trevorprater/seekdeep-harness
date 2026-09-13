//! Schedule-owned use of the shared session durability barrier.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_core::{session::Session, session_store::SESSIONS};
use thiserror::Error;

/// Failure to prove that the current live prefix reached a persistence listener.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("Schedule persistence did not complete.")]
pub struct SchedulePersistenceError;

/// Requires one successful shared persistence checkpoint.
///
/// # Errors
///
/// Returns a `SchedulePersistenceError` when no listener acknowledges durability.
pub async fn flush_schedule_persistence(
    context: &Context,
    session: &Arc<Session>,
) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("schedule persistence requires the session store"))?;
    match sessions.flush(session).await {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(SchedulePersistenceError.into()),
    }
}
