//! Native flow rising-edge and HMR lifetime parity.

use seekdeep_client_ui_directory_picker_native::NativeDirectoryFlowState;

#[test]
fn one_pick_runs_per_open_edge_and_face_identity_changes_do_not_relaunch() {
    let mut state = NativeDirectoryFlowState::new();
    assert!(state.reconcile_open(true));
    assert!(!state.reconcile_open(true));
    assert!(!state.reconcile_open(true));
    assert!(!state.reconcile_open(false));
    assert!(state.reconcile_open(true));
}

#[test]
fn strict_mode_remount_rearms_alive_while_real_unmount_discards_settlements() {
    let mut state = NativeDirectoryFlowState::new();
    assert!(state.accepts_settlement());
    state.unmount();
    assert!(!state.accepts_settlement());
    state.mount();
    assert!(state.accepts_settlement());
}
