//! Trajectory timeline, record identity, and virtual-ledger semantics.

mod assistant_definition;
#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
mod browser_timeline;
mod compaction_definitions;
mod contract;
mod duration_store;
mod layout;
mod message_definitions;
mod preview;
mod record;
mod request_header;
mod search_index;
mod snapshot_builder;
mod timeline;
mod timeline_controller;
mod tool_definition;
mod virtual_rows;

pub use assistant_definition::*;
#[cfg(target_arch = "wasm32")]
pub use browser::*;
#[cfg(target_arch = "wasm32")]
pub use browser_timeline::*;
pub use compaction_definitions::*;
pub use contract::*;
pub use duration_store::*;
pub use layout::*;
pub use message_definitions::*;
pub use preview::*;
pub use record::*;
pub use request_header::*;
pub use search_index::*;
pub use snapshot_builder::*;
pub use timeline::*;
pub use timeline_controller::*;
pub use tool_definition::*;
pub use virtual_rows::*;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-trajectory";
/// Browser plugin dependencies in exact source order.
pub const INJECT: &[&str] = &[
    "slots",
    "conversationEvents",
    "conversationViews",
    "sessions",
    "locale",
];
/// Dictionary namespace owned by the browser plugin.
pub const LOCALE_NAMESPACE: &str = "trajectory";
/// Browser-wide duration preference persistence key.
pub const DURATION_PERSISTENCE_KEY: &str = "dsh.trajectory.duration";
/// Initial duration preference.
pub const DEFAULT_ACTUAL_DURATION: bool = false;
/// Compiled simple trajectory component stylesheet.
pub const SIMPLE_COMPONENT_STYLES: &str = include_str!("../data/simple-components.css");
/// Simplified-Chinese trajectory copy in source order.
pub const TRAJECTORY_ZH: &[(&str, &str)] = &[
    ("view.trajectory", "轨迹"),
    ("toolbar.aria", "轨迹工具栏"),
    ("toolbar.duration", "Duration"),
    ("toolbar.useActualDuration", "Use actual duration"),
    ("toolbar.useEqualWidth", "Use equal-width operations"),
    ("toolbar.actualTime", "实际时间"),
    ("toolbar.turns", "Turns"),
    ("toolbar.expandTurns", "Expand turns"),
    ("toolbar.collapseTurns", "Collapse turns"),
    ("toolbar.calls", "Calls"),
    ("toolbar.expandCalls", "Expand calls"),
    ("toolbar.collapseCalls", "Collapse calls"),
    ("toolbar.search", "搜索轨迹"),
    ("toolbar.searchPlaceholder", "搜索"),
];
/// English trajectory copy in source order.
pub const TRAJECTORY_EN: &[(&str, &str)] = &[
    ("view.trajectory", "Trajectory"),
    ("toolbar.aria", "Trajectory toolbar"),
    ("toolbar.duration", "Duration"),
    ("toolbar.useActualDuration", "Use actual duration"),
    ("toolbar.useEqualWidth", "Use equal-width operations"),
    ("toolbar.actualTime", "Actual time"),
    ("toolbar.turns", "Turns"),
    ("toolbar.expandTurns", "Expand turns"),
    ("toolbar.collapseTurns", "Collapse turns"),
    ("toolbar.calls", "Calls"),
    ("toolbar.expandCalls", "Expand calls"),
    ("toolbar.collapseCalls", "Collapse calls"),
    ("toolbar.search", "Search trajectory"),
    ("toolbar.searchPlaceholder", "Search"),
];

/// Builds every native trajectory Event Definition in Client registration order.
#[must_use]
pub fn trajectory_event_definitions() -> Vec<seekdeep_client_runtime::AssemblerNodeDefinition> {
    let mut definitions = Vec::new();
    definitions.extend(trajectory_message_definitions());
    definitions.push(trajectory_request_header_definition());
    definitions.extend(trajectory_assistant_definitions());
    definitions.push(trajectory_tool_definition());
    definitions.extend(trajectory_compaction_definitions());
    definitions
}

/// Builds the no-op Host half of this browser-owned plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
