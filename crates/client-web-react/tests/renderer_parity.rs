//! Session incarnation and slot-kind renderer decision parity.

use seekdeep_client_web_react::{
    MaybeIncarnation, RenderEntry, RenderListRow, RenderSessionId, ShadowOutcome, list_rows,
    shadow_outcome,
};

#[test]
fn session_maybe_blank_incarnation_adopts_once_then_remounts_on_switch_and_loss() {
    let mut state = MaybeIncarnation::default();
    assert_eq!(state.epoch(), 0);
    assert!(state.adopted().is_none());
    assert!(!state.transition(Some(RenderSessionId::new("s1"))));
    assert_eq!(state.adopted().unwrap().as_str(), "s1");
    assert_eq!(state.epoch(), 0);
    assert!(!state.transition(Some(RenderSessionId::new("s1"))));
    assert!(state.transition(Some(RenderSessionId::new("s2"))));
    assert_eq!(state.epoch(), 1);
    assert!(state.transition(None));
    assert_eq!(state.epoch(), 2);
    assert!(state.adopted().is_none());
    assert!(!state.transition(Some(RenderSessionId::new("s3"))));
    assert_eq!(state.epoch(), 2);
}

fn entry(identity: u64, cell: Option<&str>, order: i64, winner: bool) -> RenderEntry {
    RenderEntry {
        identity,
        cell: cell.map(str::to_owned),
        order,
        winner,
    }
}

#[test]
fn single_and_keyed_cells_distinguish_natural_empty_dead_and_winner() {
    assert_eq!(shadow_outcome(&[], None), ShadowOutcome::Fallback);
    assert_eq!(
        shadow_outcome(&[entry(1, None, 0, false)], None),
        ShadowOutcome::Dead
    );
    assert_eq!(
        shadow_outcome(&[entry(1, None, 0, false), entry(2, None, 0, true)], None),
        ShadowOutcome::Winner(2)
    );
    let keyed = [entry(1, Some("a"), 0, false), entry(2, Some("b"), 0, true)];
    assert_eq!(shadow_outcome(&keyed, Some("a")), ShadowOutcome::Dead);
    assert_eq!(shadow_outcome(&keyed, Some("b")), ShadowOutcome::Winner(2));
    assert_eq!(
        shadow_outcome(&keyed, Some("missing")),
        ShadowOutcome::Fallback
    );
}

#[test]
fn list_rows_merge_winners_and_dry_cells_then_sort_and_filter() {
    let entries = [
        entry(1, Some("later"), 20, true),
        entry(2, Some("dry"), 5, false),
        entry(3, Some("early"), 10, true),
        entry(4, Some("later"), 30, false),
    ];
    assert_eq!(
        list_rows(&entries, None),
        [
            RenderListRow {
                entry: None,
                cell: Some("dry".into()),
                order: 5,
            },
            RenderListRow {
                entry: Some(3),
                cell: Some("early".into()),
                order: 10,
            },
            RenderListRow {
                entry: Some(1),
                cell: Some("later".into()),
                order: 20,
            },
        ]
    );
    assert_eq!(
        list_rows(&entries, Some("later")),
        [RenderListRow {
            entry: Some(1),
            cell: Some("later".into()),
            order: 20,
        }]
    );
    assert!(list_rows(&entries, Some("missing")).is_empty());
}
