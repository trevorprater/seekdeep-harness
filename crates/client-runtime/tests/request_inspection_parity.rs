//! Request-inspection and model-context structural contract parity.

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssistantProvenanceView, AssistantRequestConfig, ConversationContext,
    ConversationContextOriginKind, ConversationPromptSnapshot, OptionalJson,
    RequestInspectionSnapshot, RequestPromptChange, RequestPromptChangeKind, RequestStatus,
    RequestView, RequestViewBase,
};
use serde_json::{Value, json};

fn config() -> AssistantRequestConfig {
    AssistantRequestConfig {
        provider: "deepseek-official".to_owned(),
        model: "deepseek-v4-flash".to_owned(),
        purpose: Some("agent".to_owned()),
        thinking: Some("enabled".to_owned()),
        reasoning_effort: Some("high".to_owned()),
        temperature: Some(0.2),
        max_tokens: Some(4_096),
        stop: Some(vec!["END".to_owned()]),
    }
}

fn base() -> RequestViewBase {
    RequestViewBase {
        start_seq: 11,
        started_at: 1_000,
        completed_at: None,
        status: RequestStatus::Running,
        error: None,
        provenance: Some(AssistantProvenanceView {
            provider: "deepseek-official".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
        }),
        request_config: Some(config()),
        usage: OptionalJson::Absent,
        result_seq: None,
    }
}

fn prompt() -> ConversationPromptSnapshot {
    ConversationPromptSnapshot {
        config: config(),
        system: "You are SeekDeep.".to_owned(),
        tools: vec![json!({"name":"bash","description":"Run a command"})],
    }
}

#[test]
fn assistant_request_uses_exact_discriminant_casing_nullability_and_prompt_change_shape() {
    let request = RequestView::Assistant {
        base: Box::new(base()),
        turn: 2,
        step: 3,
        prompt: Some(Box::new(prompt())),
        prompt_change: Some(Box::new(RequestPromptChange {
            seq: 11,
            time: 1_000,
            kind: RequestPromptChangeKind::SystemAndTools,
            previous: None,
        })),
        retry: Some(1),
        max_retries: Some(3),
        retry_delay_ms: Some(500),
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["purpose"], "assistant");
    assert_eq!(value["startSeq"], 11);
    assert_eq!(value["completedAt"], Value::Null);
    assert_eq!(value["status"], "running");
    assert_eq!(value["requestConfig"]["reasoningEffort"], "high");
    assert_eq!(value["requestConfig"]["maxTokens"], 4_096);
    assert_eq!(value["promptChange"]["kind"], "system-and-tools");
    assert_eq!(value["retryDelayMs"], 500);
    assert!(value.get("error").is_none());
    assert_eq!(
        serde_json::from_value::<RequestView>(value).unwrap(),
        request
    );
}

#[test]
fn compaction_request_keeps_required_null_turn_zero_step_and_complete_outputs() {
    let request = RequestView::Compaction {
        base: Box::new(RequestViewBase {
            completed_at: Some(2_000),
            status: RequestStatus::Complete,
            usage: OptionalJson::Present(Value::Null),
            result_seq: Some(20),
            ..base()
        }),
        turn: None,
        step: 0,
        replacement_seq: Some(21),
        summary: Some(vec![json!({"type":"text","text":"safe"})]),
        raw_output: Some(vec![json!({"type":"reasoning","text":"raw"})]),
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["purpose"], "compaction");
    assert_eq!(value["turn"], Value::Null);
    assert_eq!(value["step"], 0);
    assert_eq!(value["replacementSeq"], 21);
    assert_eq!(value["summary"][0]["text"], "safe");
    assert_eq!(value["rawOutput"][0]["text"], "raw");
    assert!(value.get("usage").is_some_and(Value::is_null));
    assert_eq!(
        serde_json::from_value::<RequestView>(value).unwrap(),
        request
    );
}

#[test]
fn inspection_snapshot_preserves_request_and_call_schema_insertion_order() {
    let snapshot = RequestInspectionSnapshot {
        requests: vec![RequestView::Assistant {
            base: Box::new(base()),
            turn: 1,
            step: 1,
            prompt: None,
            prompt_change: None,
            retry: None,
            max_retries: None,
            retry_delay_ms: None,
        }],
        call_schemas: IndexMap::from([
            ("call-b".to_owned(), json!({"name":"beta"})),
            ("call-a".to_owned(), json!({"name":"alpha"})),
        ]),
    };
    assert_eq!(
        snapshot
            .call_schemas
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["call-b", "call-a"]
    );
    let value = serde_json::to_value(&snapshot).unwrap();
    assert!(value.get("callSchemas").is_some());
    assert_eq!(
        serde_json::from_value::<RequestInspectionSnapshot>(value).unwrap(),
        snapshot
    );
}

#[test]
fn conversation_context_uses_zero_based_parented_generations_and_closed_origins() {
    let context = ConversationContext {
        id: 2,
        parent_id: Some(1),
        origin: Some(ConversationContextOriginKind::Rewrite),
        origin_seq: Some(30),
        created_at: Some(3_000),
        prompt: Some(Box::new(prompt())),
        nodes: vec![json!({"kind":"assistant","seq":31})],
    };
    let value = serde_json::to_value(&context).unwrap();
    assert_eq!(value["parentId"], 1);
    assert_eq!(value["origin"], "rewrite");
    assert_eq!(value["originSeq"], 30);
    assert_eq!(value["createdAt"], 3_000);
    assert_eq!(
        serde_json::from_value::<ConversationContext>(value).unwrap(),
        context
    );
}
