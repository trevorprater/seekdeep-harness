//! Pure `TrajectoryView` request, partial, and fold projection parity.

use std::{collections::BTreeSet, rc::Rc};

use seekdeep_client_ui_trajectory::{
    TrajectoryCell, TrajectoryCellKind, TrajectoryGroupModel, TrajectoryRecordState,
    TrajectoryRequestPurpose, TrajectorySearchOffer, TrajectoryTimelineMode, TrajectoryTurnModel,
    TrajectoryUsage, TrajectoryViewSearchController, add_trajectory_usage,
    all_trajectory_folds_selected, derive_trajectory_request_numbers, last_trajectory_cell_index,
    trajectory_collapsible_assistant_ids, trajectory_collapsible_turn_ids,
    trajectory_partial_structure_signature, trajectory_request_usage, trajectory_timeline_mode,
    trajectory_timeline_partial,
};
use serde_json::json;

#[test]
fn structure_only_partial_preserves_identity_but_removes_streaming_content() {
    let partial = json!({
        "turn": 2,
        "step": 3,
        "blocks": [
            {"kind": "text", "text": "secret"},
            {"kind": "reasoning", "text": "thought"},
            {"kind": "image", "url": "data:image/png;base64,AA"},
            {"kind": "tool-call", "callId": "call-1", "name": "read", "argsRaw": "changing"},
            {"kind": "other", "block": {"changing": true}}
        ]
    });
    assert_eq!(
        trajectory_partial_structure_signature(Some(&partial)).unwrap(),
        "text\0reasoning\0image\0tool-call:call-1:read\0other"
    );
    assert_eq!(
        trajectory_timeline_partial(Some(&partial)).unwrap(),
        Some(json!({
            "turn": 2,
            "step": 3,
            "blocks": [
                {"kind": "text", "text": ""},
                {"kind": "reasoning", "text": ""},
                {"kind": "image", "url": "data:image/png;base64,AA"},
                {"kind": "tool-call", "callId": "call-1", "name": "read", "argsRaw": ""},
                {"kind": "other", "block": null}
            ]
        }))
    );
    assert_eq!(trajectory_timeline_partial(None).unwrap(), None);
}

#[test]
fn request_numbers_merge_requests_nodes_usage_provenance_and_compaction() {
    let nodes = vec![
        json!({
            "kind": "assistant", "seq": 10, "turn": 1, "step": 1,
            "usage": {"inputTokens": 2, "outputTokens": 3},
            "provenance": {"provider": "node-provider", "model": "node-model"},
            "requestConfig": {"temperature": 0}
        }),
        json!({
            "kind": "assistant", "seq": 30, "turn": 2, "step": 1,
            "usage": {"cacheReadTokens": 5}
        }),
    ];
    let requests = vec![
        json!({
            "purpose": "assistant", "startSeq": 10, "turn": 1, "step": 1,
            "status": "complete", "startedAt": 1000, "completedAt": 2000,
            "usage": {"inputTokens": 7, "reasoningTokens": 1},
            "provenance": {"provider": "request-provider"},
            "resultSeq": 11
        }),
        json!({
            "purpose": "compaction", "startSeq": 20, "turn": null, "step": 0,
            "status": "running", "startedAt": 2500, "completedAt": null,
            "usage": {"cacheWriteTokens": 4}
        }),
    ];
    let numbered = derive_trajectory_request_numbers(&nodes, &requests).unwrap();
    assert_eq!(numbered.len(), 3);
    assert_eq!(numbered[0].number, 1);
    assert_eq!(numbered[0].group, "Step 1");
    assert_eq!(numbered[0].provider.as_deref(), Some("request-provider"));
    assert_eq!(numbered[0].model.as_deref(), Some("node-model"));
    assert_eq!(numbered[0].status, Some(TrajectoryRecordState::Complete));
    assert_eq!(numbered[0].usage.unwrap().input, Some(7));
    assert_eq!(numbered[1].purpose, TrajectoryRequestPurpose::Compaction);
    assert_eq!(numbered[1].group, "Compaction 20");
    assert_eq!(numbered[1].turn, None);
    assert_eq!(numbered[1].status, Some(TrajectoryRecordState::Running));
    assert_eq!(numbered[1].cumulative_usage.unwrap().cache_write, Some(4));
    assert_eq!(numbered[2].number, 3);
    assert_eq!(numbered[2].turn, Some(2));
    assert_eq!(numbered[2].usage.unwrap().cache_read, Some(5));
    assert_eq!(numbered[2].cumulative_usage.unwrap().input, Some(7));
}

#[test]
fn usage_modes_and_fold_ids_match_source_rules() {
    let usage = trajectory_request_usage(Some(&json!({
        "inputTokens": 1,
        "cacheReadTokens": 2,
        "outputTokens": 3,
        "reasoningTokens": 1
    })))
    .unwrap();
    assert_eq!(usage.input, Some(1));
    assert_eq!(
        add_trajectory_usage(
            Some(usage),
            Some(TrajectoryUsage {
                cache_write: Some(4),
                ..TrajectoryUsage::default()
            })
        ),
        Some(TrajectoryUsage {
            input: Some(1),
            cache_read: Some(2),
            cache_write: Some(4),
            output: Some(3),
            reasoning: Some(1),
        })
    );
    assert_eq!(
        trajectory_timeline_mode(false, false),
        TrajectoryTimelineMode::Sequence
    );
    assert_eq!(
        trajectory_timeline_mode(false, true),
        TrajectoryTimelineMode::Time
    );
    assert_eq!(
        trajectory_timeline_mode(true, false),
        TrajectoryTimelineMode::Duration
    );
    assert_eq!(
        trajectory_timeline_mode(true, true),
        TrajectoryTimelineMode::Actual
    );

    let mut assistant = TrajectoryCell::new(1, TrajectoryCellKind::Message, "assistant");
    assistant.source_seq = Some(1);
    let turns = vec![TrajectoryTurnModel {
        turn: Some(1),
        groups: vec![TrajectoryGroupModel {
            title: "Step 1".to_owned(),
            description: None,
            cells: vec![
                assistant.clone(),
                TrajectoryCell::new(2, TrajectoryCellKind::Tool, "bash"),
            ],
        }],
    }];
    assert_eq!(last_trajectory_cell_index(&turns), 2);
    assert_eq!(trajectory_collapsible_turn_ids(&turns), vec![1]);
    assert_eq!(
        trajectory_collapsible_assistant_ids(&turns),
        vec![seekdeep_client_ui_trajectory::trajectory_record_id(
            &assistant
        )]
    );
    assert!(all_trajectory_folds_selected(&[1], &BTreeSet::from([1])));
    assert!(!all_trajectory_folds_selected::<u64>(&[], &BTreeSet::new()));
}

#[test]
fn search_index_initializes_immediately_then_coalesces_one_pending_timer() {
    let first = TrajectoryTurnModel {
        turn: Some(1),
        groups: vec![TrajectoryGroupModel {
            title: "Message".to_owned(),
            description: None,
            cells: vec![TrajectoryCell::new(1, TrajectoryCellKind::User, "alpha")],
        }],
    };
    let second = TrajectoryTurnModel {
        turn: Some(2),
        groups: vec![TrajectoryGroupModel {
            title: "Message".to_owned(),
            description: None,
            cells: vec![TrajectoryCell::new(2, TrajectoryCellKind::User, "beta")],
        }],
    };
    let mut controller = TrajectoryViewSearchController::new();
    let first_layouts = Rc::new(vec![vec![first.clone()], Vec::new()]);
    assert_eq!(
        controller.offer(&first_layouts),
        TrajectorySearchOffer::Updated
    );
    assert_eq!(
        controller.offer(&first_layouts),
        TrajectorySearchOffer::None
    );
    assert!(
        controller
            .search("alpha")
            .is_some_and(|matches| matches.len() == 1)
    );

    let second_layouts = Rc::new(vec![vec![first, second.clone()], Vec::new()]);
    assert_eq!(
        controller.offer(&second_layouts),
        TrajectorySearchOffer::Schedule
    );
    assert_eq!(
        controller.offer(&Rc::new(vec![vec![second], Vec::new()])),
        TrajectorySearchOffer::None
    );
    assert!(controller.fire());
    assert!(
        controller
            .search("beta")
            .is_some_and(|matches| matches.len() == 1)
    );
    assert!(!controller.fire());
    controller.cancel();
}
