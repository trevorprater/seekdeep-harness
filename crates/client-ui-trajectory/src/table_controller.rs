//! Deterministic selection, history, scroll, and resize state for the trajectory ledger.

use serde::{Deserialize, Serialize};

use crate::{
    TrajectoryDetailTab, TrajectoryTableRecord, trajectory_detail_tabs, trajectory_record_id,
};

const BOTTOM_FOLLOW_THRESHOLD_PX: f64 = 2.0;
const OLDER_LOAD_THRESHOLD_PX: f64 = 48.0;
const DETAILS_MIN_WIDTH: f64 = 320.0;
const DETAILS_MAX_WIDTH: f64 = 720.0;
const TABLE_MIN_WIDTH: f64 = 280.0;
const DETAILS_RESIZE_STEP: f64 = 16.0;
const TOOL_REQUEST_SHARE: f64 = 0.58;
const TOOL_REQUEST_MIN_WIDTH: f64 = 180.0;
const TOOL_REQUEST_MAX_WIDTH: f64 = 480.0;
const DEFAULT_TOOL_REQUEST_SHARE: f64 = 0.36;
const DEFAULT_TOOL_REQUEST_OFFSET: f64 = 56.0;

/// Request selected in the local table inspector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedTrajectoryRequest {
    /// Model turn, absent for between-turn compaction.
    pub turn: Option<u64>,
    /// Source group title.
    pub group: String,
    /// Request anchor sequence, when finalized.
    pub seq: Option<u64>,
}

/// Scroll-box measurements used by deterministic table lifecycle decisions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrajectoryTableScrollMetrics {
    /// Current vertical scroll offset.
    pub scroll_top: f64,
    /// Complete scrollable height.
    pub scroll_height: f64,
    /// Visible client height.
    pub client_height: f64,
}

/// DOM scroll effect requested by the controller.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TrajectoryTableScrollAction {
    /// No scroll mutation.
    #[default]
    None,
    /// Set an exact non-virtual scroll offset.
    SetScrollTop(f64),
    /// Move to the virtual or non-virtual tail.
    ScrollToEnd,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OlderLoadAnchor {
    history_start_seq: Option<u64>,
    scroll_height: f64,
    scroll_top: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DetailsResizeDrag {
    pointer_id: i32,
    start_x: f64,
    start_width: f64,
    split_width: f64,
    start_tool_request_offset: f64,
}

/// Render-facing snapshot of local table controller state.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // Independent source hooks are observable render inputs.
pub struct TrajectoryTableControllerSnapshot {
    /// Projection-stable selected record identity.
    pub selected_record_id: Option<String>,
    /// Selected request identity.
    pub selected_request: Option<SelectedTrajectoryRequest>,
    /// Active inspector tab.
    pub active_tab: TrajectoryDetailTab,
    /// Whether long assistant thinking is expanded.
    pub thinking_expanded: bool,
    /// User-resized details width.
    pub details_width: Option<f64>,
    /// Coupled Tool/request split offset.
    pub tool_request_offset: Option<f64>,
    /// Whether subsequent appends should follow the tail.
    pub follows_table_tail: bool,
    /// Whether initial history scroll positioning completed.
    pub table_scroll_ready: bool,
    /// Whether this ledger owns an older-page request.
    pub loading_older: bool,
    /// Record awaiting a post-render scroll.
    pub pending_scroll_record_id: Option<String>,
}

/// Single-owner local state machine for the trajectory table.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // Independent lifecycle flags retain source transitions.
pub struct TrajectoryTableController {
    selected_record_id: Option<String>,
    selected_request: Option<SelectedTrajectoryRequest>,
    active_tab: TrajectoryDetailTab,
    tab_history: Vec<TrajectoryDetailTab>,
    thinking_expanded: bool,
    details_width: Option<f64>,
    tool_request_offset: Option<f64>,
    details_resize_drag: Option<DetailsResizeDrag>,
    follows_table_tail: bool,
    table_scroll_initialized: bool,
    table_scroll_ready: bool,
    loading_older: bool,
    older_load_anchor: Option<OlderLoadAnchor>,
    pending_scroll_record_id: Option<String>,
}

impl Default for TrajectoryTableController {
    fn default() -> Self {
        Self::new()
    }
}

impl TrajectoryTableController {
    /// Creates the source initial state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            selected_record_id: None,
            selected_request: None,
            active_tab: TrajectoryDetailTab::Overview,
            tab_history: vec![TrajectoryDetailTab::Overview],
            thinking_expanded: false,
            details_width: None,
            tool_request_offset: None,
            details_resize_drag: None,
            follows_table_tail: false,
            table_scroll_initialized: false,
            table_scroll_ready: false,
            loading_older: false,
            older_load_anchor: None,
            pending_scroll_record_id: None,
        }
    }

    /// Returns immutable render-facing state.
    #[must_use]
    pub fn snapshot(&self) -> TrajectoryTableControllerSnapshot {
        TrajectoryTableControllerSnapshot {
            selected_record_id: self.selected_record_id.clone(),
            selected_request: self.selected_request.clone(),
            active_tab: self.active_tab,
            thinking_expanded: self.thinking_expanded,
            details_width: self.details_width,
            tool_request_offset: self.tool_request_offset,
            follows_table_tail: self.follows_table_tail,
            table_scroll_ready: self.table_scroll_ready,
            loading_older: self.loading_older,
            pending_scroll_record_id: self.pending_scroll_record_id.clone(),
        }
    }

    /// Resolves the selected record after projection indexes shift.
    #[must_use]
    pub fn selected_index(&self, records: &[TrajectoryTableRecord]) -> Option<usize> {
        let id = self.selected_record_id.as_deref()?;
        records
            .iter()
            .find(|record| trajectory_record_id(&record.cell) == id)
            .map(|record| record.cell.index)
    }

    /// Selects one record by its current display index and restores its most recent valid tab.
    pub fn select_record(&mut self, records: &[TrajectoryTableRecord], index: usize) {
        let record = records.iter().find(|record| record.cell.index == index);
        self.selected_request = None;
        self.selected_record_id = record.map(|record| trajectory_record_id(&record.cell));
        let Some(record) = record else {
            return;
        };
        let tabs = trajectory_detail_tabs(record);
        self.active_tab = self
            .tab_history
            .iter()
            .rev()
            .find(|candidate| tabs.iter().any(|tab| tab.id == **candidate))
            .copied()
            .or_else(|| tabs.first().map(|tab| tab.id))
            .unwrap_or(TrajectoryDetailTab::Overview);
    }

    /// Selects a request and opens Summary or Timing.
    pub fn select_request(&mut self, request: SelectedTrajectoryRequest, tab: TrajectoryDetailTab) {
        debug_assert!(matches!(
            tab,
            TrajectoryDetailTab::Overview | TrajectoryDetailTab::Timing
        ));
        self.selected_record_id = None;
        self.selected_request = Some(request);
        self.activate_tab(tab);
    }

    /// Clears record and request inspector selection.
    pub fn clear_selection(&mut self) {
        self.selected_record_id = None;
        self.selected_request = None;
    }

    /// Makes one inspector tab active and most recent.
    pub fn activate_tab(&mut self, tab: TrajectoryDetailTab) {
        self.tab_history.retain(|candidate| *candidate != tab);
        self.tab_history.push(tab);
        self.active_tab = tab;
    }

    /// Toggles long assistant thinking in the current inspector.
    pub fn toggle_thinking(&mut self) {
        self.thinking_expanded = !self.thinking_expanded;
    }

    /// Applies one cross-view call inspection when the record is currently resolvable.
    #[must_use]
    pub fn inspect_call(&mut self, records: &[TrajectoryTableRecord], call_id: &str) -> bool {
        let Some(record) = records
            .iter()
            .find(|record| record.cell.call_id.as_deref() == Some(call_id))
        else {
            return false;
        };
        let id = trajectory_record_id(&record.cell);
        self.selected_request = None;
        self.selected_record_id = Some(id.clone());
        self.activate_tab(TrajectoryDetailTab::Overview);
        self.pending_scroll_record_id = Some(id);
        true
    }

    /// Requests a post-render scroll without changing inspector selection.
    pub fn focus_record(&mut self, records: &[TrajectoryTableRecord], index: usize) {
        self.pending_scroll_record_id = records
            .iter()
            .find(|record| record.cell.index == index)
            .map(|record| trajectory_record_id(&record.cell));
    }

    /// Resolves and consumes a pending scroll only once its uncollapsed row exists.
    pub fn take_pending_scroll_index(
        &mut self,
        records: &[TrajectoryTableRecord],
    ) -> Option<usize> {
        let id = self.pending_scroll_record_id.as_deref()?;
        let record = records.iter().find(|record| {
            record.collapsed_summary.is_none() && trajectory_record_id(&record.cell) == id
        })?;
        self.pending_scroll_record_id = None;
        self.follows_table_tail = false;
        Some(record.cell.index)
    }

    /// Updates tail-follow state and returns whether top-proximity can request older history.
    #[must_use]
    pub fn on_scroll(&mut self, metrics: TrajectoryTableScrollMetrics) -> bool {
        self.follows_table_tail =
            metrics.scroll_height - metrics.client_height - metrics.scroll_top
                <= BOTTOM_FOLLOW_THRESHOLD_PX;
        metrics.scroll_top <= OLDER_LOAD_THRESHOLD_PX
    }

    /// Starts one older-page request and snapshots the non-virtual scroll anchor.
    #[must_use]
    #[allow(clippy::fn_params_excessive_bools)] // Each source guard rejects a distinct load cause.
    pub fn begin_older_load(
        &mut self,
        has_older_records: bool,
        can_load_older: bool,
        externally_loading: bool,
        require_top: bool,
        history_start_seq: Option<u64>,
        metrics: TrajectoryTableScrollMetrics,
    ) -> bool {
        if !has_older_records
            || !can_load_older
            || self.loading_older
            || externally_loading
            || (require_top && metrics.scroll_top > OLDER_LOAD_THRESHOLD_PX)
        {
            return false;
        }
        self.loading_older = true;
        self.older_load_anchor = Some(OlderLoadAnchor {
            history_start_seq,
            scroll_height: metrics.scroll_height,
            scroll_top: metrics.scroll_top,
        });
        true
    }

    /// Settles the owned older-page Promise, retaining only an advancing anchor.
    pub fn settle_older_load(&mut self, advanced: bool) {
        if !advanced {
            self.older_load_anchor = None;
        }
        self.loading_older = false;
    }

    /// Reconciles prepend anchoring, initial tail positioning, and append following.
    #[must_use]
    pub fn reconcile_scroll(
        &mut self,
        history_loading: bool,
        history_start_seq: Option<u64>,
        virtualization_enabled: bool,
        metrics: TrajectoryTableScrollMetrics,
    ) -> TrajectoryTableScrollAction {
        if let Some(anchor) = self.older_load_anchor
            && anchor.history_start_seq != history_start_seq
        {
            self.older_load_anchor = None;
            self.follows_table_tail = false;
            return if virtualization_enabled {
                TrajectoryTableScrollAction::None
            } else {
                TrajectoryTableScrollAction::SetScrollTop(
                    anchor.scroll_top + metrics.scroll_height - anchor.scroll_height,
                )
            };
        }
        if !self.table_scroll_initialized {
            if history_loading {
                return TrajectoryTableScrollAction::None;
            }
            self.table_scroll_initialized = true;
            self.follows_table_tail = true;
            self.table_scroll_ready = true;
            return TrajectoryTableScrollAction::ScrollToEnd;
        }
        if self.follows_table_tail {
            TrajectoryTableScrollAction::ScrollToEnd
        } else {
            TrajectoryTableScrollAction::None
        }
    }

    /// Starts a details-resize pointer gesture.
    pub fn begin_details_resize(
        &mut self,
        pointer_id: i32,
        client_x: f64,
        details_width: f64,
        split_width: f64,
    ) {
        self.details_resize_drag = Some(DetailsResizeDrag {
            pointer_id,
            start_x: client_x,
            start_width: details_width,
            split_width,
            start_tool_request_offset: self.tool_request_offset.unwrap_or_else(|| {
                split_width * TOOL_REQUEST_SHARE - default_tool_request_width(split_width)
            }),
        });
    }

    /// Applies a matching details-resize pointer move.
    pub fn move_details_resize(&mut self, pointer_id: i32, client_x: f64) {
        let Some(drag) = self
            .details_resize_drag
            .filter(|drag| drag.pointer_id == pointer_id)
        else {
            return;
        };
        let next =
            clamp_details_width(drag.start_width + drag.start_x - client_x, drag.split_width);
        self.details_width = Some(next);
        self.tool_request_offset =
            Some(drag.start_tool_request_offset + (next - drag.start_width) * TOOL_REQUEST_SHARE);
    }

    /// Ends a matching details-resize pointer gesture.
    pub fn end_details_resize(&mut self, pointer_id: i32) {
        if self
            .details_resize_drag
            .is_some_and(|drag| drag.pointer_id == pointer_id)
        {
            self.details_resize_drag = None;
        }
    }

    /// Cancels any details-resize pointer gesture.
    pub fn cancel_details_resize(&mut self) {
        self.details_resize_drag = None;
    }

    /// Resizes details one keyboard step; negative direction shrinks.
    pub fn keyboard_details_resize(&mut self, direction: i8, current_width: f64, split_width: f64) {
        let next = clamp_details_width(
            current_width + f64::from(direction) * DETAILS_RESIZE_STEP,
            split_width,
        );
        let current_offset = self.tool_request_offset.unwrap_or_else(|| {
            split_width * TOOL_REQUEST_SHARE - default_tool_request_width(split_width)
        });
        self.details_width = Some(next);
        self.tool_request_offset =
            Some(current_offset + (next - current_width) * TOOL_REQUEST_SHARE);
    }

    /// Restores responsive details sizing.
    pub fn reset_details_resize(&mut self) {
        self.details_width = None;
        self.tool_request_offset = None;
        self.details_resize_drag = None;
    }
}

fn clamp_details_width(width: f64, split_width: f64) -> f64 {
    let maximum = DETAILS_MIN_WIDTH.max(DETAILS_MAX_WIDTH.min(split_width - TABLE_MIN_WIDTH));
    js_round(width.max(DETAILS_MIN_WIDTH).min(maximum))
}

fn default_tool_request_width(split_width: f64) -> f64 {
    (split_width * DEFAULT_TOOL_REQUEST_SHARE - DEFAULT_TOOL_REQUEST_OFFSET)
        .clamp(TOOL_REQUEST_MIN_WIDTH, TOOL_REQUEST_MAX_WIDTH)
}

fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}
