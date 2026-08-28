//! Deterministic Timeline zoom, pan, drag, focus, and selection controller.

use crate::{
    TrajectoryTimeRange, TrajectoryTimelineMode, TrajectoryTimelineModel, TrajectoryTimelineSpan,
};
use serde::Serialize;

const MINIMUM_DRAG_PX: f64 = 3.0;
const MINIMUM_ZOOM_OPERATIONS: f64 = 4.0;
const EDGE_PAN_ZONE_FRACTION: f64 = 0.08;
const EDGE_PAN_STEP_FRACTION: f64 = 0.025;
const MAXIMUM_EDGE_PAN_PX: f64 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct DragGesture {
    pointer_id: i32,
    anchor_time: f64,
    anchor_client_x: f64,
    record_index: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PanGesture {
    pointer_id: i32,
    anchor_client_x: f64,
    anchor_start: f64,
    moved: bool,
    pannable: bool,
}

/// Timeline hover projection.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineHoverPoint {
    /// Track-relative fraction.
    pub fraction: f64,
    /// Hovered record, absent on whitespace.
    pub record_index: Option<usize>,
}

/// One pointer completion's outward callbacks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelinePointerOutcome {
    /// `Some(None)` clears the range; `Some(Some(_))` commits one.
    pub range_change: Option<Option<TrajectoryTimeRange>>,
    /// Directly clicked span.
    pub record_select: Option<usize>,
    /// Nearest whitespace-click record.
    pub record_focus: Option<usize>,
}

/// Immutable render-facing controller snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineControllerSnapshot {
    /// Current viewport, absent at full zoom.
    pub viewport: Option<TrajectoryTimeRange>,
    /// Current selection drag.
    pub draft: Option<TrajectoryTimeRange>,
    /// Current hover.
    pub hover: Option<TimelineHoverPoint>,
    /// Right-button gesture state.
    pub panning: bool,
    /// Whether viewport CSS should animate.
    pub animate_viewport: bool,
    /// Active domain.
    pub domain: TrajectoryTimeRange,
    /// Full-domain duration, clamped to at least one.
    pub full_duration: f64,
}

/// Stateful deterministic interaction layer over one timeline model.
#[derive(Clone, Debug)]
pub struct TimelineViewportController {
    model: TrajectoryTimelineModel,
    mode: TrajectoryTimelineMode,
    viewport: Option<TrajectoryTimeRange>,
    draft: Option<TrajectoryTimeRange>,
    hover: Option<TimelineHoverPoint>,
    drag: Option<DragGesture>,
    pan: Option<PanGesture>,
    panning: bool,
    animate_viewport: bool,
}

#[allow(clippy::float_cmp)] // Source commits only exact clamped projection coordinates.
impl TimelineViewportController {
    /// Creates the full-domain viewport.
    #[must_use]
    pub fn new(model: TrajectoryTimelineModel, mode: TrajectoryTimelineMode) -> Self {
        Self {
            model,
            mode,
            viewport: None,
            draft: None,
            hover: None,
            drag: None,
            pan: None,
            panning: false,
            animate_viewport: false,
        }
    }

    /// Current zoomed viewport, absent at full domain.
    #[must_use]
    pub const fn viewport(&self) -> Option<TrajectoryTimeRange> {
        self.viewport
    }

    /// Current in-progress selection.
    #[must_use]
    pub const fn draft(&self) -> Option<TrajectoryTimeRange> {
        self.draft
    }

    /// Current hover point.
    #[must_use]
    pub const fn hover(&self) -> Option<TimelineHoverPoint> {
        self.hover
    }

    /// Whether a right-button pan gesture is active.
    #[must_use]
    pub const fn panning(&self) -> bool {
        self.panning
    }

    /// Whether the next viewport projection should animate.
    #[must_use]
    pub const fn animate_viewport(&self) -> bool {
        self.animate_viewport
    }

    /// Returns the immutable render state.
    #[must_use]
    pub fn snapshot(&self) -> TimelineControllerSnapshot {
        TimelineControllerSnapshot {
            viewport: self.viewport,
            draft: self.draft,
            hover: self.hover,
            panning: self.panning,
            animate_viewport: self.animate_viewport,
            domain: self.domain(),
            full_duration: self.full_duration(),
        }
    }

    /// Active domain start and duration after clamping the viewport.
    #[must_use]
    pub fn domain(&self) -> TrajectoryTimeRange {
        let full_duration = self.full_duration();
        let viewport_duration = self.viewport.map_or(full_duration, |viewport| {
            (viewport.end - viewport.start).max(1.0).min(full_duration)
        });
        let start = self.viewport.map_or(self.model.start, |viewport| {
            viewport
                .start
                .max(self.model.start)
                .min(self.model.end - viewport_duration)
        });
        TrajectoryTimeRange {
            start,
            end: start + viewport_duration,
        }
    }

    /// Replaces the model, dropping only a viewport wholly outside the new domain.
    pub fn set_model(&mut self, model: TrajectoryTimelineModel) {
        self.model = model;
        self.animate_viewport = false;
        if self.viewport.is_some_and(|viewport| {
            viewport.end < self.model.start || viewport.start > self.model.end
        }) {
            self.viewport = None;
        }
    }

    /// Returns whether a committed range lies wholly outside the model and must clear.
    #[must_use]
    pub fn range_is_outside(&self, range: TrajectoryTimeRange) -> bool {
        range.end < self.model.start || range.start > self.model.end
    }

    /// Zooms around one clamped track fraction. Native scrolling is always suppressed by the caller.
    pub fn wheel(&mut self, anchor_fraction: f64, delta_y: f64) {
        self.animate_viewport = false;
        let anchor_fraction = clamp_fraction(anchor_fraction);
        let full_duration = self.full_duration();
        let domain = self.domain();
        let domain_duration = domain.end - domain.start;
        let minimum = if matches!(self.mode, TrajectoryTimelineMode::Sequence) {
            MINIMUM_ZOOM_OPERATIONS
        } else {
            20.0
        }
        .min(full_duration);
        let next_duration = (domain_duration * (delta_y * 0.0015).exp())
            .max(minimum)
            .min(full_duration);
        if next_duration >= full_duration * 0.999 {
            self.viewport = None;
            return;
        }
        let anchor_time = domain.start + anchor_fraction * domain_duration;
        let next_start = (anchor_time - anchor_fraction * next_duration)
            .max(self.model.start)
            .min(self.model.end - next_duration);
        self.viewport = Some(TrajectoryTimeRange {
            start: next_start,
            end: next_start + next_duration,
        });
    }

    /// Begins a left-button selection or right-button pan.
    pub fn pointer_down(
        &mut self,
        button: i16,
        pointer_id: i32,
        client_x: f64,
        track_left: f64,
        track_width: f64,
        record_index: Option<usize>,
    ) {
        if button == 2 {
            self.pan = Some(PanGesture {
                pointer_id,
                anchor_client_x: client_x,
                anchor_start: self.domain().start,
                moved: false,
                pannable: self.viewport.is_some(),
            });
            if self.viewport.is_some() {
                self.animate_viewport = false;
            }
            self.panning = true;
            return;
        }
        if button != 0 {
            return;
        }
        let fraction = point_fraction(client_x, track_left, track_width);
        let domain = self.domain();
        let anchor_time = domain.start + fraction * (domain.end - domain.start);
        self.hover = Some(TimelineHoverPoint {
            fraction,
            record_index,
        });
        self.drag = Some(DragGesture {
            pointer_id,
            anchor_time,
            anchor_client_x: client_x,
            record_index,
        });
        self.draft = Some(TrajectoryTimeRange {
            start: anchor_time,
            end: anchor_time,
        });
    }

    /// Advances the active pan or range drag, including repeated edge auto-pan.
    pub fn pointer_move(
        &mut self,
        pointer_id: i32,
        client_x: f64,
        track_left: f64,
        track_width: f64,
        record_index: Option<usize>,
    ) {
        let fraction = point_fraction(client_x, track_left, track_width);
        self.hover = Some(TimelineHoverPoint {
            fraction,
            record_index,
        });
        let current_domain = self.domain();
        if let Some(pan) = self.pan.as_mut().filter(|pan| pan.pointer_id == pointer_id) {
            if (client_x - pan.anchor_client_x).abs() >= MINIMUM_DRAG_PX {
                pan.moved = true;
            }
            if !pan.pannable {
                return;
            }
            let duration = current_domain.end - current_domain.start;
            let delta = (client_x - pan.anchor_client_x) / track_width.max(1.0);
            let next_start = (pan.anchor_start - delta * duration)
                .max(self.model.start)
                .min(self.model.end - duration);
            self.viewport = Some(TrajectoryTimeRange {
                start: next_start,
                end: next_start + duration,
            });
            return;
        }
        let Some(drag) = self.drag.filter(|drag| drag.pointer_id == pointer_id) else {
            return;
        };
        let domain = current_domain;
        let duration = domain.end - domain.start;
        let mut next_start = domain.start;
        if self.viewport.is_some() {
            let local_x = client_x - track_left;
            let edge_width =
                MAXIMUM_EDGE_PAN_PX.min((track_width * EDGE_PAN_ZONE_FRACTION).max(1.0));
            let direction = if local_x < edge_width {
                -1.0
            } else if local_x > track_width - edge_width {
                1.0
            } else {
                0.0
            };
            if direction != 0.0 {
                let distance = if direction < 0.0 {
                    edge_width - local_x
                } else {
                    local_x - (track_width - edge_width)
                };
                let strength = clamp_fraction(distance / edge_width).max(0.2);
                let desired =
                    domain.start + direction * duration * EDGE_PAN_STEP_FRACTION * strength;
                next_start = desired.max(self.model.start).min(self.model.end - duration);
                if next_start != domain.start {
                    self.animate_viewport = false;
                    self.viewport = Some(TrajectoryTimeRange {
                        start: next_start,
                        end: next_start + duration,
                    });
                }
            }
        }
        let point_time = next_start + fraction * duration;
        self.draft = Some(ordered_range(drag.anchor_time, point_time));
    }

    /// Completes an active gesture and returns the callbacks the React boundary emits.
    #[must_use]
    pub fn pointer_up(
        &mut self,
        pointer_id: i32,
        client_x: f64,
        track_left: f64,
        track_width: f64,
        record_index: Option<usize>,
    ) -> TimelinePointerOutcome {
        if let Some(pan) = self.pan.filter(|pan| pan.pointer_id == pointer_id) {
            let moved = pan.moved || (client_x - pan.anchor_client_x).abs() >= MINIMUM_DRAG_PX;
            self.pan = None;
            self.panning = false;
            return TimelinePointerOutcome {
                range_change: (!moved).then_some(None),
                ..TimelinePointerOutcome::default()
            };
        }
        let Some(drag) = self.drag.filter(|drag| drag.pointer_id == pointer_id) else {
            return TimelinePointerOutcome::default();
        };
        let domain = self.domain();
        let duration = domain.end - domain.start;
        let fraction = point_fraction(client_x, track_left, track_width);
        let point_time = domain.start + fraction * duration;
        let selected = ordered_range(drag.anchor_time, point_time);
        self.hover = Some(TimelineHoverPoint {
            fraction,
            record_index,
        });
        self.drag = None;
        self.draft = None;
        let click = (client_x - drag.anchor_client_x).abs() < MINIMUM_DRAG_PX;
        if click
            && let Some(index) = drag.record_index
            && self.model.spans.iter().any(|span| span.index == index)
        {
            return TimelinePointerOutcome {
                range_change: Some(None),
                record_select: Some(index),
                record_focus: None,
            };
        }
        let minimum = duration.min(self.full_duration() / usize_as_f64(self.model.spans.len()));
        let committed = if selected.end - selected.start < minimum {
            centered_range(
                if click {
                    selected.start
                } else {
                    f64::midpoint(selected.start, selected.end)
                },
                minimum,
                self.model.start,
                self.model.end,
            )
        } else {
            selected
        };
        TimelinePointerOutcome {
            range_change: Some(Some(committed)),
            record_select: None,
            record_focus: click.then(|| nearest_span(&self.model.spans, selected.start).index),
        }
    }

    /// Cancels every pointer-local state and hover.
    pub fn pointer_cancel(&mut self) {
        self.drag = None;
        self.pan = None;
        self.draft = None;
        self.hover = None;
        self.panning = false;
    }

    /// Clears hover only when no gesture owns it.
    pub fn pointer_leave(&mut self) {
        if self.drag.is_none() && self.pan.is_none() {
            self.hover = None;
        }
    }

    /// Pans the current zoom just far enough to reveal a newly selected span.
    pub fn reveal_selected(&mut self, selected_index: usize) {
        let Some(selected) = self
            .model
            .spans
            .iter()
            .find(|span| span.index == selected_index)
        else {
            return;
        };
        self.animate_viewport = true;
        let Some(viewport) = self.viewport else {
            return;
        };
        if selected.end > viewport.start && selected.start < viewport.end {
            return;
        }
        let duration = (viewport.end - viewport.start).max(1.0);
        let desired = if selected.end <= viewport.start {
            selected.start
        } else {
            selected.end - duration
        };
        let start = desired
            .max(self.model.start)
            .min((self.model.end - duration).max(self.model.start));
        if start != viewport.start {
            self.viewport = Some(TrajectoryTimeRange {
                start,
                end: start + duration,
            });
        }
    }

    fn full_duration(&self) -> f64 {
        (self.model.end - self.model.start).max(1.0)
    }
}

fn point_fraction(client_x: f64, left: f64, width: f64) -> f64 {
    clamp_fraction((client_x - left) / width.max(1.0))
}

fn clamp_fraction(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

fn ordered_range(left: f64, right: f64) -> TrajectoryTimeRange {
    if left <= right {
        TrajectoryTimeRange {
            start: left,
            end: right,
        }
    } else {
        TrajectoryTimeRange {
            start: right,
            end: left,
        }
    }
}

fn centered_range(center: f64, width: f64, minimum: f64, maximum: f64) -> TrajectoryTimeRange {
    let width = width.max(0.0).min(maximum - minimum);
    let start = (center - width / 2.0).max(minimum).min(maximum - width);
    TrajectoryTimeRange {
        start,
        end: start + width,
    }
}

fn nearest_span(spans: &[TrajectoryTimelineSpan], point: f64) -> &TrajectoryTimelineSpan {
    spans
        .iter()
        .reduce(|candidate, span| {
            if span_distance(span, point) < span_distance(candidate, point) {
                span
            } else {
                candidate
            }
        })
        .expect("timeline model contains at least one span")
}

fn span_distance(span: &TrajectoryTimelineSpan, point: f64) -> f64 {
    if point < span.start {
        span.start - point
    } else if point > span.end {
        point - span.end
    } else {
        0.0
    }
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
