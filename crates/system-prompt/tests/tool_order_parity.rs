//! Canonical tool-order parity specifications.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventOptions};
use seekdeep_llm::ToolSchema;
use seekdeep_system_prompt::{
    AssembleContext, SystemPrompt, SystemPromptConfig, TOOL_ORDER_REST, ToolProviderResult,
};
use serde_json::Map;

fn tool(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.to_owned(),
        description: format!("{name} tool"),
        parameters: Map::new(),
    }
}

fn names(tools: &[ToolSchema]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name.as_str()).collect()
}

fn provider(names: &[&str]) -> seekdeep_system_prompt::ToolProvider {
    let schemas = names.iter().map(|name| tool(name)).collect::<Vec<_>>();
    Arc::new(move |_| {
        Ok(ToolProviderResult {
            schemas: schemas.clone(),
            known_names: None,
        })
    })
}

#[tokio::test]
async fn defaults_to_stable_javascript_code_unit_order_independent_of_registration_order() {
    assert_eq!(TOOL_ORDER_REST, "<unlisted-tools>");
    for groups in [
        vec![vec!["zebra", "alpha", "middle"]],
        vec![vec!["middle"], vec!["zebra"], vec!["alpha"]],
        vec![vec!["alpha"], vec!["middle", "zebra"]],
    ] {
        let context = Context::new();
        let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
        for group in groups {
            prompt.tools(&context, provider(&group)).unwrap();
        }
        let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
        assert_eq!(names(&assembly.tools), ["alpha", "middle", "zebra"]);
    }

    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    // ECMAScript compares UTF-16 code units: the astral surrogate sorts before
    // the BMP private-use character even though Unicode scalar order does not.
    prompt
        .tools(&context, provider(&["\u{e000}", "\u{10000}"]))
        .unwrap();
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(names(&assembly.tools), ["\u{10000}", "\u{e000}"]);
}

#[tokio::test]
async fn explicit_order_places_listed_and_rest_tools_and_validates_known_names() {
    let context = Context::new();
    let prompt = SystemPrompt::new(
        &context,
        SystemPromptConfig {
            tool_order: Some(vec![
                "zebra".to_owned(),
                TOOL_ORDER_REST.to_owned(),
                "alpha".to_owned(),
            ]),
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    prompt
        .tools(&context, provider(&["alpha", "two", "zebra", "one"]))
        .unwrap();
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(names(&assembly.tools), ["zebra", "one", "two", "alpha"]);

    let restricted = Context::new();
    let prompt = SystemPrompt::new(
        &restricted,
        SystemPromptConfig {
            tool_order: Some(vec!["hidden".to_owned(), TOOL_ORDER_REST.to_owned()]),
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    prompt
        .tools(
            &restricted,
            Arc::new(|_| {
                Ok(ToolProviderResult {
                    schemas: vec![tool("visible")],
                    known_names: Some(vec!["visible".to_owned(), "hidden".to_owned()]),
                })
            }),
        )
        .unwrap();
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(names(&assembly.tools), ["visible"]);
}

#[tokio::test]
async fn rejects_unknown_reserved_duplicate_and_missing_rest_configuration() {
    for order in [
        vec!["alpha".to_owned()],
        vec![TOOL_ORDER_REST.to_owned(), TOOL_ORDER_REST.to_owned()],
    ] {
        assert!(
            SystemPrompt::new(
                &Context::new(),
                SystemPromptConfig {
                    tool_order: Some(order),
                    ..SystemPromptConfig::default()
                },
            )
            .is_err()
        );
    }

    let context = Context::new();
    let prompt = SystemPrompt::new(
        &context,
        SystemPromptConfig {
            tool_order: Some(vec!["missing".to_owned(), TOOL_ORDER_REST.to_owned()]),
            ..SystemPromptConfig::default()
        },
    )
    .unwrap();
    let error = prompt
        .assemble(AssembleContext::default())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("toolOrder lists unregistered tool \"missing\"; known tools: (none)")
    );

    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    prompt
        .tools(&context, provider(&[TOOL_ORDER_REST]))
        .unwrap();
    assert!(
        prompt
            .assemble(AssembleContext::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("reserved tool name")
    );
}

#[tokio::test]
async fn stable_sort_preserves_provider_order_for_duplicate_names() {
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    prompt
        .tools(
            &context,
            Arc::new(|_| {
                Ok(ToolProviderResult {
                    schemas: vec![
                        ToolSchema {
                            name: "same".to_owned(),
                            description: "first".to_owned(),
                            parameters: Map::new(),
                        },
                        ToolSchema {
                            name: "same".to_owned(),
                            description: "second".to_owned(),
                            parameters: Map::new(),
                        },
                    ],
                    known_names: None,
                })
            }),
        )
        .unwrap();
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(
        assembly
            .tools
            .iter()
            .map(|tool| tool.description.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[tokio::test]
async fn canonicalizes_before_waterfall_and_preserves_listener_owned_append_order() {
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    prompt
        .tools(&context, provider(&["zulu", "alpha"]))
        .unwrap();
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let observed = seen.clone();
    prompt
        .on_assemble(
            &context,
            move |mut assembly, _, next| {
                let observed = observed.clone();
                async move {
                    *observed.lock() = assembly
                        .tools
                        .iter()
                        .map(|tool| tool.name.clone())
                        .collect();
                    assembly.tools.push(tool("aardvark"));
                    next.run_with(assembly).await
                }
            },
            EventOptions::default(),
        )
        .unwrap();
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert_eq!(*seen.lock(), ["alpha", "zulu"]);
    assert_eq!(names(&assembly.tools), ["alpha", "zulu", "aardvark"]);
}
