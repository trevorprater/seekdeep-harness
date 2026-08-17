//! Global registry, lifecycle, and waterfall parity specifications.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_llm::ToolSchema;
use seekdeep_scope::{ScopeKey, create_scope};
use seekdeep_system_prompt::{
    AssembleContext, AssembledContext, AssembledSection, PromptAssembly, PromptContext,
    PromptSection, PromptText, SYSTEM_PROMPT, SystemPrompt, SystemPromptConfig, ToolProviderResult,
    install, render_prompt,
};
use serde_json::{Map, json};

const IDENTITY: &str = "You are an AI agent powered by SeekDeep Harness.";

fn tool(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.to_owned(),
        description: format!("{name} tool"),
        parameters: Map::from_iter([("type".to_owned(), json!("object"))]),
    }
}

#[tokio::test]
async fn configures_identity_persona_dynamic_text_and_runtime_context_suppression() {
    assert_eq!(
        serde_json::from_value::<SystemPromptConfig>(json!({})).unwrap(),
        SystemPromptConfig::default()
    );
    assert_eq!(
        serde_json::from_value::<SystemPromptConfig>(json!({
            "includeHarnessIdentity": false,
            "includeRuntimeContext": false,
            "persona": "x",
            "toolOrder": ["<unlisted-tools>"]
        }))
        .unwrap(),
        SystemPromptConfig {
            include_harness_identity: false,
            include_runtime_context: false,
            persona: "x".to_owned(),
            tool_order: Some(vec!["<unlisted-tools>".to_owned()]),
        }
    );
    assert!(serde_json::from_value::<SystemPromptConfig>(json!({ "extra": true })).is_err());

    let mounted_context = Context::new();
    let mounted = install(&mounted_context, SystemPromptConfig::default()).unwrap();
    assert!(Arc::ptr_eq(
        &mounted,
        &mounted_context.get(SYSTEM_PROMPT).unwrap()
    ));

    let context = Context::new();
    let prompt = SystemPrompt::new(
        &context,
        SystemPromptConfig {
            persona: "Deployment persona".to_owned(),
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    assert_eq!(
        render_prompt(&prompt.assemble(AssembleContext::default()).await.unwrap()).unwrap(),
        format!("{IDENTITY}\n\nDeployment persona")
    );

    let context = Context::new();
    let prompt = SystemPrompt::new(
        &context,
        SystemPromptConfig {
            include_harness_identity: false,
            persona: "Complete deployment persona".to_owned(),
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    assert_eq!(
        render_prompt(&prompt.assemble(AssembleContext::default()).await.unwrap()).unwrap(),
        "Complete deployment persona"
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let context = Context::new();
    let prompt = SystemPrompt::new(
        &context,
        SystemPromptConfig {
            include_runtime_context: false,
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    prompt
        .prompt_context(
            &context,
            PromptContext::new(
                "never",
                0.0,
                PromptText::Dynamic(Arc::new(move |_| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Ok("secret".to_owned())
                })),
            ),
        )
        .unwrap();
    prompt
        .on_assemble(
            &context,
            |_, _, next| async move {
                let mut assembly = next.run().await?;
                assembly.contexts.push(AssembledContext {
                    name: "injected".to_owned(),
                    text: "leak".to_owned(),
                });
                Ok(assembly)
            },
            EventOptions::default(),
        )
        .unwrap();
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert!(assembly.contexts.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn assembles_ordered_dynamic_inputs_and_drops_owned_effects() {
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    let section_contexts = Arc::new(Mutex::new(Vec::new()));
    let seen = section_contexts.clone();
    let dynamic = prompt
        .section(
            &context,
            PromptSection::new(
                "dynamic",
                20.0,
                PromptText::Dynamic(Arc::new(move |assembly| {
                    seen.lock().push(assembly.fields["turn"].clone());
                    Ok(format!("turn {}", assembly.fields["turn"]))
                })),
            ),
        )
        .unwrap();
    prompt
        .section(&context, PromptSection::new("first", -50.0, "first"))
        .unwrap();
    prompt
        .prompt_context(&context, PromptContext::new("later", 20.0, "later"))
        .unwrap();
    prompt
        .prompt_context(&context, PromptContext::new("earlier", 10.0, "earlier"))
        .unwrap();
    prompt
        .tools(
            &context,
            Arc::new(|_| {
                Ok(ToolProviderResult {
                    schemas: vec![tool("z"), tool("a")],
                    known_names: None,
                })
            }),
        )
        .unwrap();

    for turn in [1, 2] {
        let assembly = prompt
            .assemble(AssembleContext {
                fields: Map::from_iter([("turn".to_owned(), json!(turn))]),
                ..AssembleContext::default()
            })
            .await
            .unwrap();
        assert_eq!(
            assembly
                .contexts
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["earlier", "later"]
        );
        assert_eq!(
            assembly
                .tools
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
    }
    assert_eq!(*section_contexts.lock(), [json!(1), json!(2)]);
    dynamic.dispose().await.unwrap();
    assert!(
        prompt
            .assemble(AssembleContext::default())
            .await
            .unwrap()
            .sections
            .iter()
            .all(|section| section.name != "dynamic")
    );
}

#[tokio::test]
async fn rejects_duplicates_nonfinite_orders_and_rolls_back_failed_change_notifications() {
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    prompt
        .section(&context, PromptSection::new("same", 1.0, "first"))
        .unwrap();
    assert!(
        prompt
            .section(&context, PromptSection::new("same", 2.0, "second"))
            .unwrap_err()
            .to_string()
            .contains("already registered")
    );
    for order in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            prompt
                .section(&context, PromptSection::new("bad", order, "x"))
                .is_err()
        );
        assert!(
            prompt
                .prompt_context(&context, PromptContext::new("bad", order, "x"))
                .is_err()
        );
    }

    context
        .events()
        .on_sync(
            &context,
            "system-prompt/change",
            |_, _| anyhow::bail!("listener exploded"),
            EventOptions::default(),
        )
        .unwrap();
    assert!(
        prompt
            .section(&context, PromptSection::new("rolled-back", 3.0, "x"))
            .unwrap_err()
            .to_string()
            .contains("listener exploded")
    );
    assert!(
        prompt
            .tools(&context, Arc::new(|_| Ok(ToolProviderResult::default())))
            .unwrap_err()
            .to_string()
            .contains("listener exploded")
    );
    assert!(
        prompt
            .variable(
                &context,
                "rolled_back",
                Arc::new(|_| Ok(Some("x".to_owned())))
            )
            .unwrap_err()
            .to_string()
            .contains("listener exploded")
    );
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert!(
        assembly
            .sections
            .iter()
            .all(|item| item.name != "rolled-back")
    );
    assert!(assembly.tools.is_empty());
    assert!(!assembly.variables.contains_key("rolled_back"));
}

#[tokio::test]
async fn snapshots_tool_membership_but_live_iterates_variable_registration() {
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    let weak = Arc::downgrade(&prompt);
    let tool_context = context.clone();
    let added_tool = Arc::new(Mutex::new(false));
    let tool_flag = added_tool.clone();
    prompt
        .tools(
            &context,
            Arc::new(move |_| {
                if !*tool_flag.lock() {
                    *tool_flag.lock() = true;
                    weak.upgrade().unwrap().tools(
                        &tool_context,
                        Arc::new(|_| {
                            Ok(ToolProviderResult {
                                schemas: vec![tool("late")],
                                known_names: None,
                            })
                        }),
                    )?;
                }
                Ok(ToolProviderResult {
                    schemas: vec![tool("first")],
                    known_names: None,
                })
            }),
        )
        .unwrap();
    let first = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(
        first
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["first"]
    );
    let second = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(
        second
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["first", "late"]
    );

    let weak = Arc::downgrade(&prompt);
    let variable_context = context.clone();
    let added_variable = Arc::new(Mutex::new(false));
    let variable_flag = added_variable.clone();
    prompt
        .variable(
            &context,
            "first",
            Arc::new(move |_| {
                if !*variable_flag.lock() {
                    *variable_flag.lock() = true;
                    weak.upgrade().unwrap().variable(
                        &variable_context,
                        "late",
                        Arc::new(|_| Ok(Some("second value".to_owned()))),
                    )?;
                }
                Ok(Some("first value".to_owned()))
            }),
        )
        .unwrap();
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(
        assembly.variables,
        IndexMap::from([
            ("first".to_owned(), Some("first value".to_owned())),
            ("late".to_owned(), Some("second value".to_owned())),
        ])
    );
}

#[tokio::test]
async fn waterfall_composes_short_circuits_and_cannot_override_complete_sections() {
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    let order = Arc::new(Mutex::new(Vec::new()));
    for label in ["first", "second"] {
        let order = order.clone();
        prompt
            .on_assemble(
                &context,
                move |mut assembly, _, next| {
                    let order = order.clone();
                    async move {
                        order.lock().push(format!("{label}:before"));
                        assembly.sections.push(AssembledSection {
                            name: format!("{label}:input"),
                            text: label.to_owned(),
                        });
                        let mut assembly = next.run_with(assembly).await?;
                        order.lock().push(format!("{label}:after"));
                        assembly.sections.push(AssembledSection {
                            name: format!("{label}:output"),
                            text: label.to_owned(),
                        });
                        Ok(assembly)
                    }
                },
                EventOptions::default(),
            )
            .unwrap();
    }
    prompt
        .section(
            &context,
            PromptSection::new("complete", 50.0, "authoritative").complete(),
        )
        .unwrap();
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(
        *order.lock(),
        [
            "first:before",
            "second:before",
            "second:after",
            "first:after"
        ]
    );
    assert_eq!(
        assembly.sections,
        [AssembledSection {
            name: "complete".to_owned(),
            text: "authoritative".to_owned(),
        }]
    );

    let short_context = Context::new();
    let prompt = SystemPrompt::new(&short_context, SystemPromptConfig::default()).unwrap();
    prompt
        .on_assemble(
            &short_context,
            |_, _, _| async move {
                Ok(PromptAssembly {
                    sections: vec![AssembledSection {
                        name: "short".to_owned(),
                        text: "short".to_owned(),
                    }],
                    ..PromptAssembly::default()
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    prompt
        .on_assemble(
            &short_context,
            |_, _, _| async move { anyhow::bail!("must not run") },
            EventOptions::default(),
        )
        .unwrap();
    assert_eq!(
        prompt
            .assemble(AssembleContext::default())
            .await
            .unwrap()
            .sections[0]
            .name,
        "short"
    );
}

#[tokio::test]
async fn rejects_multiple_complete_sections_and_detaches_assembly_mutations() {
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    prompt
        .section(&context, PromptSection::new("one", 1.0, "one").complete())
        .unwrap();
    prompt
        .section(&context, PromptSection::new("two", 2.0, "two").complete())
        .unwrap();
    assert!(
        prompt
            .assemble(AssembleContext::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("multiple complete prompt sections")
    );

    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    prompt
        .tools(
            &context,
            Arc::new(|_| {
                Ok(ToolProviderResult {
                    schemas: vec![tool("echo")],
                    known_names: None,
                })
            }),
        )
        .unwrap();
    let first = prompt.assemble(AssembleContext::default()).await.unwrap();
    let mut mutated = first;
    mutated.sections.clear();
    mutated.tools[0].description = "mutated".to_owned();
    mutated.tools[0]
        .parameters
        .insert("leak".to_owned(), json!(true));
    let second = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert!(!second.sections.is_empty());
    assert_eq!(second.tools[0].description, "echo tool");
    assert!(!second.tools[0].parameters.contains_key("leak"));
}

#[tokio::test]
async fn change_events_and_scope_disposal_remove_every_owned_provider() {
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    let changes = Arc::new(AtomicUsize::new(0));
    let counted = changes.clone();
    context
        .events()
        .on_sync(
            &context,
            "system-prompt/change",
            move |_, _| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let scope = create_scope(&context, ScopeKey::new(), None).unwrap();
    prompt
        .section(&scope.context, PromptSection::new("owned", 0.0, "x"))
        .unwrap();
    prompt
        .prompt_context(&scope.context, PromptContext::new("owned", 0.0, "x"))
        .unwrap();
    prompt
        .tools(
            &scope.context,
            Arc::new(|_| Ok(ToolProviderResult::default())),
        )
        .unwrap();
    prompt
        .variable(&scope.context, "owned", Arc::new(|_| Ok(None)))
        .unwrap();
    assert_eq!(changes.load(Ordering::SeqCst), 4);
    scope.dispose().await.unwrap();
    assert_eq!(changes.load(Ordering::SeqCst), 8);
}
