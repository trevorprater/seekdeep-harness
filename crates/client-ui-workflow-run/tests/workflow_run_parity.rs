//! Durable workflow-run fold, projection, and renderer-decision parity.

use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationEventInput, ConversationLocationEvent, ConversationNodeAssembler,
    ConversationTimelineSnapshot, ConversationViewNode,
};
use seekdeep_client_ui_workflow_run::{
    WORKFLOW_RUN_EN, WORKFLOW_RUN_NS, WORKFLOW_RUN_ZH, WorkflowDotState, WorkflowRunChatData,
    WorkflowRunMemberData, WorkflowRunPhaseData, WorkflowRunStatus, WorkflowSessionSummary,
    navigable_members, phase_requires_expansion, phase_status_counts, run_requires_expansion,
    workflow_dot_state, workflow_phase_key, workflow_run_definition,
};
use seekdeep_identity::SessionId;
use serde_json::{Map, Value, json};

struct EventDefinitions(Vec<Rc<AssemblerNodeDefinition>>);

impl AssemblerEventDefinitions for EventDefinitions {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        self.0.clone()
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        None
    }
}

struct ViewDefinitions(Vec<Rc<AssemblerViewDefinition>>);

impl AssemblerViewDefinitions for ViewDefinitions {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        self.0.clone()
    }
}

#[derive(Default)]
struct ChatBuilder {
    nodes: Vec<Rc<ConversationViewNode>>,
}

impl ChatBuilder {
    fn snapshot(&self) -> Rc<Value> {
        let nodes = self
            .nodes
            .iter()
            .map(|node| (node.key.clone(), node.data.as_ref().clone()))
            .collect::<Map<_, _>>();
        Rc::new(json!({"nodes":nodes}))
    }
}

impl AssemblerViewBuilder for ChatBuilder {
    fn empty(&self) -> Rc<Value> {
        self.snapshot()
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        self.nodes = nodes.to_vec();
        Ok(self.snapshot())
    }

    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        for upsert in upserts {
            if let Some(node) = self.nodes.iter_mut().find(|node| node.key == upsert.key) {
                *node = upsert.clone();
            } else {
                self.nodes.push(upsert.clone());
            }
        }
        Ok(self.snapshot())
    }
}

fn assembler() -> ConversationNodeAssembler {
    ConversationNodeAssembler::new(
        Rc::new(EventDefinitions(vec![Rc::new(workflow_run_definition())])),
        Rc::new(ViewDefinitions(vec![Rc::new(AssemblerViewDefinition {
            target: "chat".to_owned(),
            create: Rc::new(|| Box::<ChatBuilder>::default()),
        })])),
    )
}

fn at(seq: u64, event_type: &str, data: Value) -> ConversationEventInput {
    ConversationEventInput {
        event: ConversationLocationEvent::with_time(
            seq,
            i64::try_from(seq * 100).unwrap(),
            event_type,
            data,
        ),
        view: None,
    }
}

fn complete_events() -> Vec<ConversationEventInput> {
    vec![
        at(1, "turn/start", json!({"turn":1})),
        at(2, "step/start", json!({"turn":1,"step":1})),
        at(
            3,
            "tool-workflow/run-start",
            json!({"runId":"run-1","name":"audit"}),
        ),
        at(
            4,
            "tool-workflow/agent-start",
            json!({
                "runId":"run-1","seq":1,"label":"first","phase":"","childId":"child-1",
            }),
        ),
        at(
            5,
            "tool-workflow/agent-start",
            json!({"runId":"run-1","seq":2,"label":"second","childId":"child-2"}),
        ),
        at(
            6,
            "tool-workflow/agent-end",
            json!({"runId":"run-1","seq":1,"outcome":"completed"}),
        ),
        at(
            7,
            "tool-workflow/agent-end",
            json!({"runId":"run-1","seq":2,"outcome":"failed"}),
        ),
        at(
            8,
            "tool-workflow/run-end",
            json!({"runId":"run-1","stopReason":"error"}),
        ),
        at(9, "step/end", json!({"turn":1,"step":1})),
        at(
            10,
            "turn/end",
            json!({"turn":1,"reason":{"kind":"completed"}}),
        ),
    ]
}

fn workflow_data(value: &ConversationNodeAssembler) -> Option<WorkflowRunChatData> {
    let snapshot = value.snapshot("chat")?;
    let data = snapshot.get("nodes")?.as_object()?.values().next()?.clone();
    serde_json::from_value(data).ok()
}

#[test]
fn replay_groups_exact_phase_identities_and_preserves_terminal_members() {
    let mut value = assembler();
    value.replace_window(&complete_events(), false).unwrap();
    value.flush().unwrap();
    assert_eq!(
        workflow_data(&value),
        Some(WorkflowRunChatData {
            name: "audit".to_owned(),
            status: WorkflowRunStatus::Failed,
            phases: vec![
                WorkflowRunPhaseData {
                    key: "value:0:".to_owned(),
                    phase: Some(String::new()),
                    members: vec![WorkflowRunMemberData {
                        seq: 1,
                        label: "first".to_owned(),
                        child_id: SessionId::new("child-1"),
                        status: WorkflowRunStatus::Completed,
                    }],
                },
                WorkflowRunPhaseData {
                    key: "missing".to_owned(),
                    phase: None,
                    members: vec![WorkflowRunMemberData {
                        seq: 2,
                        label: "second".to_owned(),
                        child_id: SessionId::new("child-2"),
                        status: WorkflowRunStatus::Failed,
                    }],
                },
            ],
        })
    );
}

#[test]
fn update_only_tail_waits_for_prepend_and_live_append_matches_replay() {
    let events = complete_events();
    let mut paged = assembler();
    paged.replace_window(&events[3..], true).unwrap();
    paged.flush().unwrap();
    assert_eq!(workflow_data(&paged), None);
    paged.prepend(&events[..3], false).unwrap();
    paged.flush().unwrap();

    let mut live = assembler();
    live.replace_window(&events[..3], false).unwrap();
    for event in &events[3..] {
        live.append(event).unwrap();
    }
    live.flush().unwrap();

    let mut replay = assembler();
    replay.replace_window(&events, false).unwrap();
    replay.flush().unwrap();
    assert_eq!(workflow_data(&paged), workflow_data(&replay));
    assert_eq!(workflow_data(&live), workflow_data(&replay));
}

#[test]
fn owning_location_closure_marks_missing_terminal_facts_interrupted() {
    let mut value = assembler();
    let open = vec![
        at(1, "turn/start", json!({"turn":1})),
        at(2, "step/start", json!({"turn":1,"step":1})),
        at(
            3,
            "tool-workflow/run-start",
            json!({"runId":"run-1","name":"audit"}),
        ),
        at(
            4,
            "tool-workflow/agent-start",
            json!({"runId":"run-1","seq":1,"label":"worker","childId":"child-1"}),
        ),
    ];
    value.replace_window(&open, false).unwrap();
    value.flush().unwrap();
    assert_eq!(
        workflow_data(&value).unwrap().status,
        WorkflowRunStatus::Running
    );
    value
        .append(&at(5, "step/end", json!({"turn":1,"step":1})))
        .unwrap();
    value.flush().unwrap();
    let data = workflow_data(&value).unwrap();
    assert_eq!(data.status, WorkflowRunStatus::Interrupted);
    assert_eq!(
        data.phases[0].members[0].status,
        WorkflowRunStatus::Interrupted
    );
}

#[test]
fn zero_member_completion_and_turn_level_cancellation_are_retained() {
    let mut empty = assembler();
    empty
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn":1})),
                at(
                    2,
                    "tool-workflow/run-start",
                    json!({"runId":"empty","name":"empty"}),
                ),
                at(
                    3,
                    "tool-workflow/run-end",
                    json!({"runId":"empty","stopReason":"completed"}),
                ),
            ],
            false,
        )
        .unwrap();
    empty.flush().unwrap();
    assert_eq!(
        workflow_data(&empty),
        Some(WorkflowRunChatData {
            name: "empty".to_owned(),
            status: WorkflowRunStatus::Completed,
            phases: Vec::new(),
        })
    );

    let mut cancelled = assembler();
    cancelled
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn":1})),
                at(
                    2,
                    "tool-workflow/run-start",
                    json!({"runId":"cancelled","name":"cancelled"}),
                ),
                at(
                    3,
                    "tool-workflow/agent-start",
                    json!({
                        "runId":"cancelled","seq":1,"label":"one","phase":"Research","childId":"child-1",
                    }),
                ),
                at(
                    4,
                    "tool-workflow/agent-end",
                    json!({"runId":"cancelled","seq":1,"outcome":"cancelled"}),
                ),
                at(
                    5,
                    "tool-workflow/run-end",
                    json!({"runId":"cancelled","stopReason":"cancelled"}),
                ),
            ],
            false,
        )
        .unwrap();
    cancelled.flush().unwrap();
    let data = workflow_data(&cancelled).unwrap();
    assert_eq!(data.status, WorkflowRunStatus::Cancelled);
    assert_eq!(
        data.phases[0].members[0].status,
        WorkflowRunStatus::Cancelled
    );
}

#[test]
fn phase_keys_use_javascript_utf16_length_and_preserve_absent_empty_identity() {
    assert_eq!(workflow_phase_key(None), "missing");
    assert_eq!(workflow_phase_key(Some("")), "value:0:");
    assert_eq!(workflow_phase_key(Some("A")), "value:1:A");
    assert_eq!(workflow_phase_key(Some("🦀")), "value:2:🦀");
}

fn phase(statuses: &[WorkflowRunStatus]) -> WorkflowRunPhaseData {
    WorkflowRunPhaseData {
        key: "missing".to_owned(),
        phase: None,
        members: statuses
            .iter()
            .enumerate()
            .map(|(index, status)| WorkflowRunMemberData {
                seq: u64::try_from(index + 1).unwrap(),
                label: format!("member-{index}"),
                child_id: SessionId::new(format!("child-{index}")),
                status: *status,
            })
            .collect(),
    }
}

#[test]
fn panel_status_and_disclosure_decisions_match_the_source_ordering() {
    assert_eq!(
        workflow_dot_state(WorkflowRunStatus::Running),
        WorkflowDotState::Ongoing
    );
    assert_eq!(
        workflow_dot_state(WorkflowRunStatus::Completed),
        WorkflowDotState::Done
    );
    assert_eq!(
        workflow_dot_state(WorkflowRunStatus::Failed),
        WorkflowDotState::Error
    );
    assert_eq!(
        workflow_dot_state(WorkflowRunStatus::Interrupted),
        WorkflowDotState::Warning
    );

    let clean = phase(&[WorkflowRunStatus::Completed, WorkflowRunStatus::Completed]);
    assert!(!phase_requires_expansion(&clean));
    assert_eq!(
        phase_status_counts(&clean),
        vec![(WorkflowRunStatus::Completed, 2)]
    );
    assert!(!run_requires_expansion(
        WorkflowRunStatus::Completed,
        &[clean]
    ));

    let interrupted = phase(&[
        WorkflowRunStatus::Completed,
        WorkflowRunStatus::Running,
        WorkflowRunStatus::Failed,
        WorkflowRunStatus::Cancelled,
        WorkflowRunStatus::Interrupted,
    ]);
    assert!(phase_requires_expansion(&interrupted));
    assert_eq!(
        phase_status_counts(&interrupted),
        vec![
            (WorkflowRunStatus::Completed, 1),
            (WorkflowRunStatus::Running, 1),
            (WorkflowRunStatus::Failed, 1),
            (WorkflowRunStatus::Cancelled, 1),
            (WorkflowRunStatus::Interrupted, 1),
        ]
    );
    assert!(run_requires_expansion(
        WorkflowRunStatus::Completed,
        &[interrupted]
    ));
}

#[test]
fn navigation_requires_a_live_ordinary_subagent_owned_by_the_exact_parent() {
    let parent = SessionId::new("parent");
    let phases = vec![phase(&[
        WorkflowRunStatus::Running,
        WorkflowRunStatus::Running,
        WorkflowRunStatus::Completed,
        WorkflowRunStatus::Running,
    ])];
    let ids = [
        SessionId::new("child-0"),
        SessionId::new("child-1"),
        SessionId::new("child-2"),
    ];
    let summaries = vec![
        WorkflowSessionSummary {
            id: SessionId::new("child-0"),
            subagent: true,
            parent_id: Some(parent.clone()),
            running: true,
        },
        WorkflowSessionSummary {
            id: SessionId::new("child-1"),
            subagent: true,
            parent_id: Some(SessionId::new("other")),
            running: true,
        },
        WorkflowSessionSummary {
            id: SessionId::new("child-2"),
            subagent: true,
            parent_id: Some(parent.clone()),
            running: true,
        },
        WorkflowSessionSummary {
            id: SessionId::new("child-3"),
            subagent: true,
            parent_id: Some(parent.clone()),
            running: true,
        },
    ];
    assert_eq!(
        navigable_members(&ids, &summaries, &phases, &parent),
        vec![SessionId::new("child-0")]
    );
}

#[test]
fn locale_namespace_and_parallel_dictionaries_are_exact() {
    assert_eq!(WORKFLOW_RUN_NS, "workflowRun");
    assert_eq!(WORKFLOW_RUN_ZH.len(), 18);
    assert_eq!(WORKFLOW_RUN_EN.len(), 18);
    assert!(
        WORKFLOW_RUN_ZH
            .iter()
            .zip(WORKFLOW_RUN_EN)
            .all(|(zh, en)| zh.0 == en.0)
    );
    assert_eq!(WORKFLOW_RUN_ZH[1], ("run.members.one", "{count} 个成员"));
    assert_eq!(
        WORKFLOW_RUN_ZH[10],
        ("statusCount.interrupted", "已中断 {count}")
    );
    assert_eq!(WORKFLOW_RUN_EN[2], ("run.members.other", "{count} members"));
    assert_eq!(WORKFLOW_RUN_EN[17], ("status.interrupted", "Interrupted"));
}
