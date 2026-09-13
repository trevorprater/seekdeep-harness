//! Request-inspection and model-context structural contract parity.

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssistantProvenanceView, AssistantRequestConfig, ConversationContext,
    ConversationContextOriginKind, ConversationPromptSnapshot, OptionalJson,
    RequestInspectionSnapshot, RequestPromptChange, RequestPromptChangeKind, RequestStatus,
    RequestView, RequestViewBase,
};
use seekdeep_lossless_json::{JsonString, JsonValue as Value};

macro_rules! json {
    ($($tokens:tt)*) => { Value::from(serde_json::json!($($tokens)*)) };
}

fn config() -> AssistantRequestConfig {
    AssistantRequestConfig {
        provider: "deepseek-official".into(),
        model: "deepseek-v4-flash".into(),
        purpose: Some("agent".into()),
        thinking: Some("enabled".into()),
        reasoning_effort: Some("high".into()),
        temperature: Some(0.2),
        max_tokens: Some(4_096),
        stop: Some(vec!["END".into()]),
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
            provider: "deepseek-official".into(),
            model: "deepseek-v4-flash".into(),
        }),
        request_config: Some(config()),
        usage: OptionalJson::Absent,
        result_seq: None,
    }
}

fn prompt() -> ConversationPromptSnapshot {
    ConversationPromptSnapshot {
        config: config(),
        system: "You are SeekDeep.".into(),
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
    let value = Value::from_serialize(&request).unwrap();
    assert_eq!(value["purpose"], "assistant");
    assert_eq!(value["startSeq"], 11);
    assert_eq!(value["completedAt"], Value::from(serde_json::Value::Null));
    assert_eq!(value["status"], "running");
    assert_eq!(value["requestConfig"]["reasoningEffort"], "high");
    assert_eq!(value["requestConfig"]["maxTokens"], 4_096);
    assert_eq!(value["promptChange"]["kind"], "system-and-tools");
    assert_eq!(value["retryDelayMs"], 500);
    assert!(value.get_value("error").is_none());
    assert_eq!(value.deserialize::<RequestView>().unwrap(), request);
}

#[test]
fn compaction_request_keeps_required_null_turn_zero_step_and_complete_outputs() {
    let request = RequestView::Compaction {
        base: Box::new(RequestViewBase {
            completed_at: Some(2_000),
            status: RequestStatus::Complete,
            usage: OptionalJson::Present(Value::from(serde_json::Value::Null)),
            result_seq: Some(20),
            ..base()
        }),
        turn: None,
        step: 0,
        replacement_seq: Some(21),
        summary: Some(vec![json!({"type":"text","text":"safe"})]),
        raw_output: Some(vec![json!({"type":"reasoning","text":"raw"})]),
    };
    let value = Value::from_serialize(&request).unwrap();
    assert_eq!(value["purpose"], "compaction");
    assert_eq!(value["turn"], Value::from(serde_json::Value::Null));
    assert_eq!(value["step"], 0);
    assert_eq!(value["replacementSeq"], 21);
    assert_eq!(value["summary"][0]["text"], "safe");
    assert_eq!(value["rawOutput"][0]["text"], "raw");
    assert!(value.get_value("usage").is_some_and(Value::is_null));
    assert_eq!(value.deserialize::<RequestView>().unwrap(), request);
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
            ("call-b".into(), json!({"name":"beta"})),
            ("call-a".into(), json!({"name":"alpha"})),
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
    let value = Value::from_serialize(&snapshot).unwrap();
    assert!(value.get_value("callSchemas").is_some());
    assert_eq!(
        value.deserialize::<RequestInspectionSnapshot>().unwrap(),
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
    let value = Value::from_serialize(&context).unwrap();
    assert_eq!(value["parentId"], 1);
    assert_eq!(value["origin"], "rewrite");
    assert_eq!(value["originSeq"], 30);
    assert_eq!(value["createdAt"], 3_000);
    assert_eq!(value.deserialize::<ConversationContext>().unwrap(), context);
}

#[test]
fn request_and_context_snapshots_preserve_raw_text_values_and_keys() {
    let raw = Value::parse(r#"{"requests":[{"purpose":"assistant","startSeq":1,"startedAt":2,"completedAt":null,"status":"running","usage":{"\udfff":"\ud800"},"turn":1,"step":1,"prompt":{"config":{"provider":"p","model":"m","stop":["\ud800"]},"system":"\udfff","tools":[{"name":"t","parameters":{"\ud800":"\udfff"}}]}}],"callSchemas":{"c":{"\ud800":"\udfff"}}}"#.to_owned()).unwrap();
    let snapshot: RequestInspectionSnapshot = raw.deserialize().unwrap();
    let encoded = Value::from_serialize(&snapshot).unwrap();
    assert_eq!(encoded, raw);
    let RequestView::Assistant {
        prompt: Some(prompt),
        base,
        ..
    } = &snapshot.requests[0]
    else {
        panic!("assistant prompt missing");
    };
    assert_eq!(prompt.system.utf16_units(), &[0xdfff]);
    assert_eq!(
        prompt.config.stop.as_ref().unwrap()[0].utf16_units(),
        &[0xd800]
    );
    assert_eq!(
        base.usage,
        OptionalJson::Present(Value::parse(r#"{"\udfff":"\ud800"}"#.to_owned()).unwrap())
    );
    let context_raw =
        Value::parse(r#"{"id":0,"nodes":[{"kind":"assistant","text":"\ud800"}]}"#.to_owned())
            .unwrap();
    let context: ConversationContext = context_raw.deserialize().unwrap();
    assert_eq!(Value::from_serialize(&context).unwrap(), context_raw);
    assert_eq!(JsonString::from_utf16(&[0xdfff]), prompt.system);
}

#[test]
fn compaction_request_roundtrip_preserves_summary_raw_output_and_present_null_usage() {
    let raw = Value::parse(r#"{"purpose":"compaction","startSeq":4,"startedAt":5,"completedAt":6,"status":"error","error":"\udfff","usage":null,"turn":null,"step":0,"summary":[{"type":"text","text":"\ud800"}],"rawOutput":[{"type":"future","\ud800":{"text":"\udfff"}}]}"#.to_owned()).unwrap();
    let request: RequestView = raw.deserialize().unwrap();
    let encoded = Value::from_serialize(&request).unwrap();
    assert_eq!(encoded, raw);
    let RequestView::Compaction {
        base,
        summary: Some(summary),
        raw_output: Some(raw_output),
        ..
    } = request
    else {
        panic!("compaction output missing");
    };
    assert_eq!(
        base.usage,
        OptionalJson::Present(Value::from(serde_json::Value::Null))
    );
    assert_eq!(base.error.unwrap().utf16_units(), &[0xdfff]);
    assert_eq!(summary[0]["text"].to_utf16().unwrap(), vec![0xd800]);
    assert_eq!(raw_output[0]["type"], "future");
    assert!(raw_output[0].clone().try_into_serde_json().is_err());
}
