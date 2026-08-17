//! Best-effort bounding of oversized plain-text tool projections.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventOptions, Fiber, fiber::EffectHandle};
use seekdeep_core::session::SessionId;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{CallId, ContentBlock};
use seekdeep_spill::{SPILL_STORE, SaveTextSpill, SpillOwner, SpillRef, SpillSource};
use seekdeep_tools::{
    CodeDispatchLog, PostToolDecision, ToolExecution, ToolExecutionResult, ToolRuntime,
};
use seekdeep_util::output_retention::{
    Omitted, RetentionUnit, TextRetainer, TextRetentionStrategy, describe_omitted,
};
use serde::{Deserialize, Serialize};

/// Cordis plugin name retained by loader-facing diagnostics.
pub const NAME: &str = "spill-policy";
/// Required service name retained by loader-facing metadata.
pub const INJECT: &[&str] = &["tools"];

/// Spill policy configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpillPolicyConfig {
    /// Maximum inline UTF-8 bytes; omission disables the policy entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_inline_bytes: Option<f64>,
}

/// Installs both prepended spill-policy waterfalls transactionally.
///
/// An omitted cap is a true no-op and returns `None`. An enabled policy owns
/// both the model-facing post-execute arm and the Code Mode durable-log arm in
/// one disposable child fiber.
///
/// # Errors
///
/// Returns for an invalid cap, inactive context, registration failure, or
/// cleanup failure while rolling back a partial installation.
pub async fn install(
    ctx: &Context,
    tools: &Arc<ToolRuntime>,
    config: SpillPolicyConfig,
) -> anyhow::Result<Option<EffectHandle>> {
    let Some(cap) = config.max_inline_bytes else {
        return Ok(None);
    };
    let cap = validate_cap(cap)?;
    let fiber = Fiber::active_child(NAME);
    let child = ctx.with_fiber(fiber.clone());
    let policy_context = child.clone();
    let install_result = (|| {
        tools.on_post_execute(
            &child,
            move |execution, result, next| {
                let context = policy_context.clone();
                async move {
                    let decision = next.run().await?;
                    shape_post_decision(&context, cap, &execution, &result, decision).await
                }
            },
            EventOptions {
                prepend: true,
                global: false,
            },
        )?;

        let policy_context = child.clone();
        tools.on_code_dispatch_log(
            &child,
            move |dispatch, next| {
                let context = policy_context.clone();
                async move {
                    let downstream = next.run().await?;
                    shape_dispatch_content(&context, cap, &dispatch, downstream).await
                }
            },
            EventOptions {
                prepend: true,
                global: false,
            },
        )?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(error) = install_result {
        return match fiber.dispose().await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow::anyhow!("{error:#}: cleanup failed: {cleanup:#}")),
        };
    }

    let cleanup_fiber = fiber.clone();
    let effect = EffectHandle::new(NAME, move || {
        Box::pin(async move { cleanup_fiber.dispose().await })
    });
    if let Err(error) = ctx.own(effect.clone()) {
        return match fiber.dispose().await {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!("{error}: cleanup failed: {cleanup:#}")),
        };
    }
    Ok(Some(effect))
}

fn validate_cap(value: f64) -> anyhow::Result<usize> {
    anyhow::ensure!(
        value.is_finite() && value >= 0.0 && value.fract() == 0.0,
        "spill-policy: maxInlineBytes must be a non-negative integer (got {})",
        format_number(value)
    );
    // JavaScript accepts integral Numbers beyond the host address space. Such
    // a cap is indistinguishable from `usize::MAX` for every realizable string.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as usize)
}

fn format_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        value.to_string()
    }
}

async fn shape_post_decision(
    ctx: &Context,
    cap: usize,
    execution: &ToolExecution,
    result: &ToolExecutionResult,
    decision: PostToolDecision,
) -> anyhow::Result<PostToolDecision> {
    let (content, additional_contexts) = match &decision {
        PostToolDecision::Accept {
            content,
            additional_contexts,
        } if execution.parent.is_none() && execution.name != "read" => (
            content.as_deref().unwrap_or_else(|| result.content()),
            additional_contexts.clone(),
        ),
        PostToolDecision::Accept { .. }
        | PostToolDecision::ReplaceValue { .. }
        | PostToolDecision::Block { .. } => return Ok(decision),
    };
    let Some(text) = flatten_plain_text(content) else {
        return Ok(decision);
    };
    let total_bytes = text.len();
    if total_bytes <= cap {
        return Ok(decision);
    }
    let replacement = spill_replacement(
        ctx,
        cap,
        &text,
        total_bytes,
        owner_session_id(execution),
        &execution.name,
        &execution.call_id,
        "result",
    )
    .await;
    Ok(
        replacement.map_or(decision, |text| PostToolDecision::Accept {
            content: Some(vec![ContentBlock::Text { text }]),
            additional_contexts,
        }),
    )
}

async fn shape_dispatch_content(
    ctx: &Context,
    cap: usize,
    dispatch: &CodeDispatchLog,
    content: Vec<ContentBlock>,
) -> anyhow::Result<Vec<ContentBlock>> {
    let Some(text) = flatten_plain_text(&content) else {
        return Ok(content);
    };
    let total_bytes = text.len();
    if total_bytes <= cap {
        return Ok(content);
    }
    let replacement = spill_replacement(
        ctx,
        cap,
        &text,
        total_bytes,
        owner_session_id(&dispatch.execution),
        &dispatch.name,
        &dispatch.sub_call_id,
        "dispatch",
    )
    .await;
    Ok(replacement.map_or(content, |text| vec![ContentBlock::Text { text }]))
}

fn flatten_plain_text(content: &[ContentBlock]) -> Option<String> {
    let mut text = String::new();
    for block in content {
        let ContentBlock::Text { text: block_text } = block else {
            return None;
        };
        text.push_str(block_text);
    }
    Some(text)
}

fn owner_session_id(execution: &ToolExecution) -> Option<&SessionId> {
    execution.agent_session.as_ref().map(|session| session.id())
}

#[allow(clippy::too_many_arguments)]
async fn spill_replacement(
    context: &Context,
    cap: usize,
    text: &str,
    total_bytes: usize,
    session_id: Option<&SessionId>,
    tool_name: &str,
    call_id: &CallId,
    label: &'static str,
) -> Option<String> {
    let Some(session_id) = session_id else {
        tracing::warn!(
            tool = tool_name,
            label,
            "spill-policy: no session owner; keeping the inline content"
        );
        return None;
    };
    let Some(store) = context.get(SPILL_STORE) else {
        tracing::warn!(
            "spill-policy: no ctx.spillStore backend loaded; keeping the inline content"
        );
        return None;
    };
    let saved = store
        .save_text(SaveTextSpill {
            owner: SpillOwner {
                session_id: session_id.clone(),
            },
            source: SpillSource {
                tool_name: tool_name.to_owned(),
                call_id: call_id.clone(),
                label: label.to_owned(),
            },
            suggested_name: format!("{tool_name}.txt"),
            content: text.to_owned(),
        })
        .await;
    let reference = match saved {
        Ok(reference) => reference,
        Err(error) => {
            tracing::warn!(
                tool = tool_name,
                error = %error,
                "spill-policy: saveText failed; keeping the inline content"
            );
            return None;
        }
    };

    let reserve = spill_notice(Omitted::Exact(total_bytes), &reference)
        .len()
        .saturating_add(2);
    let preview_budget = cap.saturating_sub(reserve);
    let head_bytes = preview_budget.div_ceil(2);
    let tail_bytes = preview_budget / 2;
    let mut retainer = TextRetainer::new(TextRetentionStrategy::HeadTail {
        head_bytes,
        tail_bytes,
    });
    retainer.push_str(text);
    let snapshot = retainer.finish();
    let notice = spill_notice(snapshot.omitted_bytes, &reference);
    let replacement = if snapshot.text.is_empty() {
        notice
    } else {
        format!("{}\n\n{notice}", snapshot.text)
    };
    if replacement.len() > cap {
        tracing::warn!(
            tool = tool_name,
            "spill-policy: spill notice exceeds maxInlineBytes; keeping the inline content"
        );
        None
    } else {
        Some(replacement)
    }
}

fn spill_notice(omitted: Omitted, reference: &SpillRef) -> String {
    let omission = describe_omitted(omitted, RetentionUnit::Bytes);
    format!(
        "({omission} Full formatted result stored at: {}. {})",
        reference.locator, reference.retrieval_hint
    )
}

/// Registers the policy package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-spill-policy", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use async_trait::async_trait;
    use parking_lot::Mutex;
    use seekdeep_core::session::{Session, SessionId};
    use seekdeep_invariants::InvariantConfig;
    use seekdeep_llm::{AbortSignal, MessageSource, UserMessage};
    use seekdeep_spill::{SpillBackend, SpillLocator, SpillStore};
    use seekdeep_tools::{
        CodeDispatchLog, PostToolDecision, ScheduledToolPreparation, ToolDefinition,
        ToolExecutionInput, ToolOutputDefinition, ToolRuntimeConfig, assert_supported_json_schema,
    };
    use serde_json::{Map, Value, json};

    use super::*;

    #[derive(Default)]
    struct StubBackend {
        saves: Mutex<Vec<SaveTextSpill>>,
        fail: AtomicBool,
    }

    #[async_trait]
    impl SpillBackend for StubBackend {
        async fn save_text(&self, input: SaveTextSpill) -> anyhow::Result<SpillRef> {
            if self.fail.load(Ordering::Acquire) {
                anyhow::bail!("disk full");
            }
            self.saves.lock().push(input.clone());
            Ok(SpillRef {
                locator: SpillLocator::new(format!("/spill/{}", input.suggested_name)),
                bytes: input.content.len() as u64,
                retrieval_hint: "Use the stub retrieval path.".to_owned(),
            })
        }
    }

    fn blocks(text: &str) -> Vec<ContentBlock> {
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }]
    }

    fn text_of(content: &[ContentBlock]) -> String {
        content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn tool(name: &str, content: Vec<ContentBlock>) -> ToolDefinition {
        ToolDefinition::new(
            name,
            name,
            Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]),
            ToolOutputDefinition::new(
                Arc::new(assert_supported_json_schema(json!({ "type": "string" })).unwrap()),
                Arc::new(move |_, _| Ok(content.clone())),
            ),
            Arc::new(|_, _| Box::pin(async { Ok(json!("canonical")) })),
        )
    }

    fn value_tool(name: &str, initial: &str) -> ToolDefinition {
        let initial = initial.to_owned();
        ToolDefinition::new(
            name,
            name,
            Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]),
            ToolOutputDefinition::new(
                Arc::new(assert_supported_json_schema(json!({ "type": "string" })).unwrap()),
                Arc::new(|_, value| Ok(blocks(value.as_str().expect("validated string output")))),
            ),
            Arc::new(move |_, _| {
                let value = initial.clone();
                Box::pin(async move { Ok(Value::String(value)) })
            }),
        )
    }

    fn session(id: &str) -> Arc<Session> {
        Session::create(&SessionId::new(id), None, None).unwrap()
    }

    fn input(name: &str, owner: Option<Arc<Session>>) -> ToolExecutionInput {
        let mut input = ToolExecutionInput::new(
            CallId::new(format!("call-{name}")),
            name,
            json!({}),
            AbortSignal::default(),
        );
        input.agent_session = owner;
        input
    }

    fn harness(with_store: bool) -> (Context, Arc<ToolRuntime>, Arc<StubBackend>) {
        let context = Context::new();
        let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
        let backend = Arc::new(StubBackend::default());
        if with_store {
            Arc::new(SpillStore::new(backend.clone()))
                .provide(&context)
                .unwrap();
        }
        (context, tools, backend)
    }

    async fn outer_execution(
        context: &Context,
        tools: &Arc<ToolRuntime>,
        owner: Arc<Session>,
    ) -> ToolExecution {
        tools
            .register(context, tool("outer", blocks("outer")))
            .unwrap();
        match tools.prepare_scheduled(input("outer", Some(owner))).await {
            ScheduledToolPreparation::Dispatch { execution } => execution,
            _ => panic!("outer execution must be dispatchable"),
        }
    }

    #[tokio::test]
    async fn omitted_cap_is_a_true_noop_and_bad_caps_fail_at_load() {
        let (context, tools, backend) = harness(true);
        assert!(
            install(&context, &tools, SpillPolicyConfig::default())
                .await
                .unwrap()
                .is_none()
        );
        tools
            .register(&context, tool("big", blocks(&"x".repeat(1_000))))
            .unwrap();
        let result = tools.execute(input("big", Some(session("s1")))).await;
        assert_eq!(text_of(result.content()), "x".repeat(1_000));
        assert!(backend.saves.lock().is_empty());

        for invalid in [-1.0, 1.5, f64::NAN, f64::INFINITY] {
            let error = install(
                &context,
                &tools,
                SpillPolicyConfig {
                    max_inline_bytes: Some(invalid),
                },
            )
            .await
            .unwrap_err();
            assert!(format!("{error:#}").contains("maxInlineBytes must be a non-negative integer"));
        }
    }

    #[tokio::test]
    async fn oversized_text_saves_verbatim_and_replaces_within_the_same_utf8_cap() {
        let (context, tools, backend) = harness(true);
        let _policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(200.0),
            },
        )
        .await
        .unwrap();
        let body = format!("{}{}", "HÉAD".repeat(200), "TAIL".repeat(200));
        tools
            .register(&context, tool("big", blocks(&body)))
            .unwrap();
        let result = tools.execute(input("big", Some(session("s1")))).await;
        assert!(!result.is_error());
        let saves = backend.saves.lock();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].content, body);
        assert_eq!(saves[0].owner.session_id.as_str(), "s1");
        assert_eq!(saves[0].source.tool_name, "big");
        assert_eq!(saves[0].source.call_id.as_str(), "call-big");
        assert_eq!(saves[0].source.label, "result");
        assert_eq!(saves[0].suggested_name, "big.txt");
        let replacement = text_of(result.content());
        assert!(replacement.len() <= 200);
        assert!(replacement.starts_with("HÉAD"));
        assert!(replacement.contains("Omitted"));
        assert!(replacement.contains("Full formatted result stored at: /spill/big.txt"));
        assert!(replacement.contains("Use the stub retrieval path."));
        assert!(std::str::from_utf8(replacement.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn small_mixed_read_and_nested_results_pass_unchanged() {
        let (context, tools, backend) = harness(true);
        let _policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(10.0),
            },
        )
        .await
        .unwrap();
        tools
            .register(&context, tool("small", blocks("tiny")))
            .unwrap();
        tools
            .register(
                &context,
                tool(
                    "mixed",
                    vec![
                        ContentBlock::Text {
                            text: "x".repeat(100),
                        },
                        ContentBlock::Reasoning {
                            text: "why".to_owned(),
                        },
                    ],
                ),
            )
            .unwrap();
        tools
            .register(&context, tool("read", blocks(&"r".repeat(100))))
            .unwrap();
        tools
            .register(&context, tool("nested", blocks(&"n".repeat(100))))
            .unwrap();
        let owner = session("s1");
        assert_eq!(
            text_of(
                tools
                    .execute(input("small", Some(owner.clone())))
                    .await
                    .content()
            ),
            "tiny"
        );
        assert_eq!(
            tools
                .execute(input("mixed", Some(owner.clone())))
                .await
                .content()
                .len(),
            2
        );
        assert_eq!(
            text_of(
                tools
                    .execute(input("read", Some(owner.clone())))
                    .await
                    .content()
            ),
            "r".repeat(100)
        );
        let outer = outer_execution(&context, &tools, owner.clone()).await;
        let mut nested = input("nested", Some(owner));
        nested.parent = Some(outer.token);
        assert_eq!(
            text_of(tools.execute(nested).await.content()),
            "n".repeat(100)
        );
        assert!(backend.saves.lock().is_empty());
    }

    #[tokio::test]
    async fn tiny_cap_never_emits_an_over_cap_notice_but_may_leave_an_orphan() {
        let (context, tools, backend) = harness(true);
        let _policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(4.0),
            },
        )
        .await
        .unwrap();
        tools
            .register(&context, tool("big", blocks("xxxxx")))
            .unwrap();
        let result = tools.execute(input("big", Some(session("s1")))).await;
        assert_eq!(text_of(result.content()), "xxxxx");
        assert_eq!(backend.saves.lock().len(), 1);
    }

    #[tokio::test]
    async fn missing_owner_backend_and_save_failure_are_best_effort() {
        let (context, tools, backend) = harness(true);
        let _policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(100.0),
            },
        )
        .await
        .unwrap();
        tools
            .register(&context, tool("ownerless", blocks(&"o".repeat(500))))
            .unwrap();
        assert_eq!(
            text_of(tools.execute(input("ownerless", None)).await.content()),
            "o".repeat(500)
        );
        backend.fail.store(true, Ordering::Release);
        tools
            .register(&context, tool("failed", blocks(&"f".repeat(500))))
            .unwrap();
        let failed = tools.execute(input("failed", Some(session("s1")))).await;
        assert_eq!(text_of(failed.content()), "f".repeat(500));
        assert!(!failed.is_error());

        let (without_context, without_tools, _) = harness(false);
        let _policy = install(
            &without_context,
            &without_tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(100.0),
            },
        )
        .await
        .unwrap();
        without_tools
            .register(&without_context, tool("missing", blocks(&"m".repeat(500))))
            .unwrap();
        let missing = without_tools
            .execute(input("missing", Some(session("s2"))))
            .await;
        assert_eq!(text_of(missing.content()), "m".repeat(500));
        assert!(!missing.is_error());
    }

    #[tokio::test]
    async fn prepended_policy_bounds_downstream_content_and_preserves_contexts() {
        let (context, tools, backend) = harness(true);
        let note = UserMessage::new(blocks("note"), MessageSource::plugin("test-spill-policy"));
        let expected_note = note.clone();
        tools
            .on_post_execute(
                &context,
                move |_, _, _| {
                    let note = note.clone();
                    async move {
                        Ok(PostToolDecision::Accept {
                            content: Some(blocks(&"z".repeat(500))),
                            additional_contexts: vec![note],
                        })
                    }
                },
                EventOptions::default(),
            )
            .unwrap();
        let _policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(200.0),
            },
        )
        .await
        .unwrap();
        tools
            .register(&context, tool("small", blocks("tiny")))
            .unwrap();
        let result = tools.execute(input("small", Some(session("s1")))).await;
        assert!(text_of(result.content()).contains("Full formatted result stored at"));
        assert_eq!(result.additional_contexts(), &[expected_note]);
        assert_eq!(backend.saves.lock()[0].content, "z".repeat(500));
    }

    #[tokio::test]
    async fn value_replacements_and_blocks_pass_through_without_spilling() {
        let (context, tools, backend) = harness(true);
        let _policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(10.0),
            },
        )
        .await
        .unwrap();
        tools
            .on_post_execute(
                &context,
                |execution, _, _| async move {
                    if execution.name == "value" {
                        Ok(PostToolDecision::ReplaceValue {
                            value: Value::String("v".repeat(500)),
                            additional_contexts: Vec::new(),
                        })
                    } else {
                        Ok(PostToolDecision::Block {
                            feedback: blocks(&"b".repeat(500)),
                            additional_contexts: Vec::new(),
                        })
                    }
                },
                EventOptions::default(),
            )
            .unwrap();
        tools
            .register(&context, value_tool("value", "tiny"))
            .unwrap();
        tools
            .register(&context, value_tool("blocked", "tiny"))
            .unwrap();
        let owner = session("s1");
        let value = tools.execute(input("value", Some(owner.clone()))).await;
        assert!(!value.is_error());
        assert_eq!(text_of(value.content()), "v".repeat(500));
        let blocked = tools.execute(input("blocked", Some(owner))).await;
        assert!(blocked.is_error());
        assert_eq!(text_of(blocked.content()), "b".repeat(500));
        assert!(backend.saves.lock().is_empty());
    }

    #[tokio::test]
    async fn ordinary_failed_results_are_also_spilled() {
        let (context, tools, backend) = harness(true);
        let _policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(200.0),
            },
        )
        .await
        .unwrap();
        tools
            .register(
                &context,
                ToolDefinition::new(
                    "fail",
                    "fail",
                    Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]),
                    ToolOutputDefinition::new(
                        Arc::new(
                            assert_supported_json_schema(json!({ "type": "string" })).unwrap(),
                        ),
                        Arc::new(|_, _| Ok(blocks("unused"))),
                    ),
                    Arc::new(|_, _| {
                        Box::pin(async { anyhow::bail!("failure {}", "X".repeat(1_000)) })
                    }),
                ),
            )
            .unwrap();
        let result = tools.execute(input("fail", Some(session("s1")))).await;
        assert!(result.is_error());
        assert!(text_of(result.content()).contains("Full formatted result stored at"));
        assert!(backend.saves.lock()[0].content.contains("failure"));
    }

    #[tokio::test]
    async fn dispatch_log_arm_bounds_read_and_preserves_program_independence() {
        let (context, tools, backend) = harness(true);
        let _policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(200.0),
            },
        )
        .await
        .unwrap();
        let execution = outer_execution(&context, &tools, session("dispatch-owner")).await;
        let full = "H".repeat(2_000);
        let dispatch = CodeDispatchLog {
            execution,
            agent: None,
            sub_call_id: CallId::new("parent:code:1"),
            name: "read".to_owned(),
            is_error: false,
            content: blocks(&full),
        };
        let logged = tools.shape_code_dispatch_log(&dispatch).await;
        let logged_text = text_of(&logged);
        assert!(logged_text.len() <= 200);
        assert!(logged_text.contains("Full formatted result stored at: /spill/read.txt"));
        assert_eq!(dispatch.content, blocks(&full));
        let saves = backend.saves.lock();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].content, full);
        assert_eq!(saves[0].source.label, "dispatch");
        assert_eq!(saves[0].source.call_id.as_str(), "parent:code:1");
    }

    #[tokio::test]
    async fn dispatch_log_small_mixed_and_failed_shapers_fall_back_exactly() {
        let (context, tools, backend) = harness(true);
        let _policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(20.0),
            },
        )
        .await
        .unwrap();
        let execution = outer_execution(&context, &tools, session("s1")).await;
        let make = |name: &str, content: Vec<ContentBlock>| CodeDispatchLog {
            execution: execution.clone(),
            agent: None,
            sub_call_id: CallId::new(format!("sub-{name}")),
            name: name.to_owned(),
            is_error: false,
            content,
        };
        let small = make("small", blocks("tiny"));
        assert_eq!(tools.shape_code_dispatch_log(&small).await, small.content);
        let mixed = make(
            "mixed",
            vec![
                ContentBlock::Text {
                    text: "x".repeat(100),
                },
                ContentBlock::Reasoning {
                    text: "why".to_owned(),
                },
            ],
        );
        assert_eq!(tools.shape_code_dispatch_log(&mixed).await, mixed.content);
        assert!(backend.saves.lock().is_empty());

        tools
            .on_code_dispatch_log(
                &context,
                |_, _| async { anyhow::bail!("broken shaper") },
                EventOptions::default(),
            )
            .unwrap();
        let oversized = make("oversized", blocks(&"q".repeat(500)));
        assert_eq!(
            tools.shape_code_dispatch_log(&oversized).await,
            oversized.content
        );
        assert!(backend.saves.lock().is_empty());
    }

    #[tokio::test]
    async fn disposal_removes_both_transformers() {
        let (context, tools, backend) = harness(true);
        let policy = install(
            &context,
            &tools,
            SpillPolicyConfig {
                max_inline_bytes: Some(200.0),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let body = "x".repeat(1_000);
        tools
            .register(&context, tool("big", blocks(&body)))
            .unwrap();
        let owner = session("s1");
        assert!(
            text_of(
                tools
                    .execute(input("big", Some(owner.clone())))
                    .await
                    .content()
            )
            .contains("Full formatted result stored at")
        );
        policy.dispose().await.unwrap();
        assert_eq!(
            text_of(tools.execute(input("big", Some(owner))).await.content()),
            body
        );
        assert_eq!(backend.saves.lock().len(), 1);
    }

    #[test]
    fn public_shape_and_invariant_use_renamed_package_identity() {
        assert_eq!(NAME, "spill-policy");
        assert_eq!(INJECT, &["tools"]);
        assert_eq!(
            serde_json::to_value(SpillPolicyConfig {
                max_inline_bytes: Some(12.0),
            })
            .unwrap(),
            json!({ "maxInlineBytes": 12.0 })
        );
        let context = Context::new();
        let registry =
            Arc::new(InvariantRegistry::new(&context, &InvariantConfig::default()).unwrap());
        let _registration = register_invariant(&registry).unwrap();
        assert!(registry.is_registered("seekdeep-spill-policy"));
    }
}
