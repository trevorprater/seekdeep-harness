//! Behavioral parity with the source repeat-tool reminder oracle.

use std::{
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
};

use seekdeep_agent::{
    Agent, AgentEvents, AgentOptions, Inbox, NoopInboxNotifications, PreStepDecision,
};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, EventOptions, PluginFiber};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, MessageSource, UserMessage};
use seekdeep_repeat_tool_reminder::RepeatToolReminderConfig;
use seekdeep_scope::ScopeKey;
use seekdeep_tools::{
    ContentToolFixtureOptions, PostToolDecision, PreToolDecision, ToolExecutionInput,
    ToolExecutionResult, ToolRuntime, ToolRuntimeConfig, define_content_tool_fixture,
};
use serde_json::{Value, json};

struct Harness {
    root: Context,
    tools: Arc<ToolRuntime>,
    _guard: Arc<PluginFiber>,
    calls: AtomicUsize,
}

impl Harness {
    async fn new(config: RepeatToolReminderConfig) -> Self {
        let root = Context::new();
        let tools = ToolRuntime::new(root.clone(), ToolRuntimeConfig::default()).expect("tools");
        let guard = root
            .plugin(
                seekdeep_repeat_tool_reminder::plugin(),
                serde_json::to_value(config).expect("config"),
            )
            .expect("guard mount");
        guard.await_settled().await.expect("guard startup");
        for name in ["probe", "other"] {
            tools
                .register(
                    &root,
                    define_content_tool_fixture(ContentToolFixtureOptions::<Value>::new(
                        name,
                        name,
                        json!({}),
                        Arc::new(|_, _| {
                            Box::pin(async {
                                Ok(vec![ContentBlock::Text {
                                    text: "ok".to_owned(),
                                }])
                            })
                        }),
                    ))
                    .expect("fixture"),
                )
                .expect("register tool");
        }
        Self {
            root,
            tools,
            _guard: guard,
            calls: AtomicUsize::new(0),
        }
    }

    fn agent(&self, id: &str) -> Arc<Agent> {
        let session = Session::create(&SessionId::new(id), None, None).expect("session");
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
        Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox,
            self.root.clone(),
            ScopeKey::new(),
        ))
    }

    async fn execute(
        &self,
        agent: Option<&Arc<Agent>>,
        name: &str,
        arguments: Value,
    ) -> ToolExecutionResult {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let mut input = ToolExecutionInput::new(
            CallId::new(format!("c{call}")),
            name,
            arguments,
            AbortSignal::default(),
        );
        if let Some(agent) = agent {
            input = input.with_agent(agent.clone());
        }
        self.tools.execute(input).await
    }

    async fn user_interjection(&self, agent: &Arc<Agent>, text: &str) {
        let message = user(text);
        AgentEvents::new(self.root.clone(), agent.clone())
            .waterfall(
                "agent/pre-step",
                AgentPreStepEvent {
                    messages: vec![message.clone()],
                    turn: 2,
                    step: 1,
                    signal: AbortSignal::default(),
                },
                move || async move {
                    Ok(PreStepDecision::Enter {
                        messages: vec![message],
                    })
                },
            )
            .await
            .expect("pre-step");
    }
}

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

fn plugin_context(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::plugin("test"),
    )
}

fn context_text(message: &UserMessage) -> &str {
    match message.content().first() {
        Some(ContentBlock::Text { text }) => text,
        other => panic!("expected text context, got {other:?}"),
    }
}

fn reminder_texts(result: &ToolExecutionResult) -> Vec<String> {
    result
        .additional_contexts()
        .iter()
        .filter(|message| {
            message.source().kind == "plugin"
                && message
                    .source()
                    .fields
                    .get("plugin")
                    .and_then(Value::as_str)
                    == Some("repeat-tool-reminder")
        })
        .map(|message| context_text(message).to_owned())
        .collect()
}

fn config(thresholds: &[f64]) -> RepeatToolReminderConfig {
    RepeatToolReminderConfig {
        thresholds: thresholds.to_vec(),
        ..RepeatToolReminderConfig::default()
    }
}

#[tokio::test]
async fn reminds_gently_at_first_default_threshold_and_in_detail_at_second() {
    let harness = Harness::new(RepeatToolReminderConfig::default()).await;
    let agent = harness.agent("a1");
    let mut reminders = Vec::new();
    for _ in 0..5 {
        reminders.extend(reminder_texts(
            &harness
                .execute(Some(&agent), "probe", json!({"q": "same"}))
                .await,
        ));
    }
    assert_eq!(reminders.len(), 2);
    assert!(reminders[0].contains("repeating the exact same tool call"));
    assert!(reminders[1].contains("consecutive_calls: 5"));
    assert!(reminders[1].contains("- tool: probe"));
    assert!(reminders[1].contains(r#"{"q":"same"}"#));
}

#[tokio::test]
async fn gentle_text_keys_to_sorted_first_threshold() {
    let harness = Harness::new(config(&[4.0, 2.0])).await;
    let agent = harness.agent("a1");
    let mut reminders = Vec::new();
    for _ in 0..4 {
        reminders.extend(reminder_texts(
            &harness.execute(Some(&agent), "probe", json!({})).await,
        ));
    }
    assert_eq!(reminders.len(), 2);
    assert!(reminders[0].contains("repeating the exact same tool call"));
    assert!(reminders[1].contains("consecutive_calls: 4"));
}

#[tokio::test]
async fn detailed_argument_preview_is_bounded_without_changing_detection_key() {
    let harness = Harness::new(RepeatToolReminderConfig {
        thresholds: vec![2.0, 3.0],
        arguments_preview_chars: 24.0,
        ..RepeatToolReminderConfig::default()
    })
    .await;
    let agent = harness.agent("a1");
    let payload = "x".repeat(400);
    let mut reminders = Vec::new();
    for _ in 0..3 {
        reminders.extend(reminder_texts(
            &harness
                .execute(Some(&agent), "probe", json!({"body": payload}))
                .await,
        ));
    }
    assert_eq!(reminders.len(), 2);
    let detailed = &reminders[1];
    assert!(detailed.contains(r#"- arguments: {"body":"xxxxxxxxxxxxxx"#));
    assert!(detailed.contains("… (+387 more chars)"));
    assert!(!detailed.contains(&"x".repeat(400)));
}

#[tokio::test]
async fn different_tracked_call_resets_chain() {
    let harness = Harness::new(RepeatToolReminderConfig::default()).await;
    let agent = harness.agent("a1");
    for (name, args) in [
        ("probe", json!({"q": 1})),
        ("probe", json!({"q": 1})),
        ("other", json!({})),
        ("probe", json!({"q": 1})),
        ("probe", json!({"q": 1})),
    ] {
        assert!(reminder_texts(&harness.execute(Some(&agent), name, args).await).is_empty());
    }
    assert_eq!(
        reminder_texts(
            &harness
                .execute(Some(&agent), "probe", json!({"q": 1}))
                .await
        )
        .len(),
        1
    );
}

#[tokio::test]
async fn excluded_calls_are_transparent() {
    let harness = Harness::new(RepeatToolReminderConfig {
        exclude: vec!["other".to_owned()],
        ..RepeatToolReminderConfig::default()
    })
    .await;
    let agent = harness.agent("a1");
    for name in ["probe", "other", "probe", "other"] {
        assert!(
            reminder_texts(&harness.execute(Some(&agent), name, json!({"q": 1})).await).is_empty()
        );
    }
    assert_eq!(
        reminder_texts(
            &harness
                .execute(Some(&agent), "probe", json!({"q": 1}))
                .await
        )
        .len(),
        1
    );
}

#[tokio::test]
async fn include_wildcards_track_only_matching_tools() {
    let harness = Harness::new(RepeatToolReminderConfig {
        include: vec!["pro*".to_owned()],
        ..RepeatToolReminderConfig::default()
    })
    .await;
    let agent = harness.agent("a1");
    for _ in 0..3 {
        assert!(
            reminder_texts(&harness.execute(Some(&agent), "other", json!({})).await).is_empty()
        );
    }
    for call in 1..=3 {
        let result = harness.execute(Some(&agent), "probe", json!({})).await;
        assert_eq!(reminder_texts(&result).len(), usize::from(call == 3));
    }
}

#[tokio::test]
async fn wildcard_patterns_escape_regex_metacharacters() {
    let harness = Harness::new(RepeatToolReminderConfig {
        exclude: vec!["pr.be".to_owned()],
        ..RepeatToolReminderConfig::default()
    })
    .await;
    let agent = harness.agent("a1");
    for _ in 0..2 {
        assert!(
            reminder_texts(&harness.execute(Some(&agent), "probe", json!({})).await).is_empty()
        );
    }
    assert_eq!(
        reminder_texts(&harness.execute(Some(&agent), "probe", json!({})).await).len(),
        1
    );
}

#[tokio::test]
async fn canonicalization_ignores_property_order_deeply() {
    let harness = Harness::new(RepeatToolReminderConfig::default()).await;
    let agent = harness.agent("a1");
    let values = [
        json!({"a": 1, "nested": {"x": [1, 2], "y": null}}),
        json!({"nested": {"y": null, "x": [1, 2]}, "a": 1}),
        json!({"a": 1, "nested": {"x": [1, 2], "y": null}}),
    ];
    for (index, value) in values.into_iter().enumerate() {
        let result = harness.execute(Some(&agent), "probe", value).await;
        assert_eq!(reminder_texts(&result).len(), usize::from(index == 2));
    }
}

#[tokio::test]
async fn chains_are_keyed_per_exact_agent() {
    let harness = Harness::new(RepeatToolReminderConfig::default()).await;
    let first = harness.agent("a");
    let second = harness.agent("b");
    for _ in 0..2 {
        assert!(
            reminder_texts(
                &harness
                    .execute(Some(&first), "probe", json!({"q": 1}))
                    .await
            )
            .is_empty()
        );
    }
    for call in 1..=3 {
        let result = harness
            .execute(Some(&second), "probe", json!({"q": 1}))
            .await;
        assert_eq!(reminder_texts(&result).len(), usize::from(call == 3));
    }
}

#[tokio::test]
async fn new_user_prompt_resets_chain() {
    let harness = Harness::new(RepeatToolReminderConfig::default()).await;
    let agent = harness.agent("a1");
    for _ in 0..2 {
        assert!(
            reminder_texts(
                &harness
                    .execute(Some(&agent), "probe", json!({"q": 1}))
                    .await
            )
            .is_empty()
        );
    }
    harness.user_interjection(&agent, "again").await;
    assert!(
        reminder_texts(
            &harness
                .execute(Some(&agent), "probe", json!({"q": 1}))
                .await
        )
        .is_empty()
    );
}

#[tokio::test]
async fn dropped_agent_does_not_transfer_chain_to_same_session_id() {
    let harness = Harness::new(config(&[2.0])).await;
    let first = harness.agent("reused");
    assert!(
        reminder_texts(
            &harness
                .execute(Some(&first), "probe", json!({"q": 1}))
                .await
        )
        .is_empty()
    );
    drop(first);
    let second = harness.agent("reused");
    assert!(
        reminder_texts(
            &harness
                .execute(Some(&second), "probe", json!({"q": 1}))
                .await
        )
        .is_empty()
    );
}

#[tokio::test]
async fn denied_calls_still_advance_chain() {
    let harness = Harness::new(config(&[2.0])).await;
    harness
        .tools
        .on_pre_execute(
            &harness.root,
            |_, _| async {
                Ok(PreToolDecision::Deny {
                    reason: "sealed".to_owned(),
                })
            },
            EventOptions::default(),
        )
        .expect("deny");
    let agent = harness.agent("a1");
    let first = harness
        .execute(Some(&agent), "probe", json!({"q": 1}))
        .await;
    assert!(first.is_error());
    assert!(reminder_texts(&first).is_empty());
    let second = harness
        .execute(Some(&agent), "probe", json!({"q": 1}))
        .await;
    assert!(second.is_error());
    assert_eq!(reminder_texts(&second).len(), 1);
}

#[tokio::test]
async fn direct_execute_without_agent_is_ignored() {
    let harness = Harness::new(config(&[2.0])).await;
    let direct = harness.execute(None, "probe", json!({"q": 1})).await;
    assert!(!direct.is_error());
    let agent = harness.agent("a1");
    assert!(
        reminder_texts(
            &harness
                .execute(Some(&agent), "probe", json!({"q": 1}))
                .await
        )
        .is_empty()
    );
}

#[tokio::test]
async fn reminder_folds_onto_downstream_block_and_preserves_feedback() {
    let harness = Harness::new(config(&[2.0])).await;
    harness
        .tools
        .on_post_execute(
            &harness.root,
            |_, _, _| async {
                Ok(PostToolDecision::Block {
                    feedback: vec![ContentBlock::Text {
                        text: "nope".to_owned(),
                    }],
                    additional_contexts: vec![plugin_context("downstream-ctx")],
                })
            },
            EventOptions::default(),
        )
        .expect("downstream block");
    let agent = harness.agent("a1");
    let first = harness
        .execute(Some(&agent), "probe", json!({"q": 1}))
        .await;
    assert!(first.is_error());
    assert_eq!(first.additional_contexts().len(), 1);
    assert_eq!(
        context_text(&first.additional_contexts()[0]),
        "downstream-ctx"
    );
    let second = harness
        .execute(Some(&agent), "probe", json!({"q": 1}))
        .await;
    assert!(second.is_error());
    assert_eq!(second.additional_contexts().len(), 2);
    assert!(context_text(&second.additional_contexts()[0]).contains("repeating the exact"));
    assert_eq!(
        context_text(&second.additional_contexts()[1]),
        "downstream-ctx"
    );
    assert_eq!(
        second.content(),
        &[ContentBlock::Text {
            text: "nope".to_owned()
        }]
    );
}

#[tokio::test]
async fn reminder_preserves_downstream_canonical_value_replacement() {
    let harness = Harness::new(config(&[2.0])).await;
    harness
        .tools
        .on_post_execute(
            &harness.root,
            |_, _, _| async {
                Ok(PostToolDecision::ReplaceValue {
                    value: json!([{"type": "text", "text": "replaced"}]),
                    additional_contexts: Vec::new(),
                })
            },
            EventOptions::default(),
        )
        .expect("downstream replacement");
    let agent = harness.agent("a1");
    let first = harness
        .execute(Some(&agent), "probe", json!({"q": 1}))
        .await;
    assert_eq!(
        first.content(),
        &[ContentBlock::Text {
            text: "replaced".to_owned()
        }]
    );
    let second = harness
        .execute(Some(&agent), "probe", json!({"q": 1}))
        .await;
    assert_eq!(reminder_texts(&second).len(), 1);
    assert_eq!(
        second.content(),
        &[ContentBlock::Text {
            text: "replaced".to_owned()
        }]
    );
}

async fn mount_error(config: Value) -> String {
    let fiber = Context::new()
        .plugin(seekdeep_repeat_tool_reminder::plugin(), config)
        .expect("mount is scheduled");
    fiber
        .await_settled()
        .await
        .expect_err("configuration must fail")
        .to_string()
}

#[tokio::test]
async fn rejects_empty_thresholds() {
    assert!(
        mount_error(json!({"thresholds": []}))
            .await
            .contains("must not be empty")
    );
}

#[tokio::test]
async fn rejects_threshold_below_two() {
    assert!(
        mount_error(json!({"thresholds": [1, 3]}))
            .await
            .contains("integer >= 2")
    );
}

#[tokio::test]
async fn rejects_non_integer_threshold() {
    assert!(
        mount_error(json!({"thresholds": [2.5]}))
            .await
            .contains("integer >= 2")
    );
}

#[tokio::test]
async fn rejects_duplicate_thresholds() {
    assert!(
        mount_error(json!({"thresholds": [3, 3]}))
            .await
            .contains("duplicates")
    );
}

#[tokio::test]
async fn rejects_non_positive_or_fractional_preview_cap() {
    assert!(
        mount_error(json!({"argumentsPreviewChars": 0}))
            .await
            .contains("argumentsPreviewChars")
    );
    assert!(
        mount_error(json!({"argumentsPreviewChars": 12.5}))
            .await
            .contains("argumentsPreviewChars")
    );
}
