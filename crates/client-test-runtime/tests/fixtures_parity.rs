//! Native fixture-default parity under the Rust Client object model.

#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use seekdeep_client_runtime::{ComposerPhase, SessionOpenState};
use seekdeep_client_test_runtime::{SessionFixture, conversation_snapshot, workspace_list_state};
use seekdeep_identity::SessionId;

#[test]
fn conversation_and_workspace_defaults_are_quiescent_ready_values() {
    let snapshot = conversation_snapshot(SessionId::new("s1"));
    assert_eq!(snapshot.session_id.as_str(), "s1");
    assert!(snapshot.chat.is_none());
    assert!(snapshot.pending.is_empty());
    assert!(snapshot.queue.is_empty());
    assert!(!snapshot.running);
    assert!(snapshot.subagent.is_none());
    assert_eq!(snapshot.composer_phase, ComposerPhase::Active);
    assert!(!snapshot.removed);
    assert_eq!(snapshot.open_state, SessionOpenState::Open);
    assert!(snapshot.open_error.is_none());
    assert!(!snapshot.has_more);
    assert!(!snapshot.loading_older);
    assert!(snapshot.prompt_error.is_none());
    assert!(!snapshot.blank);
    assert!(snapshot.last_agent_error.is_none());

    let workspaces = workspace_list_state();
    assert!(workspaces.items.is_empty());
    assert!(workspaces.archived_session_ids.is_empty());
    assert!(workspaces.baselines_ready);
    assert!(workspaces.recent_workspace_id.is_none());
}

#[test]
fn fixture_overrides_are_typed_against_production_snapshots() {
    let mut fixture = SessionFixture::new("s1");
    fixture.snapshot = Some(Rc::new(|snapshot| {
        snapshot.running = true;
        snapshot.blank = true;
    }));
    let mut snapshot = conversation_snapshot(SessionId::new(&fixture.id));
    fixture.snapshot.as_ref().unwrap()(&mut snapshot);
    assert!(snapshot.running);
    assert!(snapshot.blank);
}
