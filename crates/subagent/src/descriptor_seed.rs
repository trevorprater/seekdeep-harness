//! Seeding of a continuable child's durable descriptor event.

use seekdeep_core::session::{AppendOptions, Session, SessionEvent, SessionId};

use crate::descriptor::SubagentDescriptorData;

/// Builds the child's creation seed: any inherited parent-history prefix
/// followed by one model-hidden descriptor event.
///
/// # Errors
///
/// Returns session-staging or append failures.
pub fn seed_descriptor_turn(
    child_id: &SessionId,
    seed: Option<Vec<SessionEvent>>,
    descriptor: &SubagentDescriptorData,
) -> anyhow::Result<Vec<SessionEvent>> {
    let staged = Session::create(child_id, seed, None)?;
    staged.append(
        "subagent/descriptor",
        serde_json::to_value(descriptor)?,
        AppendOptions::default(),
    )?;
    Ok(staged.events())
}
