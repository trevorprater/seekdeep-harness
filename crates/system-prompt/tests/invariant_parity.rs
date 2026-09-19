//! Authoritative assembly-invariant parity specifications.

use std::sync::Arc;

use indexmap::IndexMap;
use seekdeep_cordis::{Context, EventArgs, EventReply};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm::ToolSchema;
use seekdeep_system_prompt::{
    AssembleContext, AssembledContext, AssembledSection, PromptAssembly, register_invariant,
};
use serde_json::Map;

fn valid() -> PromptAssembly {
    PromptAssembly {
        sections: vec![AssembledSection {
            name: "identity".to_owned(),
            text: "prompt".to_owned(),
        }],
        contexts: vec![AssembledContext {
            name: "policy".to_owned(),
            text: "current policy".to_owned(),
        }],
        tools: vec![ToolSchema {
            name: "echo".to_owned(),
            description: "Echo".to_owned(),
            parameters: Map::new(),
        }],
        variables: IndexMap::from([
            ("cwd".to_owned(), Some("/repo".to_owned())),
            ("optional".to_owned(), None),
        ]),
    }
}

async fn setup() -> (Context, seekdeep_invariants::InvariantRegistration) {
    let context = Context::new();
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
    let registration = register_invariant(&registry).expect("register");
    registration.await_ready().await.expect("invariant ready");
    (context, registration)
}

async fn authoritative(
    context: &Context,
    result: PromptAssembly,
) -> anyhow::Result<PromptAssembly> {
    let args = EventArgs::from_values(vec![
        Arc::new(valid()),
        Arc::new(AssembleContext::default()),
    ]);
    context
        .events()
        .waterfall(context, "system-prompt/assemble", &args, move || {
            Box::pin(async move { Ok(EventReply::Value(Arc::new(result))) })
        })
        .await?
        .downcast::<PromptAssembly>()
        .map(|assembly| (*assembly).clone())
        .ok_or_else(|| anyhow::anyhow!("missing authoritative assembly"))
}

#[tokio::test]
async fn accepts_well_formed_authoritative_assembly() {
    let (context, _registration) = setup().await;
    assert_eq!(authoritative(&context, valid()).await.unwrap(), valid());
}

#[tokio::test]
async fn rejects_every_malformed_shape_representable_by_the_rust_contract() {
    let (context, _registration) = setup().await;
    let cases = [
        (
            PromptAssembly {
                sections: vec![AssembledSection {
                    name: String::new(),
                    text: "x".to_owned(),
                }],
                ..valid()
            },
            "section names must be non-empty",
        ),
        (
            PromptAssembly {
                sections: vec![
                    AssembledSection {
                        name: "x".to_owned(),
                        text: "a".to_owned(),
                    },
                    AssembledSection {
                        name: "x".to_owned(),
                        text: "b".to_owned(),
                    },
                ],
                ..valid()
            },
            "section name \"x\" is duplicated",
        ),
        (
            PromptAssembly {
                contexts: vec![AssembledContext {
                    name: String::new(),
                    text: "x".to_owned(),
                }],
                ..valid()
            },
            "context names must be non-empty",
        ),
        (
            PromptAssembly {
                contexts: vec![
                    AssembledContext {
                        name: "x".to_owned(),
                        text: "a".to_owned(),
                    },
                    AssembledContext {
                        name: "x".to_owned(),
                        text: "b".to_owned(),
                    },
                ],
                ..valid()
            },
            "context name \"x\" is duplicated",
        ),
        (
            PromptAssembly {
                tools: vec![ToolSchema {
                    name: String::new(),
                    description: "x".to_owned(),
                    parameters: Map::new(),
                }],
                ..valid()
            },
            "tool names must be non-empty",
        ),
        (
            PromptAssembly {
                variables: IndexMap::from([("Bad".to_owned(), Some("x".to_owned()))]),
                ..valid()
            },
            "variable name \"Bad\" is invalid",
        ),
    ];

    for (assembly, expected) in cases {
        let error = authoritative(&context, assembly).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}
