//! Operation-sequence and recorded-time trajectory projections.

use std::collections::BTreeSet;

use crate::{TrajectoryCell, TrajectoryCellKind, format_duration_millis};

/// One titled group of projected cells.
#[derive(Clone, Debug, PartialEq)]
pub struct TrajectoryGroupModel {
    /// Display title.
    pub title: String,
    /// Records in projection order.
    pub cells: Vec<TrajectoryCell>,
}

/// One turn or between-turn section.
#[derive(Clone, Debug, PartialEq)]
pub struct TrajectoryTurnModel {
    /// Model turn number, absent between turns.
    pub turn: Option<u64>,
    /// Ordered groups.
    pub groups: Vec<TrajectoryGroupModel>,
}

/// Horizontal timeline projection mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrajectoryTimelineMode {
    /// Equal-width operation slots.
    #[default]
    Sequence,
    /// Recorded durations with idle gaps compressed.
    Duration,
    /// Start-time points retaining wall-clock gaps.
    Time,
    /// Recorded durations retaining wall-clock gaps.
    Actual,
}

/// Inclusive selection in the active projection domain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrajectoryTimeRange {
    /// Lower domain coordinate.
    pub start: f64,
    /// Upper domain coordinate.
    pub end: f64,
}

/// One ledger record projected into the active timeline domain.
#[derive(Clone, Debug, PartialEq)]
pub struct TrajectoryTimelineSpan {
    /// Lower coordinate.
    pub start: f64,
    /// Upper coordinate.
    pub end: f64,
    /// Record display index.
    pub index: usize,
    /// Failure marker.
    pub is_error: bool,
    /// Semantic kind.
    pub kind: TrajectoryCellKind,
    /// Display label.
    pub label: String,
    /// Stable three-lane assignment.
    pub lane: u8,
}

/// One model-turn boundary in the active timeline domain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrajectoryTimelineTurnBoundary {
    /// Model turn number.
    pub turn: u64,
    /// First projected coordinate of the turn.
    pub time: f64,
}

/// Full-domain timeline model.
#[derive(Clone, Debug, PartialEq)]
pub struct TrajectoryTimelineModel {
    /// Domain lower bound.
    pub start: f64,
    /// Domain upper bound.
    pub end: f64,
    /// Visible record spans.
    pub spans: Vec<TrajectoryTimelineSpan>,
    /// Visible model-turn boundaries.
    pub turn_boundaries: Vec<TrajectoryTimelineTurnBoundary>,
}

/// Formats a timeline duration as an integer-millisecond label.
#[must_use]
pub fn format_timeline_offset(milliseconds: f64) -> String {
    format_duration_millis(Some(milliseconds))
}

/// Projects every visible record into the selected stable three-lane domain.
#[must_use]
pub fn derive_trajectory_timeline(
    turns: &[TrajectoryTurnModel],
    mode: TrajectoryTimelineMode,
) -> Option<TrajectoryTimelineModel> {
    if !matches!(mode, TrajectoryTimelineMode::Sequence) {
        return derive_timed_timeline(
            turns,
            matches!(
                mode,
                TrajectoryTimelineMode::Duration | TrajectoryTimelineMode::Actual
            ),
            matches!(mode, TrajectoryTimelineMode::Duration),
        );
    }
    let mut spans = Vec::new();
    let mut turn_boundaries = Vec::new();
    for turn in turns {
        let cells = turn
            .groups
            .iter()
            .flat_map(|group| &group.cells)
            .filter(|cell| cell.request_only != Some(true))
            .collect::<Vec<_>>();
        if cells.is_empty() {
            continue;
        }
        if let Some(turn) = turn.turn {
            turn_boundaries.push(TrajectoryTimelineTurnBoundary {
                turn,
                time: usize_as_f64(spans.len()),
            });
        }
        for cell in cells {
            let start = usize_as_f64(spans.len());
            spans.push(span(cell, start, start + 1.0));
        }
    }
    if spans.is_empty() {
        return None;
    }
    Some(TrajectoryTimelineModel {
        start: 0.0,
        end: usize_as_f64(spans.len()),
        spans,
        turn_boundaries,
    })
}

/// Identifies records intersecting an inclusive selected interval.
#[must_use]
pub fn trajectory_timeline_focus_indexes(
    turns: &[TrajectoryTurnModel],
    range: TrajectoryTimeRange,
    mode: TrajectoryTimelineMode,
) -> BTreeSet<usize> {
    derive_trajectory_timeline(turns, mode).map_or_else(BTreeSet::new, |model| {
        model
            .spans
            .into_iter()
            .filter(|span| span.start <= range.end && span.end >= range.start)
            .map(|span| span.index)
            .collect()
    })
}

fn derive_timed_timeline(
    turns: &[TrajectoryTurnModel],
    actual_duration: bool,
    compress_idle: bool,
) -> Option<TrajectoryTimelineModel> {
    let mut raw_spans = Vec::<TrajectoryTimelineSpan>::new();
    let mut timed_turns = Vec::<(Option<u64>, Vec<usize>)>::new();
    for turn in turns {
        let mut indexes = Vec::new();
        for cell in turn.groups.iter().flat_map(|group| &group.cells) {
            if cell.request_only == Some(true) {
                continue;
            }
            let Some(range) = cell_range(cell) else {
                continue;
            };
            indexes.push(raw_spans.len());
            raw_spans.push(span(cell, range.start, range.end));
        }
        if !indexes.is_empty() {
            timed_turns.push((turn.turn, indexes));
        }
    }
    if raw_spans.is_empty() {
        return None;
    }

    let mut order = (0..raw_spans.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        raw_spans[*left]
            .start
            .total_cmp(&raw_spans[*right].start)
            .then_with(|| raw_spans[*left].end.total_cmp(&raw_spans[*right].end))
    });
    let mut removed_idle_by_span = vec![0.0; raw_spans.len()];
    let mut removed_idle = 0.0;
    let mut covered_until = None::<f64>;
    for index in order {
        let current = &raw_spans[index];
        if compress_idle
            && let Some(covered) = covered_until
            && current.start > covered
        {
            removed_idle += current.start - covered;
        }
        removed_idle_by_span[index] = removed_idle;
        covered_until = Some(covered_until.map_or(current.end, |covered| covered.max(current.end)));
    }

    let mut spans = Vec::new();
    let mut turn_boundaries = Vec::new();
    for (turn, indexes) in timed_turns {
        let mut turn_start = f64::INFINITY;
        for index in indexes {
            let raw = &raw_spans[index];
            let offset = removed_idle_by_span[index];
            let projected = TrajectoryTimelineSpan {
                start: raw.start - offset,
                end: if actual_duration { raw.end } else { raw.start } - offset,
                ..raw.clone()
            };
            turn_start = turn_start.min(projected.start);
            spans.push(projected);
        }
        if let Some(turn) = turn {
            turn_boundaries.push(TrajectoryTimelineTurnBoundary {
                turn,
                time: turn_start,
            });
        }
    }
    let start = spans
        .iter()
        .map(|span| span.start)
        .fold(f64::INFINITY, f64::min);
    let end = spans
        .iter()
        .map(|span| span.end)
        .fold(f64::NEG_INFINITY, f64::max);
    Some(TrajectoryTimelineModel {
        start,
        end,
        spans,
        turn_boundaries,
    })
}

fn cell_range(cell: &TrajectoryCell) -> Option<TrajectoryTimeRange> {
    let start = cell.started_at.filter(|value| value.is_finite())?;
    let duration = cell
        .time_seconds
        .filter(|value| value.is_finite())
        .map_or(0.0, |value| value.max(0.0) * 1_000.0);
    Some(TrajectoryTimeRange {
        start,
        end: start + duration,
    })
}

fn lane_for(kind: TrajectoryCellKind) -> u8 {
    match kind {
        TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool => 2,
        TrajectoryCellKind::Message | TrajectoryCellKind::Compacted => 1,
        TrajectoryCellKind::System | TrajectoryCellKind::User | TrajectoryCellKind::Context => 0,
    }
}

fn span(cell: &TrajectoryCell, start: f64, end: f64) -> TrajectoryTimelineSpan {
    TrajectoryTimelineSpan {
        start,
        end,
        index: cell.index,
        is_error: cell.is_error == Some(true),
        kind: cell.kind,
        label: cell.text.clone(),
        lane: lane_for(cell.kind),
    }
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
