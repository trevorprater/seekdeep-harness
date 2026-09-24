//! One test binary per crate: every top-level test file is a module here.

mod assistant_timing_parity;
mod conversation_assembler_parity;
mod conversation_classifier_parity;
mod conversation_location_parity;
mod conversation_prematch_parity;
mod conversation_registry_parity;
mod event_script_contract;
mod fake_api_contract;
mod misc_parity;
mod node_half_parity;
mod notifier_parity;
mod partial_parity;
mod pending_queue_steering_parity;
mod projection_store_parity;
mod provide_parity;
mod request_inspection_parity;
mod session_manager_parity;
mod session_parity;
mod session_service_parity;
mod settings_scope_contract;
mod slots_service_parity;
mod store_parity;
mod subagent_lineage_parity;
mod tool_call_tree_parity;
mod workspace_service_parity;

#[path = "support/event_script.rs"]
mod event_script;
#[path = "support/fake_api.rs"]
mod fake_api;
