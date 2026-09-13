//! Typed browser faces and exact Slot registration declarations.

use std::sync::Arc;

use seekdeep_cordis_client_runner::{CordisRunOrchestrator, DynamicCordisClientRuntime};
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
};

use crate::{CordisDynamicPort, CordisInventory, CordisRunCardStore};

/// Services required by the complete browser plugin.
pub const UI_CORDIS_INJECT: [&str; 6] = [
    "slots",
    "locale",
    "inputTriggers",
    "remote",
    "remote.dynamicCordisRunner",
    "dynamicCordisRunner",
];

/// Package-owned keyed child Slot hosted by eligible Run cards.
pub const CORDIS_TOOL_VIEW_SLOT: &str = "tool.view.cordis";

/// Owner currency delivered to a dynamic Package's business view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CordisToolViewOwnerProps {
    /// Stable Plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Immutable Package identity.
    pub package_id: CordisDynamicPackageId,
    /// Exact activation identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
}

/// One exact static UI contribution installed by the browser plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CordisSlotRegistration {
    /// Parent Slot.
    pub slot: &'static str,
    /// Keyed entry identity, when the parent is keyed.
    pub key: Option<&'static str>,
    /// Unkeyed entry identity, when the parent uses ids.
    pub id: Option<&'static str>,
    /// Whether this entry declares the Package-owned child Slot.
    pub declares_tool_view: bool,
}

/// Exact registration inventory installed by `ui-cordis`.
pub const UI_CORDIS_REGISTRATIONS: [CordisSlotRegistration; 5] = [
    CordisSlotRegistration {
        slot: "sidebar.footer.action",
        key: None,
        id: Some("cordis-panel"),
        declares_tool_view: false,
    },
    CordisSlotRegistration {
        slot: "tool.call.toolview",
        key: Some("cordis_define"),
        id: None,
        declares_tool_view: false,
    },
    CordisSlotRegistration {
        slot: "tool.call.toolview",
        key: Some("cordis_run"),
        id: None,
        declares_tool_view: true,
    },
    CordisSlotRegistration {
        slot: "tool.call.toolview",
        key: Some("cordis_stop"),
        id: None,
        declares_tool_view: false,
    },
    CordisSlotRegistration {
        slot: "tool.call.toolview",
        key: Some("cordis_undefine"),
        id: None,
        declares_tool_view: false,
    },
];

/// Live facts consumed by a Define card.
#[derive(Clone)]
pub struct CordisCardFace {
    /// Shared Host inventory.
    pub inventory: Arc<CordisInventory>,
    /// Page-local Client activation runtime.
    pub runtime: Arc<DynamicCordisClientRuntime>,
}

/// Live facts consumed by a Run card.
#[derive(Clone)]
pub struct CordisRunCardFace {
    /// Shared card facts.
    pub card: CordisCardFace,
    /// Session-local latest-card Store.
    pub run_cards: Arc<CordisRunCardStore>,
    /// Page-side activation orchestrator.
    pub orchestrator: Arc<CordisRunOrchestrator>,
}

/// Frame-wide panel state and lifecycle capabilities.
#[derive(Clone)]
pub struct CordisPanelFace {
    /// Shared Host inventory.
    pub inventory: Arc<CordisInventory>,
    /// Page-local Client activation runtime.
    pub runtime: Arc<DynamicCordisClientRuntime>,
    /// Page-side approval and activation owner.
    pub orchestrator: Arc<CordisRunOrchestrator>,
    /// Host lifecycle RPC boundary.
    pub port: Arc<dyn CordisDynamicPort>,
}
