//! Scoped registry and dispatch parity specifications.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, fiber::EffectHandle};
use seekdeep_llm::ToolSchema;
use seekdeep_scope::{Scope, ScopeKey, create_scope};
use seekdeep_system_prompt::{
    AssembleContext, AssembledSection, PERSONA_ORDER, PERSONA_SECTION, PromptContext,
    PromptSection, PromptText, SystemPrompt, SystemPromptConfig, TOOL_ORDER_REST,
    ToolProviderResult, render_context_snapshot, render_prompt,
};
use serde_json::Map;

fn mint_scope(root: &Context, key: ScopeKey) -> Scope {
    create_scope(root, key, None).unwrap()
}

fn scoped_context(key: ScopeKey) -> AssembleContext {
    AssembleContext {
        scope: Some(key),
        ..AssembleContext::default()
    }
}

fn schema(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.to_owned(),
        description: format!("tool {name}"),
        parameters: Map::new(),
    }
}

#[tokio::test]
async fn scoped_sections_shadow_before_evaluation_and_unwind_without_global_leakage() {
    let root = Context::new();
    let prompt = SystemPrompt::new(
        &root,
        SystemPromptConfig {
            persona: "You are the deployment.".to_owned(),
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    let key = ScopeKey::new();
    let scope = mint_scope(&root, key);
    prompt
        .section(
            &scope.context,
            PromptSection::new(PERSONA_SECTION, PERSONA_ORDER, "You run tests."),
        )
        .unwrap();
    prompt
        .section(
            &scope.context,
            PromptSection::new("child:extra", 50.0, "Extra guidance."),
        )
        .unwrap();
    let scoped = render_prompt(&prompt.assemble(scoped_context(key)).await.unwrap()).unwrap();
    let global =
        render_prompt(&prompt.assemble(AssembleContext::default()).await.unwrap()).unwrap();
    assert!(scoped.contains("You run tests."));
    assert!(!scoped.contains("You are the deployment."));
    assert!(scoped.contains("Extra guidance."));
    assert!(global.contains("You are the deployment."));
    assert!(!global.contains("You run tests."));
    assert!(!global.contains("Extra guidance."));
    scope.dispose().await.unwrap();
    assert!(
        !render_prompt(&prompt.assemble(scoped_context(key)).await.unwrap())
            .unwrap()
            .contains("Extra guidance.")
    );

    let key = ScopeKey::new();
    let scope = mint_scope(&root, key);
    let global_calls = Arc::new(Mutex::new(0));
    let counted = global_calls.clone();
    prompt
        .section(
            &root,
            PromptSection::new(
                "shared",
                1.0,
                PromptText::Dynamic(Arc::new(move |_| {
                    *counted.lock() += 1;
                    Ok("global text".to_owned())
                })),
            ),
        )
        .unwrap();
    prompt
        .section(
            &scope.context,
            PromptSection::new("shared", 1.0, "scoped text"),
        )
        .unwrap();
    let assembly = prompt.assemble(scoped_context(key)).await.unwrap();
    assert_eq!(
        assembly
            .sections
            .iter()
            .find(|section| section.name == "shared")
            .unwrap()
            .text,
        "scoped text"
    );
    assert_eq!(*global_calls.lock(), 0);
}

#[tokio::test]
async fn duplicate_names_are_layer_local_and_scoped_variables_shadow_globals() {
    let root = Context::new();
    let prompt = SystemPrompt::new(
        &root,
        SystemPromptConfig {
            persona: "Mode: {{mode}}.".to_owned(),
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    prompt
        .section(&root, PromptSection::new("x", 1.0, "a"))
        .unwrap();
    assert!(
        prompt
            .section(&root, PromptSection::new("x", 1.0, "b"))
            .unwrap_err()
            .to_string()
            .contains("agent.ctx")
    );
    prompt
        .variable(&root, "mode", Arc::new(|_| Ok(Some("normal".to_owned()))))
        .unwrap();

    let key = ScopeKey::new();
    let scope = mint_scope(&root, key);
    prompt
        .section(&scope.context, PromptSection::new("y", 1.0, "a"))
        .unwrap();
    assert!(
        prompt
            .section(&scope.context, PromptSection::new("y", 1.0, "b"))
            .unwrap_err()
            .to_string()
            .contains("already registered in this scope")
    );
    prompt
        .variable(
            &scope.context,
            "mode",
            Arc::new(|_| Ok(Some("strict".to_owned()))),
        )
        .unwrap();
    assert!(
        render_prompt(&prompt.assemble(scoped_context(key)).await.unwrap())
            .unwrap()
            .contains("Mode: strict.")
    );
    assert!(
        render_prompt(&prompt.assemble(AssembleContext::default()).await.unwrap())
            .unwrap()
            .contains("Mode: normal.")
    );
}

#[tokio::test]
async fn replacing_the_last_scoped_variable_generation_is_deferred_to_the_next_assembly() {
    let root = Context::new();
    let prompt = SystemPrompt::new(
        &root,
        SystemPromptConfig {
            persona: "Mode: {{mode}}.".to_owned(),
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    let key = ScopeKey::new();
    let scope = mint_scope(&root, key);
    prompt
        .section(
            &scope.context,
            PromptSection::new("scope:sibling", 1.0, "Scoped."),
        )
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handle = Arc::new(Mutex::new(None::<EffectHandle>));
    let handle_for_provider = handle.clone();
    let calls_for_provider = calls.clone();
    let prompt_for_provider = Arc::downgrade(&prompt);
    let context_for_provider = scope.context.clone();
    let effect = prompt
        .variable(
            &scope.context,
            "mode",
            Arc::new(move |_| {
                calls_for_provider.lock().push("first");
                let effect = handle_for_provider.lock().take().unwrap();
                futures::executor::block_on(effect.dispose())?;
                let calls = calls_for_provider.clone();
                prompt_for_provider.upgrade().unwrap().variable(
                    &context_for_provider,
                    "mode",
                    Arc::new(move |_| {
                        calls.lock().push("replacement");
                        Ok(Some("replacement".to_owned()))
                    }),
                )?;
                Ok(Some("first".to_owned()))
            }),
        )
        .unwrap();
    *handle.lock() = Some(effect);

    assert!(
        render_prompt(&prompt.assemble(scoped_context(key)).await.unwrap())
            .unwrap()
            .contains("Mode: first.")
    );
    assert_eq!(*calls.lock(), ["first"]);
    assert!(
        render_prompt(&prompt.assemble(scoped_context(key)).await.unwrap())
            .unwrap()
            .contains("Mode: replacement.")
    );
    assert_eq!(*calls.lock(), ["first", "replacement"]);
}

#[tokio::test]
async fn scoped_context_shadow_and_suppression_restore_on_disposal() {
    let root = Context::new();
    let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).unwrap();
    prompt
        .prompt_context(&root, PromptContext::new("policy", 1.0, "global policy"))
        .unwrap();
    let key = ScopeKey::new();
    let scope = mint_scope(&root, key);
    prompt
        .prompt_context(
            &scope.context,
            PromptContext::new("policy", 1.0, "scoped policy"),
        )
        .unwrap();
    assert!(
        prompt
            .prompt_context(
                &scope.context,
                PromptContext::new("policy", 2.0, "duplicate"),
            )
            .is_err()
    );
    assert!(
        render_context_snapshot(&prompt.assemble(scoped_context(key)).await.unwrap())
            .unwrap()
            .contains("scoped policy")
    );
    assert!(
        render_context_snapshot(&prompt.assemble(AssembleContext::default()).await.unwrap())
            .unwrap()
            .contains("global policy")
    );
    scope.dispose().await.unwrap();
    assert!(
        render_context_snapshot(&prompt.assemble(scoped_context(key)).await.unwrap())
            .unwrap()
            .contains("global policy")
    );

    let key = ScopeKey::new();
    let scope = mint_scope(&root, key);
    let suppressor = prompt.suppress_runtime_context(&scope.context).unwrap();
    assert!(
        prompt
            .assemble(scoped_context(key))
            .await
            .unwrap()
            .contexts
            .is_empty()
    );
    suppressor.dispose().await.unwrap();
    assert!(
        render_context_snapshot(&prompt.assemble(scoped_context(key)).await.unwrap())
            .unwrap()
            .contains("global policy")
    );
}

#[tokio::test]
async fn scoped_tools_and_assemble_listeners_apply_only_to_their_scope() {
    let root = Context::new();
    let prompt = SystemPrompt::new(
        &root,
        SystemPromptConfig {
            tool_order: Some(vec!["hidden".to_owned(), TOOL_ORDER_REST.to_owned()]),
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    prompt
        .tools(
            &root,
            Arc::new(|_| {
                Ok(ToolProviderResult {
                    schemas: vec![schema("global")],
                    known_names: Some(vec!["global".to_owned(), "hidden".to_owned()]),
                })
            }),
        )
        .unwrap();
    let key = ScopeKey::new();
    let scope = mint_scope(&root, key);
    let scoped_tool = prompt
        .tools(
            &scope.context,
            Arc::new(|_| {
                Ok(ToolProviderResult {
                    schemas: vec![schema("scoped")],
                    known_names: None,
                })
            }),
        )
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    prompt
        .on_assemble(
            &scope.context,
            move |_, context, next| {
                let seen = seen.clone();
                async move {
                    seen.lock().push(context.scope);
                    let mut assembly = next.run().await?;
                    assembly.sections.push(AssembledSection {
                        name: "listener:extra".to_owned(),
                        text: "listener text".to_owned(),
                    });
                    Ok(assembly)
                }
            },
            EventOptions::default(),
        )
        .unwrap();
    let scoped = prompt.assemble(scoped_context(key)).await.unwrap();
    let global = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(
        scoped
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["global", "scoped"]
    );
    assert_eq!(
        global
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["global"]
    );
    assert!(
        scoped
            .sections
            .iter()
            .any(|item| item.name == "listener:extra")
    );
    assert!(
        global
            .sections
            .iter()
            .all(|item| item.name != "listener:extra")
    );
    assert_eq!(*calls.lock(), [Some(key)]);

    scoped_tool.dispose().await.unwrap();
    assert_eq!(
        prompt
            .assemble(scoped_context(key))
            .await
            .unwrap()
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["global"]
    );
}
