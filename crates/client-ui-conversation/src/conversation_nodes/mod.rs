//! Durable Session-event definitions for the Chat conversation view.

mod command;
mod common;
mod compaction;
mod fallback;
mod inbox;
mod message;
mod retry;
mod turn_error;
mod turn_max_tokens;

pub use command::*;
pub use compaction::*;
pub use fallback::*;
pub use inbox::*;
pub use message::*;
pub use retry::*;
pub use turn_error::*;
pub use turn_max_tokens::*;

pub use common::{
    CHAT_FINALIZED_FOLLOWUP_OFFSET, CHAT_INTERRUPTED_ASSISTANT_OFFSET,
    CHAT_INTERRUPTED_FOLLOWUP_OFFSET, CHAT_MAX_TOKENS_NOTICE_OFFSET, conversation_coordinate,
};

pub(crate) use common::*;

use seekdeep_client_runtime::AssemblerNodeDefinition;

/// Builds the currently self-contained Chat event definitions in source registration order.
#[must_use]
pub fn conversation_simple_definitions() -> Vec<AssemblerNodeDefinition> {
    let mut definitions = Vec::new();
    definitions.extend(conversation_inbox_definitions());
    definitions.push(conversation_message_definition());
    definitions.push(conversation_command_definition());
    definitions.push(conversation_compaction_definition());
    definitions.push(conversation_retry_definition());
    definitions.push(conversation_turn_error_definition());
    definitions.push(conversation_turn_max_tokens_definition());
    definitions
}
