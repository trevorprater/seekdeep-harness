//! Conversation UI semantic core and Rust/WASM surfaces.

mod metrics;
mod submission;

#[cfg(not(target_arch = "wasm32"))]
mod images;

#[cfg(target_arch = "wasm32")]
mod browser_approval_panel;
#[cfg(target_arch = "wasm32")]
mod browser_assistant;
#[cfg(target_arch = "wasm32")]
mod browser_chat_seat;
#[cfg(target_arch = "wasm32")]
mod browser_chat_store;
#[cfg(target_arch = "wasm32")]
mod browser_chat_view;
#[cfg(target_arch = "wasm32")]
mod browser_command;
#[cfg(target_arch = "wasm32")]
mod browser_context_body;
#[cfg(target_arch = "wasm32")]
mod browser_context_meter;
#[cfg(target_arch = "wasm32")]
mod browser_conversation_session;
#[cfg(target_arch = "wasm32")]
mod browser_details_panel;
#[cfg(target_arch = "wasm32")]
mod browser_empty_hero;
#[cfg(target_arch = "wasm32")]
mod browser_enter_behavior;
#[cfg(target_arch = "wasm32")]
mod browser_image_labels;
#[cfg(target_arch = "wasm32")]
mod browser_message_actions;
#[cfg(target_arch = "wasm32")]
mod browser_message_chrome;
#[cfg(target_arch = "wasm32")]
mod browser_message_item;
#[cfg(target_arch = "wasm32")]
mod browser_permission_select;
#[cfg(target_arch = "wasm32")]
mod browser_queue_dock;
#[cfg(target_arch = "wasm32")]
mod browser_queue_face;
#[cfg(target_arch = "wasm32")]
mod browser_reasoning;
#[cfg(target_arch = "wasm32")]
mod browser_register_node_renderers;
#[cfg(target_arch = "wasm32")]
mod browser_stats_line;
#[cfg(target_arch = "wasm32")]
mod browser_todo_panel;
#[cfg(target_arch = "wasm32")]
mod browser_tool_node_reader;
#[cfg(target_arch = "wasm32")]
mod browser_turn_tail;

#[cfg(not(target_arch = "wasm32"))]
mod host;

pub use metrics::*;
pub use submission::*;

#[cfg(not(target_arch = "wasm32"))]
pub use images::*;

#[cfg(target_arch = "wasm32")]
pub use browser_approval_panel::*;
#[cfg(target_arch = "wasm32")]
pub use browser_assistant::*;
#[cfg(target_arch = "wasm32")]
pub use browser_chat_seat::*;
#[cfg(target_arch = "wasm32")]
pub use browser_chat_store::*;
#[cfg(target_arch = "wasm32")]
pub use browser_chat_view::*;
#[cfg(target_arch = "wasm32")]
pub use browser_command::*;
#[cfg(target_arch = "wasm32")]
pub use browser_context_body::*;
#[cfg(target_arch = "wasm32")]
pub use browser_context_meter::*;
#[cfg(target_arch = "wasm32")]
pub use browser_conversation_session::*;
#[cfg(target_arch = "wasm32")]
pub use browser_details_panel::*;
#[cfg(target_arch = "wasm32")]
pub use browser_empty_hero::*;
#[cfg(target_arch = "wasm32")]
pub use browser_enter_behavior::*;
#[cfg(target_arch = "wasm32")]
pub use browser_image_labels::*;
#[cfg(target_arch = "wasm32")]
pub use browser_message_actions::*;
#[cfg(target_arch = "wasm32")]
pub use browser_message_chrome::*;
#[cfg(target_arch = "wasm32")]
pub use browser_message_item::*;
#[cfg(target_arch = "wasm32")]
pub use browser_permission_select::*;
#[cfg(target_arch = "wasm32")]
pub use browser_queue_dock::*;
#[cfg(target_arch = "wasm32")]
pub use browser_queue_face::*;
#[cfg(target_arch = "wasm32")]
pub use browser_reasoning::*;
#[cfg(target_arch = "wasm32")]
pub use browser_register_node_renderers::*;
#[cfg(target_arch = "wasm32")]
pub use browser_stats_line::*;
#[cfg(target_arch = "wasm32")]
pub use browser_todo_panel::*;
#[cfg(target_arch = "wasm32")]
pub use browser_tool_node_reader::*;
#[cfg(target_arch = "wasm32")]
pub use browser_turn_tail::*;

#[cfg(not(target_arch = "wasm32"))]
pub use host::*;

/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-ui-conversation-invariant";
