//! Behavioral mirror of `packages/llm/llm-deepseek/tests/serialize.spec.ts`.

use seekdeep_attachment::{AttachmentId, ImageAttachmentRef, ImageMediaType};
use seekdeep_llm::{
    CallId, ContentBlock, GenerateOptions, LlmRequestPurpose, Message, MessageRole, MessageSource,
    ReasoningEffortId, ToolSchema,
};
use seekdeep_llm_deepseek::{
    ReasoningEffort, RequestDefaults,
    serialize::{serialize_messages, serialize_request},
    types::ThinkingMode,
};
use serde_json::{Value, json};

fn message(role: MessageRole, content: Vec<ContentBlock>) -> Message {
    Message::new(role, content, MessageSource::plugin("test"))
}

fn request() -> GenerateOptions {
    GenerateOptions::new(
        seekdeep_llm::ProviderId::new("deepseek-official"),
        seekdeep_llm::ModelId::new("deepseek-v4-flash"),
        Vec::new(),
    )
}

fn value<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap()
}

#[test]
fn serializes_roles_text_reasoning_and_parallel_tool_calls() {
    let messages = vec![
        message(
            MessageRole::System,
            vec![ContentBlock::Text {
                text: "be brief".to_owned(),
            }],
        ),
        message(
            MessageRole::User,
            vec![
                ContentBlock::Text {
                    text: "hello ".to_owned(),
                },
                ContentBlock::Text {
                    text: "world".to_owned(),
                },
            ],
        ),
        message(
            MessageRole::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "think".to_owned(),
                },
                ContentBlock::ToolCall {
                    id: CallId::new("a"),
                    name: "one".to_owned(),
                    arguments: "{}".to_owned(),
                },
                ContentBlock::ToolCall {
                    id: CallId::new("b"),
                    name: "two".to_owned(),
                    arguments: "{}".to_owned(),
                },
            ],
        ),
    ];
    assert_eq!(
        value(serialize_messages(&messages).unwrap()),
        json!([
            {"role":"system","content":"be brief"},
            {"role":"user","content":"hello world"},
            {
                "role":"assistant",
                "content":"",
                "reasoning_content":"think",
                "tool_calls":[
                    {"id":"a","type":"function","function":{"name":"one","arguments":"{}"}},
                    {"id":"b","type":"function","function":{"name":"two","arguments":"{}"}}
                ]
            }
        ])
    );
}

#[test]
fn plain_and_reasoning_only_assistant_turns_always_replay_string_content() {
    let messages = vec![
        message(
            MessageRole::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "ignored".to_owned(),
                },
                ContentBlock::Text {
                    text: "answer".to_owned(),
                },
            ],
        ),
        message(
            MessageRole::Assistant,
            vec![ContentBlock::Reasoning {
                text: "reasoning only".to_owned(),
            }],
        ),
        message(MessageRole::Assistant, vec![]),
    ];
    assert_eq!(
        value(serialize_messages(&messages).unwrap()),
        json!([
            {"role":"assistant","content":"answer"},
            {"role":"assistant","content":""},
            {"role":"assistant","content":""}
        ])
    );
}

#[test]
fn expands_mixed_tool_results_and_supplies_empty_output_sentinel() {
    let messages = [message(
        MessageRole::User,
        vec![
            ContentBlock::Text {
                text: "context note".to_owned(),
            },
            ContentBlock::ToolResult {
                tool_call_id: CallId::new("call-1"),
                content: vec![ContentBlock::Text {
                    text: "ok".to_owned(),
                }],
                is_error: None,
            },
            ContentBlock::ToolResult {
                tool_call_id: CallId::new("call-2"),
                content: vec![],
                is_error: Some(true),
            },
        ],
    )];
    assert_eq!(
        value(serialize_messages(&messages).unwrap()),
        json!([
            {"role":"user","content":"context note"},
            {"role":"tool","tool_call_id":"call-1","content":"ok"},
            {"role":"tool","tool_call_id":"call-2","content":"(no output)"}
        ])
    );
}

#[test]
fn skips_unknown_blocks_keeps_empty_user_and_rejects_nested_images() {
    let unknown = ContentBlock::Unknown {
        block_type: "chart".to_owned(),
        fields: serde_json::Map::from_iter([("data".to_owned(), json!("x"))]),
    };
    assert_eq!(
        value(
            serialize_messages(&[
                message(
                    MessageRole::User,
                    vec![
                        unknown,
                        ContentBlock::Text {
                            text: "see chart".to_owned(),
                        },
                    ],
                ),
                message(MessageRole::User, vec![]),
            ])
            .unwrap()
        ),
        json!([
            {"role":"user","content":"see chart"},
            {"role":"user","content":""}
        ])
    );

    let image = ContentBlock::Image {
        attachment: ImageAttachmentRef {
            attachment_id: AttachmentId::new(format!("sha256:{}", "a".repeat(64))),
            media_type: ImageMediaType::Png,
            bytes: 68,
            width: 1,
            height: 1,
            name: None,
        },
    };
    let error = serialize_messages(&[message(
        MessageRole::User,
        vec![ContentBlock::ToolResult {
            tool_call_id: CallId::new("call"),
            content: vec![image],
            is_error: None,
        }],
    )])
    .unwrap_err();
    assert_eq!(error.code(), "UNSUPPORTED_CONTENT");
}

#[test]
fn request_maps_basics_system_tools_sampling_and_stop() {
    let mut request = request();
    request.system = Some("be helpful".to_owned());
    request.messages = vec![message(
        MessageRole::User,
        vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
    )];
    request.temperature = Some(0.2);
    request.max_tokens = Some(100);
    request.stop = Some(vec!["END".to_owned()]);
    request.tools = Some(vec![ToolSchema {
        name: "a".to_owned(),
        description: "A".to_owned(),
        parameters: serde_json::Map::from_iter([
            ("type".to_owned(), json!("object")),
            ("properties".to_owned(), json!({})),
        ]),
    }]);
    assert_eq!(
        value(serialize_request(&request, RequestDefaults::default()).unwrap()),
        json!({
            "model":"deepseek-v4-flash",
            "messages":[
                {"role":"system","content":"be helpful"},
                {"role":"user","content":"hi"}
            ],
            "stream":true,
            "stream_options":{"include_usage":true},
            "tools":[{"type":"function","function":{"name":"a","description":"A","parameters":{"type":"object","properties":{}}}}],
            "temperature":0.2,
            "max_tokens":100,
            "stop":["END"]
        })
    );
}

#[test]
fn thinking_resolution_matches_request_and_deployment_rules() {
    let mut request = request();
    request.reasoning_effort = Some(ReasoningEffortId::new("max"));
    let wire = value(
        serialize_request(
            &request,
            RequestDefaults {
                thinking: Some(ThinkingMode::Enabled),
                reasoning_effort: Some(ReasoningEffort::High),
            },
        )
        .unwrap(),
    );
    assert_eq!(wire["thinking"], json!({"type":"enabled"}));
    assert_eq!(wire["reasoning_effort"], "max");

    request.reasoning_effort = Some(ReasoningEffortId::new("off"));
    let wire = value(
        serialize_request(
            &request,
            RequestDefaults {
                thinking: Some(ThinkingMode::Enabled),
                reasoning_effort: Some(ReasoningEffort::Max),
            },
        )
        .unwrap(),
    );
    assert_eq!(wire["thinking"], json!({"type":"disabled"}));
    assert!(wire.get("reasoning_effort").is_none());

    request.reasoning_effort = Some(ReasoningEffortId::new("high"));
    let error = serialize_request(
        &request,
        RequestDefaults {
            thinking: Some(ThinkingMode::Disabled),
            reasoning_effort: None,
        },
    )
    .unwrap_err();
    assert_eq!(error.code(), "UNSUPPORTED_REASONING_EFFORT");

    request.reasoning_effort = Some(ReasoningEffortId::new("medium"));
    assert_eq!(
        serialize_request(&request, RequestDefaults::default())
            .unwrap_err()
            .code(),
        "UNSUPPORTED_REASONING_EFFORT"
    );
}

#[test]
fn session_title_forces_thinking_off_and_empty_tools_are_omitted() {
    let mut request = request();
    request.reasoning_effort = Some(ReasoningEffortId::new("max"));
    request.purpose = Some(LlmRequestPurpose::SessionTitle);
    request.tools = Some(vec![]);
    let wire = value(
        serialize_request(
            &request,
            RequestDefaults {
                thinking: Some(ThinkingMode::Enabled),
                reasoning_effort: Some(ReasoningEffort::Max),
            },
        )
        .unwrap(),
    );
    assert_eq!(wire["thinking"], json!({"type":"disabled"}));
    assert!(wire.get("reasoning_effort").is_none());
    assert!(wire.get("tools").is_none());
}
