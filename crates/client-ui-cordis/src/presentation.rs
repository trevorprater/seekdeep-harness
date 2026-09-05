//! Pure derivation for Cordis card, panel, and input-trigger presentation.

use std::collections::{BTreeMap, BTreeSet};

use seekdeep_cordis_client_runner::{CordisRunActivity, DynamicCordisLivePackage};
use seekdeep_cordis_dynamic_types::{
    ApprovalRequestId, CordisDynamicPackageId, CordisDynamicPluginId,
    DynamicCordisInventoryPackage, DynamicCordisInventoryRow, DynamicCordisRunMode,
};
use seekdeep_identity::SessionId;

use crate::{
    CordisActionCard, CordisDefineCard, CordisRunCard, CordisRunCardPointer, CordisToolState,
    CordisToolViewKey, CordisVisibleStatus, cordis_tool_view_key, cordis_visible_status,
    package_of,
};

/// Selected source tab in an expanded Define card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CordisSourceTab {
    /// Browser-half function body.
    Client,
    /// Native Host function body.
    Host,
}

/// Visible reading for one historical Define card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CordisDefineReading {
    /// Package has no live activation.
    Idle,
    /// Host is active and Client is not loaded in this page.
    ClientPending,
    /// Every required half is active.
    Running,
    /// Plugin was explicitly removed or disappeared from inventory.
    Removed,
}

impl CordisDefineReading {
    /// Locale key for the visible reading.
    #[must_use]
    pub const fn locale_key(self) -> &'static str {
        match self {
            Self::Idle => "status.idle",
            Self::ClientPending => "status.clientPending",
            Self::Running => "status.running",
            Self::Removed => "status.removed",
        }
    }
}

/// Complete non-local presentation state for one Define card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CordisDefineRowModel {
    /// Frozen card data.
    pub card: CordisDefineCard,
    /// Live status reading.
    pub reading: CordisDefineReading,
    /// Display name after call-id fallback.
    pub name: String,
    /// Whether disclosure can open.
    pub expandable: bool,
    /// Accessible lifecycle message key.
    pub a11y_state_key: Option<&'static str>,
    /// Effective source tab after availability fallback.
    pub active_source: CordisSourceTab,
    /// Effective source body.
    pub active_code: Option<String>,
}

/// Derives one Define row without retaining presentation-only state in the session log.
#[must_use]
pub fn cordis_define_row_model(
    card: CordisDefineCard,
    call_id: &str,
    inventory: &[DynamicCordisInventoryRow],
    removed: &BTreeSet<CordisDynamicPluginId>,
    loaded: &[DynamicCordisLivePackage],
    selected_source: CordisSourceTab,
) -> CordisDefineRowModel {
    let row = card
        .plugin_id
        .as_ref()
        .and_then(|plugin_id| inventory.iter().find(|row| row.plugin_id == *plugin_id));
    let reading = if card
        .plugin_id
        .as_ref()
        .is_some_and(|plugin_id| removed.contains(plugin_id))
    {
        CordisDefineReading::Removed
    } else if let (Some(row), Some(package_id)) = (row, card.package_id.as_ref()) {
        match cordis_visible_status(row, package_id, loaded) {
            CordisVisibleStatus::Idle => CordisDefineReading::Idle,
            CordisVisibleStatus::ClientPending => CordisDefineReading::ClientPending,
            CordisVisibleStatus::Running => CordisDefineReading::Running,
        }
    } else {
        CordisDefineReading::Idle
    };
    let active_source = if card.client_code.is_some()
        && (selected_source == CordisSourceTab::Client || card.host_code.is_none())
    {
        CordisSourceTab::Client
    } else {
        CordisSourceTab::Host
    };
    let active_code = match active_source {
        CordisSourceTab::Client => card.client_code.clone(),
        CordisSourceTab::Host => card.host_code.clone(),
    };
    let name = card.name.clone().unwrap_or_else(|| call_id.to_owned());
    let expandable =
        card.host_code.is_some() || card.client_code.is_some() || card.output.is_some();
    let a11y_state_key = match card.state {
        CordisToolState::Running => Some("a11y.defining"),
        CordisToolState::Error => Some("a11y.failed"),
        CordisToolState::Stopped => Some("a11y.stopped"),
        CordisToolState::Ok => None,
    };
    CordisDefineRowModel {
        card,
        reading,
        name,
        expandable,
        a11y_state_key,
        active_source,
        active_code,
    }
}

/// Visible reading for one historical Run card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CordisRunReading {
    /// Package has no live activation.
    Idle,
    /// Activation waits for explicit approval.
    AwaitingApproval,
    /// Latest exact attempt failed.
    Failed,
    /// Host is active and Client is not loaded in this page.
    ClientPending,
    /// Every required half is active.
    Running,
    /// Plugin was removed.
    Removed,
    /// A later successful card owns the Package business view.
    Superseded,
}

impl CordisRunReading {
    /// Locale key for the visible reading.
    #[must_use]
    pub const fn locale_key(self) -> &'static str {
        match self {
            Self::Idle => "status.idle",
            Self::AwaitingApproval => "status.awaitingApproval",
            Self::Failed => "status.failed",
            Self::ClientPending => "status.clientPending",
            Self::Running => "status.running",
            Self::Removed => "status.removed",
            Self::Superseded => "status.superseded",
        }
    }
}

/// Complete non-local presentation state for one Run card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CordisRunRowModel {
    /// Frozen card data.
    pub card: CordisRunCard,
    /// Stable Package business-view key when the card can own one.
    pub key: Option<CordisToolViewKey>,
    /// Live status reading.
    pub reading: CordisRunReading,
    /// Plugin/Package or error summary.
    pub summary: String,
    /// Exact attempt failure detail when available.
    pub failure_message: Option<String>,
    /// Whether this card owns the Package business view.
    pub show_business: bool,
}

/// Derives one Run row and exact-generation business-view ownership.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn cordis_run_row_model(
    card: CordisRunCard,
    call_id: &str,
    inventory: &[DynamicCordisInventoryRow],
    removed: &BTreeSet<CordisDynamicPluginId>,
    loaded: &[DynamicCordisLivePackage],
    latest_cards: &BTreeMap<CordisToolViewKey, CordisRunCardPointer>,
    active_runs: &BTreeMap<CordisDynamicPluginId, CordisRunActivity>,
) -> CordisRunRowModel {
    let key = if card.state == CordisToolState::Ok {
        card.plugin_id
            .as_ref()
            .zip(card.package_id.as_ref())
            .zip(card.plugin_run_id.as_ref())
            .zip(card.seq)
            .map(|(((plugin_id, package_id), _), _)| cordis_tool_view_key(plugin_id, package_id))
    } else {
        None
    };
    let row = card
        .plugin_id
        .as_ref()
        .and_then(|plugin_id| inventory.iter().find(|row| row.plugin_id == *plugin_id));
    let pointer = key.as_ref().and_then(|key| latest_cards.get(key));
    let superseded = pointer
        .is_some_and(|pointer| pointer.call_id != call_id && pointer.seq >= card.seq.unwrap_or(0));
    let activity = card
        .plugin_id
        .as_ref()
        .and_then(|plugin_id| active_runs.get(plugin_id));
    let attempt = card.plugin_run_id.as_ref().and_then(|plugin_run_id| {
        row.and_then(|row| {
            row.latest_run
                .as_ref()
                .filter(|attempt| attempt.plugin_run_id == *plugin_run_id)
        })
    });
    let awaiting_approval = attempt.is_some_and(|attempt| {
        attempt.status == seekdeep_cordis_dynamic_types::CordisRunStatus::AwaitingApproval
    }) || card.package_id.as_ref().is_some_and(|package_id| {
        matches!(
            activity,
            Some(CordisRunActivity::AwaitingApproval {
                package_id: active_package,
                mode,
                ..
            }) if active_package == package_id && card.mode.is_none_or(|card_mode| card_mode == *mode)
        )
    });
    let reading = if card
        .plugin_id
        .as_ref()
        .is_some_and(|plugin_id| removed.contains(plugin_id))
    {
        CordisRunReading::Removed
    } else if superseded {
        CordisRunReading::Superseded
    } else if awaiting_approval {
        CordisRunReading::AwaitingApproval
    } else if attempt.is_some_and(|attempt| {
        attempt.status == seekdeep_cordis_dynamic_types::CordisRunStatus::Failed
    }) {
        CordisRunReading::Failed
    } else if let (Some(row), Some(package_id)) = (row, card.package_id.as_ref()) {
        match cordis_visible_status(row, package_id, loaded) {
            CordisVisibleStatus::Idle => CordisRunReading::Idle,
            CordisVisibleStatus::ClientPending => CordisRunReading::ClientPending,
            CordisVisibleStatus::Running => CordisRunReading::Running,
        }
    } else {
        CordisRunReading::Idle
    };
    let summary = card.error_summary.clone().unwrap_or_else(|| {
        card.plugin_id.as_ref().map_or_else(
            || call_id.to_owned(),
            |plugin_id| {
                card.package_id.as_ref().map_or_else(
                    || plugin_id.to_string(),
                    |package_id| format!("{plugin_id} · {package_id}"),
                )
            },
        )
    });
    let failure_message = attempt
        .and_then(|attempt| attempt.error.as_ref())
        .map(|error| error.message.clone());
    let show_business = reading == CordisRunReading::Running && key.is_some();
    CordisRunRowModel {
        card,
        key,
        reading,
        summary,
        failure_message,
        show_business,
    }
}

/// Complete non-local presentation state for one Stop or Remove card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CordisActionRowModel {
    /// Frozen card data.
    pub card: CordisActionCard,
    /// Whether the tool removes the Plugin instead of stopping it.
    pub remove: bool,
    /// Localized title key.
    pub title_key: &'static str,
    /// Error, Plugin identity, or call-id summary.
    pub summary: String,
}

/// Derives one Stop or Remove row.
#[must_use]
pub fn cordis_action_row_model(
    card: CordisActionCard,
    call_id: &str,
    tool_name: &str,
) -> CordisActionRowModel {
    let remove = tool_name == "cordis_undefine";
    let title_key = if remove {
        "row.removeTitle"
    } else {
        "row.stopTitle"
    };
    let summary = card.error_summary.clone().unwrap_or_else(|| {
        card.plugin_id
            .as_ref()
            .map_or_else(|| call_id.to_owned(), ToString::to_string)
    });
    CordisActionRowModel {
        card,
        remove,
        title_key,
        summary,
    }
}

/// Product-visible panel lifecycle status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CordisPanelStatus {
    /// No Host activation exists.
    Idle,
    /// A request waits for user approval.
    AwaitingApproval,
    /// Host's latest selected-package attempt failed.
    Failed,
    /// Host is active and Client is not loaded in this page.
    ClientPending,
    /// Every required half is active.
    Running,
}

impl CordisPanelStatus {
    /// Locale key for the visible reading.
    #[must_use]
    pub const fn locale_key(self) -> &'static str {
        match self {
            Self::Idle => "status.idle",
            Self::AwaitingApproval => "status.awaitingApproval",
            Self::Failed => "status.failed",
            Self::ClientPending => "status.clientPending",
            Self::Running => "status.running",
        }
    }
}

/// Merged Host inventory and page activity for one panel row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CordisPanelRowModel {
    /// Stable Plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Owning Session.
    pub agent_id: SessionId,
    /// Inventory row when the Host read has landed.
    pub listed: Option<DynamicCordisInventoryRow>,
    /// Page activity when approval or orchestration is active.
    pub activity: Option<CordisRunActivity>,
    /// Selected immutable Package.
    pub selected_package_id: Option<CordisDynamicPackageId>,
    /// Selected Package metadata.
    pub selected_package: Option<DynamicCordisInventoryPackage>,
    /// Active Package metadata.
    pub active_package: Option<DynamicCordisInventoryPackage>,
    /// Package or Plugin display name.
    pub name: String,
    /// Package or pending-request purpose.
    pub purpose: String,
    /// Visible lifecycle status.
    pub status: CordisPanelStatus,
    /// Pending approval identity.
    pub awaiting: Option<ApprovalRequestId>,
    /// Whether the page is already transitioning this Plugin.
    pub busy: bool,
    /// Run or update intent for the selected Package.
    pub run_mode: DynamicCordisRunMode,
    /// Lifecycle actions currently available to the row.
    pub actions: BTreeSet<CordisPanelAction>,
}

/// Lifecycle gesture currently available to a panel row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CordisPanelAction {
    /// Activate the selected immutable Package.
    RunSelected,
    /// Retry the active Client-required Package on this page.
    RetryClient,
    /// Stop the physical activation while retaining definitions.
    Stop,
    /// Remove the Plugin and every immutable Package.
    Remove,
}

/// Frame-wide panel projection grouped by current and other Sessions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CordisPanelModel {
    /// Current-Session rows, with approval blockers first.
    pub mine: Vec<CordisPanelRowModel>,
    /// Other-Session rows, with approval blockers first.
    pub theirs: Vec<CordisPanelRowModel>,
    /// Number of pending approvals.
    pub approvals: usize,
    /// Number of fully running Plugins.
    pub running: usize,
}

/// Derives the complete panel row set and grouping.
#[must_use]
pub fn cordis_panel_model(
    inventory: &[DynamicCordisInventoryRow],
    active_runs: &BTreeMap<CordisDynamicPluginId, CordisRunActivity>,
    loaded: &[DynamicCordisLivePackage],
    current_session: Option<&SessionId>,
    selected: &BTreeMap<CordisDynamicPluginId, CordisDynamicPackageId>,
    pending: &BTreeSet<CordisDynamicPluginId>,
) -> CordisPanelModel {
    let mut merged = BTreeMap::<
        CordisDynamicPluginId,
        (
            SessionId,
            Option<DynamicCordisInventoryRow>,
            Option<CordisRunActivity>,
        ),
    >::new();
    for listed in inventory {
        let activity = active_runs.get(&listed.plugin_id).cloned();
        let agent_id = activity
            .as_ref()
            .map_or_else(|| listed.agent_id.clone(), activity_agent_id);
        merged.insert(
            listed.plugin_id.clone(),
            (agent_id, Some(listed.clone()), activity),
        );
    }
    for (plugin_id, activity) in active_runs {
        merged
            .entry(plugin_id.clone())
            .or_insert_with(|| (activity_agent_id(activity), None, Some(activity.clone())));
    }
    let mut all = merged
        .into_iter()
        .map(|(plugin_id, (agent_id, listed, activity))| {
            panel_row(
                plugin_id,
                agent_id,
                listed.as_ref(),
                activity,
                selected,
                loaded,
                pending,
            )
        })
        .collect::<Vec<_>>();
    all.sort_by_key(|row| {
        !matches!(
            row.activity,
            Some(CordisRunActivity::AwaitingApproval { .. })
        )
    });
    let approvals = all.iter().filter(|row| row.awaiting.is_some()).count();
    let running = all
        .iter()
        .filter(|row| row.status == CordisPanelStatus::Running)
        .count();
    let (mine, theirs) = all
        .into_iter()
        .partition(|row| current_session.is_some_and(|current| row.agent_id == *current));
    CordisPanelModel {
        mine,
        theirs,
        approvals,
        running,
    }
}

/// Candidate exposed by the `@pluginId` input source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CordisTriggerCandidate {
    /// Stable Plugin identity as input text.
    pub name: String,
    /// Preferred Package purpose when known.
    pub description: Option<String>,
}

/// Filters and labels `@pluginId` candidates for one Session.
#[must_use]
pub fn cordis_trigger_candidates(
    rows: &[DynamicCordisInventoryRow],
    session_id: &SessionId,
    query: &str,
) -> Vec<CordisTriggerCandidate> {
    rows.iter()
        .filter(|row| row.agent_id == *session_id && row.plugin_id.as_str().contains(query))
        .map(|row| {
            let package_id = row
                .next_package_id
                .as_ref()
                .or(row.current_package_id.as_ref())
                .or_else(|| row.packages.last().map(|package| &package.package_id));
            let package = package_id.and_then(|package_id| package_of(row, package_id));
            CordisTriggerCandidate {
                name: row.plugin_id.to_string(),
                description: package.map(|package| package.purpose.clone()),
            }
        })
        .collect()
}

/// Builds the inserted text for one selected `@pluginId` candidate.
#[must_use]
pub fn cordis_trigger_pick(candidate: &CordisTriggerCandidate) -> String {
    format!("@{} ", candidate.name)
}

fn panel_row(
    plugin_id: CordisDynamicPluginId,
    agent_id: SessionId,
    listed: Option<&DynamicCordisInventoryRow>,
    activity: Option<CordisRunActivity>,
    selected: &BTreeMap<CordisDynamicPluginId, CordisDynamicPackageId>,
    loaded: &[DynamicCordisLivePackage],
    pending: &BTreeSet<CordisDynamicPluginId>,
) -> CordisPanelRowModel {
    let selected_package_id =
        selected_package_id_of(&plugin_id, listed, activity.as_ref(), selected);
    let selected_package = listed
        .zip(selected_package_id.as_ref())
        .and_then(|(listed, package_id)| package_of(listed, package_id))
        .cloned();
    let active_package = listed
        .and_then(|listed| {
            listed
                .active_run
                .as_ref()
                .map(|run| (listed, &run.package_id))
        })
        .and_then(|(listed, package_id)| package_of(listed, package_id))
        .cloned();
    let name = selected_package.as_ref().map_or_else(
        || match &activity {
            Some(CordisRunActivity::AwaitingApproval { name, .. }) => name.clone(),
            Some(CordisRunActivity::Orchestrating { .. }) | None => plugin_id.to_string(),
        },
        |package| package.name.clone(),
    );
    let purpose = selected_package.as_ref().map_or_else(
        || match &activity {
            Some(CordisRunActivity::AwaitingApproval { purpose, .. }) => purpose.clone(),
            Some(CordisRunActivity::Orchestrating { .. }) | None => String::new(),
        },
        |package| package.purpose.clone(),
    );
    let latest = listed.and_then(|listed| listed.latest_run.as_ref());
    let awaiting = match &activity {
        Some(CordisRunActivity::AwaitingApproval { request_id, .. }) => Some(request_id.clone()),
        Some(CordisRunActivity::Orchestrating { .. }) | None => latest
            .filter(|latest| {
                latest.status == seekdeep_cordis_dynamic_types::CordisRunStatus::AwaitingApproval
            })
            .and_then(|latest| latest.approval_request_id.clone()),
    };
    let status = panel_status(
        listed,
        latest,
        selected_package_id.as_ref(),
        awaiting.as_ref(),
        loaded,
    );
    let busy = pending.contains(&plugin_id)
        || matches!(activity, Some(CordisRunActivity::Orchestrating { .. }));
    let current_package_id = listed.and_then(|listed| listed.current_package_id.as_ref());
    let active_run = listed.and_then(|listed| listed.active_run.as_ref());
    let run_mode =
        if current_package_id.is_some() && selected_package_id.as_ref() != current_package_id {
            DynamicCordisRunMode::Update
        } else {
            DynamicCordisRunMode::Run
        };
    let can_run_selected = awaiting.is_none()
        && listed.is_some()
        && selected_package_id.is_some()
        && (active_run.is_none()
            || active_run
                .is_some_and(|active| Some(&active.package_id) != selected_package_id.as_ref()));
    let can_retry_client = awaiting.is_none()
        && status == CordisPanelStatus::ClientPending
        && active_package.is_some()
        && active_run
            .is_some_and(|active| Some(&active.package_id) == selected_package_id.as_ref());
    let mut actions = BTreeSet::new();
    if can_run_selected {
        actions.insert(CordisPanelAction::RunSelected);
    }
    if can_retry_client {
        actions.insert(CordisPanelAction::RetryClient);
    }
    if awaiting.is_none() && active_run.is_some() {
        actions.insert(CordisPanelAction::Stop);
    }
    if awaiting.is_none() && listed.is_some() {
        actions.insert(CordisPanelAction::Remove);
    }
    CordisPanelRowModel {
        plugin_id,
        agent_id,
        listed: listed.cloned(),
        activity,
        selected_package_id,
        selected_package,
        active_package,
        name,
        purpose,
        status,
        awaiting,
        busy,
        run_mode,
        actions,
    }
}

fn selected_package_id_of(
    plugin_id: &CordisDynamicPluginId,
    listed: Option<&DynamicCordisInventoryRow>,
    activity: Option<&CordisRunActivity>,
    selected: &BTreeMap<CordisDynamicPluginId, CordisDynamicPackageId>,
) -> Option<CordisDynamicPackageId> {
    selected
        .get(plugin_id)
        .filter(|selected| listed.is_some_and(|listed| package_of(listed, selected).is_some()))
        .cloned()
        .or_else(|| listed.and_then(|listed| listed.next_package_id.clone()))
        .or_else(|| listed.and_then(|listed| listed.current_package_id.clone()))
        .or_else(|| {
            listed
                .and_then(|listed| listed.packages.last())
                .map(|package| package.package_id.clone())
        })
        .or_else(|| activity.map(activity_package_id))
}

fn panel_status(
    listed: Option<&DynamicCordisInventoryRow>,
    latest: Option<&seekdeep_cordis_dynamic_types::DynamicCordisRunAttempt>,
    selected_package_id: Option<&CordisDynamicPackageId>,
    awaiting: Option<&ApprovalRequestId>,
    loaded: &[DynamicCordisLivePackage],
) -> CordisPanelStatus {
    if awaiting.is_some() {
        return CordisPanelStatus::AwaitingApproval;
    }
    if latest.is_some_and(|latest| {
        latest.status == seekdeep_cordis_dynamic_types::CordisRunStatus::Failed
            && selected_package_id == Some(&latest.package_id)
    }) {
        return CordisPanelStatus::Failed;
    }
    listed.map_or(CordisPanelStatus::Idle, |listed| {
        listed.active_run.as_ref().map_or(
            CordisPanelStatus::Idle,
            |run| match cordis_visible_status(listed, &run.package_id, loaded) {
                CordisVisibleStatus::Idle => CordisPanelStatus::Idle,
                CordisVisibleStatus::ClientPending => CordisPanelStatus::ClientPending,
                CordisVisibleStatus::Running => CordisPanelStatus::Running,
            },
        )
    })
}

fn activity_agent_id(activity: &CordisRunActivity) -> SessionId {
    match activity {
        CordisRunActivity::AwaitingApproval { agent_id, .. }
        | CordisRunActivity::Orchestrating { agent_id, .. } => agent_id.clone(),
    }
}

fn activity_package_id(activity: &CordisRunActivity) -> CordisDynamicPackageId {
    match activity {
        CordisRunActivity::AwaitingApproval { package_id, .. }
        | CordisRunActivity::Orchestrating { package_id, .. } => package_id.clone(),
    }
}
