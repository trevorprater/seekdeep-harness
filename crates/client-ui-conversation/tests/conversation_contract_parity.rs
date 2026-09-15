//! Portable coverage for Chat node and view currencies.

use std::rc::Rc;

use seekdeep_client_runtime::OptionalJson;
use seekdeep_client_runtime::{RunningToolCall, ToolCallBlock, ToolResultNode};
use seekdeep_client_ui_conversation::{
    AssistantChatData, AssistantChatStatus, ChatNodeKind, ChatNodeVisibility, ConversationCallId,
    ConversationChatStoreState, ConversationViewTab, SelectionTarget, TurnTailChatData,
    is_running_tool, is_settled_tool,
};

fn running() -> ToolCallBlock {
    ToolCallBlock::Running(RunningToolCall {
        call_id: "call-1".to_owned(),
        name: "read".to_owned(),
        args_raw: "{}".to_owned(),
        turn: 1,
        step: 2,
        time: 3,
        call_view: None,
        sub_calls: Rc::new(Vec::new()),
    })
}

#[test]
fn nominal_ids_store_defaults_extensible_kinds_and_tool_guards_match_contract() {
    let call_id = ConversationCallId::new("call-1");
    assert_eq!(call_id.as_str(), "call-1");
    let selection = SelectionTarget {
        turn_seq: 1,
        step_seq: Some(2),
        call_id: Some(call_id.clone()),
        tool_name: Some("read".to_owned()),
    };
    let store = ConversationChatStoreState {
        selection: Some(selection.clone()),
        draft: "persisted".to_owned(),
        view: Some("chat".to_owned()),
        inspect: Some(call_id),
    };
    assert_eq!(store.selection, Some(selection));
    assert_eq!(ConversationChatStoreState::default().draft, "");
    assert_eq!(
        ConversationViewTab {
            id: "chat".to_owned(),
            label: "Chat".to_owned(),
        }
        .id,
        "chat"
    );
    assert_eq!(
        ChatNodeKind::new("extension-kind").as_str(),
        "extension-kind"
    );
    assert_eq!(ChatNodeVisibility::default(), ChatNodeVisibility::Visible);

    let running = running();
    assert!(is_running_tool(&running));
    assert!(!is_settled_tool(&running));
    let settled = ToolCallBlock::Settled(Box::new(ToolResultNode {
        seq: 4,
        time: 5,
        call_id: "call-1".to_owned(),
        call: None,
        call_time: Some(3),
        content: Vec::new(),
        is_error: false,
        error: None,
        meta: OptionalJson::Absent,
        call_view: None,
        result_view: None,
        sub_calls: Rc::new(Vec::new()),
    }));
    assert!(is_settled_tool(&settled));
    assert!(!is_running_tool(&settled));

    let assistant = AssistantChatData {
        status: AssistantChatStatus::Interrupted,
        turn: 1,
        step: 2,
        blocks: Rc::new(Vec::new()),
        time: 3,
        usage: None,
        final_node: None,
    };
    let tail = TurnTailChatData {
        turn: 1,
        seq: 9,
        time: 10,
        closing: Some(assistant),
        branch_unavailable: true,
        ttft_ms: Some(12.5),
        tokens_per_second: Some(30.0),
    };
    assert!(tail.branch_unavailable);
    assert_eq!(
        tail.closing.unwrap().status,
        AssistantChatStatus::Interrupted
    );
}
