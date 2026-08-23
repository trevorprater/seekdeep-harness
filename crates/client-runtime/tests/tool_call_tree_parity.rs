//! Tool child-call graph cycle, depth, settlement, and structural-sharing parity.

use std::rc::Rc;

use seekdeep_client_runtime::*;
use serde_json::json;

fn start(seq: i64, parent: &str, child: &str) -> ToolDispatchEvent {
    ToolDispatchEvent::Start {
        time: 1_700_000_000_000 + seq,
        parent_call_id: parent.to_owned(),
        sub_call_id: child.to_owned(),
        name: "run_code".to_owned(),
        arguments: json!({}),
    }
}

fn settle(seq: i64, parent: &str, child: &str) -> ToolDispatchEvent {
    ToolDispatchEvent::Settle {
        seq,
        time: 1_700_000_000_000 + seq,
        parent_call_id: parent.to_owned(),
        sub_call_id: child.to_owned(),
        name: "run_code".to_owned(),
        arguments: json!({}),
        content: Vec::new(),
        is_error: false,
    }
}

fn root(id: &str) -> Rc<ToolCallBlock> {
    Rc::new(ToolCallBlock::Running(RunningToolCall {
        call_id: id.to_owned(),
        name: "run_code".to_owned(),
        args_raw: "{}".to_owned(),
        turn: 1,
        step: 1,
        time: 1_700_000_000_000,
        sub_calls: Rc::new(Vec::new()),
    }))
}

#[test]
fn self_parenting_is_consumed_but_not_projected() {
    let mut tree = ToolCallTree::default();
    let roots = Rc::new(vec![root("root")]);
    assert!(tree.apply(&start(0, "root", "root")));
    assert!(Rc::ptr_eq(
        &tree.project_running_calls(roots.clone()),
        &roots
    ));
}

#[test]
fn settling_edge_that_closes_cycle_is_rejected_without_hiding_existing_tree() {
    let mut tree = ToolCallTree::default();
    tree.apply(&start(0, "a", "b"));
    tree.apply(&start(1, "b", "c"));
    assert!(tree.apply(&settle(2, "c", "a")));
    let projected = tree.project_running_calls(Rc::new(vec![root("a")]));
    assert_eq!(projected[0].sub_calls()[0].call_id(), "b");
    assert_eq!(projected[0].sub_calls()[0].sub_calls()[0].call_id(), "c");
}

#[test]
fn acyclic_graph_accepts_shared_descendant_and_projects_each_parent_branch() {
    let mut tree = ToolCallTree::default();
    for event in [
        start(0, "a", "b"),
        start(1, "a", "c"),
        start(2, "b", "d"),
        start(3, "c", "d"),
        start(4, "root", "a"),
    ] {
        tree.apply(&event);
    }
    let projected = tree.project_running_calls(Rc::new(vec![root("root")]));
    let a = &projected[0].sub_calls()[0];
    assert_eq!(a.sub_calls().len(), 2);
    assert_eq!(a.sub_calls()[0].sub_calls()[0].call_id(), "d");
    assert_eq!(a.sub_calls()[1].sub_calls()[0].call_id(), "d");
}

#[test]
fn edge_beyond_depth_ceiling_is_rejected() {
    let mut tree = ToolCallTree::default();
    for depth in 1..MAX_TOOL_CALL_TREE_DEPTH {
        tree.apply(&start(
            depth.to_string().parse().unwrap(),
            &format!("call-{}", depth - 1),
            &format!("call-{depth}"),
        ));
    }
    tree.apply(&start(
        MAX_TOOL_CALL_TREE_DEPTH.to_string().parse().unwrap(),
        &format!("call-{}", MAX_TOOL_CALL_TREE_DEPTH - 1),
        &format!("call-{MAX_TOOL_CALL_TREE_DEPTH}"),
    ));
    let projected = tree.project_running_calls(Rc::new(vec![root("call-0")]));
    let mut current = projected[0].clone();
    let mut depth = 1;
    while let Some(child) = current.sub_calls().first() {
        current = child.clone();
        depth += 1;
    }
    assert_eq!(depth, MAX_TOOL_CALL_TREE_DEPTH);
    assert_eq!(
        current.call_id(),
        format!("call-{}", MAX_TOOL_CALL_TREE_DEPTH - 1)
    );
}

#[test]
fn settlement_replaces_started_child_and_projection_caches_unchanged_inputs() {
    let mut tree = ToolCallTree::default();
    tree.apply(&start(0, "root", "child"));
    let roots = Rc::new(vec![root("root")]);
    let running = tree.project_running_calls(roots.clone());
    assert!(Rc::ptr_eq(&running, &tree.project_running_calls(roots)));
    tree.apply(&settle(1, "root", "child"));
    let settled = tree.project_running_calls(running);
    assert!(matches!(
        settled[0].sub_calls()[0].as_ref(),
        ToolCallBlock::Settled(ToolResultNode {
            call_time: Some(_),
            ..
        })
    ));
    tree.reset();
    let base = Rc::new(vec![root("root")]);
    assert!(Rc::ptr_eq(&base, &tree.project_running_calls(base.clone())));
}
