//! Timeline zoom, pan, drag, focus, and selected-record reveal parity.

#![allow(clippy::cast_precision_loss, clippy::float_cmp)] // Exact small fixture coordinates.

use seekdeep_client_ui_trajectory::{
    TimelineViewportController, TrajectoryCellKind, TrajectoryTimeRange, TrajectoryTimelineMode,
    TrajectoryTimelineModel, TrajectoryTimelineSpan,
};

fn model(count: usize) -> TrajectoryTimelineModel {
    TrajectoryTimelineModel {
        start: 0.0,
        end: count as f64,
        spans: (0..count)
            .map(|index| TrajectoryTimelineSpan {
                start: index as f64,
                end: index as f64 + 1.0,
                index,
                is_error: false,
                kind: TrajectoryCellKind::Message,
                label: format!("record {index}"),
                lane: 1,
            })
            .collect(),
        turn_boundaries: Vec::new(),
    }
}

#[test]
fn wheel_zoom_is_anchor_stable_clamped_and_restores_full_domain() {
    let mut controller =
        TimelineViewportController::new(model(10), TrajectoryTimelineMode::Sequence);
    controller.wheel(0.5, -1_000.0);
    assert_eq!(
        controller.viewport(),
        Some(TrajectoryTimeRange {
            start: 3.0,
            end: 7.0
        })
    );
    controller.wheel(0.5, 10_000.0);
    assert_eq!(controller.viewport(), None);

    let mut duration =
        TimelineViewportController::new(model(100), TrajectoryTimelineMode::Duration);
    duration.wheel(0.25, -10_000.0);
    assert_eq!(
        duration.viewport(),
        Some(TrajectoryTimeRange {
            start: 20.0,
            end: 40.0
        })
    );
}

#[test]
fn right_click_clears_without_changing_zoom_and_drag_pans_without_selection() {
    let mut controller =
        TimelineViewportController::new(model(10), TrajectoryTimelineMode::Sequence);
    controller.wheel(0.5, -1_000.0);
    let before = controller.viewport();
    controller.pointer_down(2, 1, 50.0, 0.0, 100.0, None);
    assert!(controller.panning());
    let cleared = controller.pointer_up(1, 50.0, 0.0, 100.0, None);
    assert_eq!(cleared.range_change, Some(None));
    assert_eq!(controller.viewport(), before);

    controller.pointer_down(2, 2, 50.0, 0.0, 100.0, None);
    controller.pointer_move(2, 75.0, 0.0, 100.0, None);
    let panned = controller.viewport();
    let outcome = controller.pointer_up(2, 75.0, 0.0, 100.0, None);
    assert_ne!(panned, before);
    assert_eq!(outcome.range_change, None);
    assert!(!controller.panning());
}

#[test]
fn span_click_selects_whitespace_click_focuses_and_short_drag_gets_minimum_width() {
    let mut controller =
        TimelineViewportController::new(model(10), TrajectoryTimelineMode::Sequence);
    controller.pointer_down(0, 1, 25.0, 0.0, 100.0, Some(2));
    let selected = controller.pointer_up(1, 25.0, 0.0, 100.0, Some(2));
    assert_eq!(selected.range_change, Some(None));
    assert_eq!(selected.record_select, Some(2));

    controller.pointer_down(0, 2, 64.0, 0.0, 100.0, None);
    let focused = controller.pointer_up(2, 64.0, 0.0, 100.0, None);
    assert_eq!(focused.record_focus, Some(6));
    let range = focused.range_change.unwrap().unwrap();
    assert_eq!(range.end - range.start, 1.0);

    controller.pointer_down(0, 3, 20.0, 0.0, 100.0, None);
    controller.pointer_move(3, 80.0, 0.0, 100.0, None);
    let dragged = controller.pointer_up(3, 80.0, 0.0, 100.0, None);
    assert_eq!(
        dragged.range_change,
        Some(Some(TrajectoryTimeRange {
            start: 2.0,
            end: 8.0
        }))
    );
}

#[test]
fn edge_drag_auto_pans_repeatedly_and_cancel_clears_pointer_local_state() {
    let mut controller =
        TimelineViewportController::new(model(10), TrajectoryTimelineMode::Sequence);
    controller.wheel(0.5, -1_000.0);
    controller.pointer_down(0, 1, 50.0, 0.0, 100.0, None);
    for _ in 0..24 {
        controller.pointer_move(1, 99.0, 0.0, 100.0, None);
    }
    let draft = controller.draft().unwrap();
    assert!(draft.end - draft.start > 4.0);
    assert!(draft.start >= 0.0 && draft.end <= 10.0);
    controller.pointer_cancel();
    assert_eq!(controller.draft(), None);
    assert_eq!(controller.hover(), None);
}

#[test]
fn selected_record_reveal_moves_only_far_enough_and_model_reconciliation_is_fail_safe() {
    let mut controller =
        TimelineViewportController::new(model(10), TrajectoryTimelineMode::Sequence);
    controller.wheel(0.5, -1_000.0);
    controller.reveal_selected(1);
    assert_eq!(
        controller.viewport(),
        Some(TrajectoryTimeRange {
            start: 1.0,
            end: 5.0
        })
    );
    assert!(controller.animate_viewport());
    controller.reveal_selected(8);
    assert_eq!(
        controller.viewport(),
        Some(TrajectoryTimeRange {
            start: 5.0,
            end: 9.0
        })
    );
    assert!(controller.range_is_outside(TrajectoryTimeRange {
        start: 20.0,
        end: 30.0
    }));
    controller.set_model(model(3));
    assert_eq!(controller.viewport(), None);
}
