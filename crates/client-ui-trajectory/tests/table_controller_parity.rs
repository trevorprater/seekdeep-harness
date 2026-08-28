//! Deterministic trajectory-table lifecycle parity.

use seekdeep_client_ui_trajectory::{
    SelectedTrajectoryRequest, TrajectoryCell, TrajectoryCellKind, TrajectoryDetailTab,
    TrajectoryGroupModel, TrajectoryTableController, TrajectoryTableScrollAction,
    TrajectoryTableScrollMetrics, TrajectoryTurnModel, flatten_trajectory_table_records,
};

fn cell(index: usize, kind: TrajectoryCellKind, text: &str, source_seq: u64) -> TrajectoryCell {
    TrajectoryCell {
        source_seq: Some(source_seq),
        ..TrajectoryCell::new(index, kind, text)
    }
}

fn records(
    cells: Vec<TrajectoryCell>,
) -> Vec<seekdeep_client_ui_trajectory::TrajectoryTableRecord> {
    flatten_trajectory_table_records(&[TrajectoryTurnModel {
        turn: Some(1),
        groups: vec![TrajectoryGroupModel {
            title: "Step 1".to_owned(),
            description: None,
            cells,
        }],
    }])
}

fn metrics(top: f64, height: f64, client: f64) -> TrajectoryTableScrollMetrics {
    TrajectoryTableScrollMetrics {
        scroll_top: top,
        scroll_height: height,
        client_height: client,
    }
}

#[test]
fn selected_record_identity_survives_prepends_and_tab_history_is_contextual() {
    let initial = records(vec![cell(
        1,
        TrajectoryCellKind::Message,
        "selected tail response",
        100,
    )]);
    let mut controller = TrajectoryTableController::new();
    controller.select_record(&initial, 1);
    assert_eq!(controller.selected_index(&initial), Some(1));

    let mut tool = cell(3, TrajectoryCellKind::Tool, "bash", 101);
    tool.output_detail = Some("ok".to_owned());
    let prepended = records(vec![
        cell(1, TrajectoryCellKind::User, "older prompt", 1),
        cell(
            2,
            TrajectoryCellKind::Message,
            "selected tail response",
            100,
        ),
        tool,
    ]);
    assert_eq!(controller.selected_index(&prepended), Some(2));

    controller.select_record(&prepended, 3);
    controller.activate_tab(TrajectoryDetailTab::Output);
    controller.select_record(&prepended, 2);
    assert_eq!(
        controller.snapshot().active_tab,
        TrajectoryDetailTab::Overview
    );
    controller.select_record(&prepended, 3);
    assert_eq!(
        controller.snapshot().active_tab,
        TrajectoryDetailTab::Output
    );

    controller.clear_selection();
    assert_eq!(controller.selected_index(&prepended), None);
    assert!(controller.snapshot().selected_request.is_none());
}

#[test]
fn request_selection_and_thinking_state_have_one_owner() {
    let mut controller = TrajectoryTableController::new();
    let request = SelectedTrajectoryRequest {
        turn: Some(2),
        group: "Step 1".to_owned(),
        seq: Some(100),
    };
    controller.select_request(request.clone(), TrajectoryDetailTab::Timing);
    assert_eq!(controller.snapshot().selected_request, Some(request));
    assert!(controller.snapshot().selected_record_id.is_none());
    assert_eq!(
        controller.snapshot().active_tab,
        TrajectoryDetailTab::Timing
    );
    assert!(!controller.snapshot().thinking_expanded);
    controller.toggle_thinking();
    assert!(controller.snapshot().thinking_expanded);
    assert!(!controller.snapshot().show_unix_timestamp);
    controller.toggle_timestamp();
    assert!(controller.snapshot().show_unix_timestamp);
}

#[test]
fn append_following_changes_only_from_real_scroll_position() {
    let mut controller = TrajectoryTableController::new();
    assert_eq!(
        controller.reconcile_scroll(true, Some(1), false, metrics(0.0, 200.0, 100.0)),
        TrajectoryTableScrollAction::None
    );
    assert!(!controller.snapshot().table_scroll_ready);
    assert_eq!(
        controller.reconcile_scroll(false, Some(1), false, metrics(0.0, 200.0, 100.0)),
        TrajectoryTableScrollAction::ScrollToEnd
    );
    assert!(controller.snapshot().table_scroll_ready);

    assert!(!controller.on_scroll(metrics(100.0, 200.0, 100.0)));
    assert!(controller.snapshot().follows_table_tail);
    assert_eq!(
        controller.reconcile_scroll(false, Some(1), false, metrics(100.0, 260.0, 100.0)),
        TrajectoryTableScrollAction::ScrollToEnd
    );

    assert!(controller.on_scroll(metrics(20.0, 260.0, 100.0)));
    assert!(!controller.snapshot().follows_table_tail);
    assert_eq!(
        controller.reconcile_scroll(false, Some(1), false, metrics(20.0, 320.0, 100.0)),
        TrajectoryTableScrollAction::None
    );
}

#[test]
fn older_history_promise_and_nonvirtual_anchor_restoration_are_exact() {
    let mut controller = TrajectoryTableController::new();
    let at_top = metrics(0.0, 200.0, 100.0);
    assert!(controller.begin_older_load(true, true, false, true, Some(1), at_top));
    assert!(controller.snapshot().loading_older);
    assert!(!controller.begin_older_load(true, true, false, false, Some(1), at_top));
    controller.settle_older_load(true);
    assert!(!controller.snapshot().loading_older);
    assert_eq!(
        controller.reconcile_scroll(false, Some(0), false, metrics(0.0, 260.0, 100.0)),
        TrajectoryTableScrollAction::SetScrollTop(60.0)
    );
    assert!(!controller.snapshot().follows_table_tail);

    assert!(!controller.begin_older_load(
        true,
        true,
        false,
        true,
        Some(0),
        metrics(49.0, 260.0, 100.0),
    ));
    assert!(!controller.begin_older_load(true, true, true, false, Some(0), at_top));
    assert!(controller.begin_older_load(true, true, false, false, Some(0), at_top));
    controller.settle_older_load(false);
    assert_eq!(
        controller.reconcile_scroll(false, Some(9), false, metrics(0.0, 300.0, 100.0)),
        TrajectoryTableScrollAction::ScrollToEnd
    );
}

#[test]
fn virtual_prepend_consumes_anchor_without_a_second_scroll() {
    let mut controller = TrajectoryTableController::new();
    assert!(controller.begin_older_load(
        true,
        true,
        false,
        false,
        Some(10),
        metrics(0.0, 1_000.0, 600.0),
    ));
    controller.settle_older_load(true);
    assert_eq!(
        controller.reconcile_scroll(false, Some(9), true, metrics(0.0, 1_300.0, 600.0),),
        TrajectoryTableScrollAction::None
    );
    assert!(!controller.snapshot().follows_table_tail);
}

#[test]
fn inspect_and_focus_wait_for_a_real_uncollapsed_row() {
    let mut target = cell(1, TrajectoryCellKind::Tool, "bash", 1);
    target.call_id = Some("call-1".to_owned());
    let records = records(vec![target]);
    let mut controller = TrajectoryTableController::new();
    assert!(!controller.inspect_call(&records, "call-missing"));
    assert!(controller.snapshot().selected_record_id.is_none());
    assert!(controller.inspect_call(&records, "call-1"));
    assert_eq!(controller.selected_index(&records), Some(1));
    assert_eq!(controller.take_pending_scroll_index(&records), Some(1));
    assert!(controller.snapshot().pending_scroll_record_id.is_none());

    controller.focus_record(&records, 1);
    let mut folded = records.clone();
    folded[0].collapsed_summary = Some("1 tool call".to_owned());
    assert_eq!(controller.take_pending_scroll_index(&folded), None);
    assert!(controller.snapshot().pending_scroll_record_id.is_some());
    assert_eq!(controller.take_pending_scroll_index(&records), Some(1));
}

#[test]
fn details_resize_is_clamped_coupled_cancelable_and_resettable() {
    let mut controller = TrajectoryTableController::new();
    controller.begin_details_resize(7, 500.0, 400.0, 1_000.0);
    controller.move_details_resize(7, 450.0);
    assert_eq!(controller.snapshot().details_width, Some(450.0));
    assert_eq!(controller.snapshot().tool_request_offset, Some(305.0));

    controller.end_details_resize(8);
    controller.move_details_resize(7, -1_000.0);
    assert_eq!(controller.snapshot().details_width, Some(720.0));
    controller.end_details_resize(7);
    controller.move_details_resize(7, 500.0);
    assert_eq!(controller.snapshot().details_width, Some(720.0));

    controller.keyboard_details_resize(-1, 400.0, 1_000.0);
    assert_eq!(controller.snapshot().details_width, Some(384.0));
    controller.cancel_details_resize();
    controller.reset_details_resize();
    assert_eq!(controller.snapshot().details_width, None);
    assert_eq!(controller.snapshot().tool_request_offset, None);
}
