//! Model-facing `ask_user_question` adapter over the user-question seam.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::ContentBlock;
use seekdeep_tools::{DefineToolOptions, DefineToolOutput, TOOLS, ToolRuntime, define_tool};
use seekdeep_user_questions::{
    AskUserQuestionAnswer, AskUserQuestionItem, AskUserQuestionOption, AskUserQuestionRequest,
    USER_QUESTIONS, UserQuestionService,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Stable public tool name.
pub const TOOL_NAME: &str = "ask_user_question";
/// Loader plugin name.
pub const NAME: &str = "tool-ask-user";
/// Loader service dependencies.
pub const INJECT: &[&str] = &["tools", "userQuestions"];

const DESCRIPTION: &str = "Ask the user a concise question when you need confirmation, a choice, or missing information before proceeding. Send one or more questions, each with a stable id that will be echoed in the answer.";

#[derive(Clone, Debug, Deserialize)]
struct ToolOption {
    label: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ToolQuestion {
    id: String,
    question: String,
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    options: Option<Vec<ToolOption>>,
    #[serde(default)]
    multi_select: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
struct ToolArgs {
    questions: Vec<ToolQuestion>,
}

fn output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "answers": {
                "type": "array", "required": true,
                "items": {
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "id": {"type": "string", "required": true},
                        "selected": {
                            "type": "array", "required": true,
                            "items": {"type": "string"}
                        },
                        "custom": {"type": "string"}
                    }
                }
            }
        }
    })
}

fn parameter_schema() -> Value {
    json!({
        "questions": {
            "type": "array", "required": true,
            "description": "Questions to ask the user before continuing.",
            "items": {
                "type": "object", "additionalProperties": true,
                "properties": {
                    "id": {
                        "type": "string", "required": true,
                        "description": "Stable id for this question; echoed in the answer."
                    },
                    "question": {
                        "type": "string", "required": true,
                        "description": "The specific question to ask the user."
                    },
                    "header": {
                        "type": "string",
                        "description": "Optional short heading for the question, such as \"Confirm\" or \"Choose Mode\"."
                    },
                    "options": {
                        "type": "array",
                        "description": "Optional choices to show the user. If you recommend one, put it first and append \"(Recommended)\" to that label.",
                        "items": {
                            "type": "object", "additionalProperties": true,
                            "properties": {
                                "label": {
                                    "type": "string", "required": true,
                                    "description": "Short user-facing option label."
                                },
                                "description": {
                                    "type": "string",
                                    "description": "One sentence explaining the tradeoff or impact."
                                }
                            }
                        }
                    },
                    "multi_select": {
                        "type": "boolean",
                        "description": "Whether the user may select more than one option. Defaults to false."
                    }
                }
            }
        }
    })
}

/// Builds the exact model-facing tool definition.
///
/// # Errors
///
/// Returns only author-schema compilation failures.
pub fn definition(
    user_questions: Arc<UserQuestionService>,
) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let output = DefineToolOutput::new(
        output_schema(),
        Arc::new(|_: &ToolArgs, value: &AskUserQuestionAnswer| {
            Ok(vec![ContentBlock::Text {
                text: serde_json::to_string(value)?,
            }])
        }),
    );
    define_tool(DefineToolOptions::new(
        TOOL_NAME,
        DESCRIPTION,
        parameter_schema(),
        output,
        Arc::new(move |args: ToolArgs, execution| {
            let user_questions = user_questions.clone();
            Box::pin(async move {
                let questions = args
                    .questions
                    .into_iter()
                    .map(|question| AskUserQuestionItem {
                        id: question.id,
                        question: question.question,
                        detail: None,
                        header: question.header,
                        options: question.options.map(|options| {
                            options
                                .into_iter()
                                .map(|option| AskUserQuestionOption {
                                    label: option.label,
                                    description: option.description,
                                })
                                .collect()
                        }),
                        multi_select: question.multi_select,
                        intent: None,
                    })
                    .collect();
                let answer = user_questions
                    .ask(AskUserQuestionRequest {
                        questions,
                        agent: execution.agent.clone(),
                        signal: Some(execution.signal()),
                    })
                    .await?;
                Ok(answer)
            })
        }),
    ))
}

/// Registers the adapter using the exact services published in `context`.
///
/// # Errors
///
/// Returns when either dependency is absent or tool registration fails.
pub fn apply(context: &Context) -> anyhow::Result<EffectHandle> {
    let tools: Arc<ToolRuntime> = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-ask-user requires tools"))?;
    let user_questions = context
        .get(USER_QUESTIONS)
        .ok_or_else(|| anyhow::anyhow!("tool-ask-user requires userQuestions"))?;
    tools.register(context, definition(user_questions)?)
}

/// Builds the Loader-compatible user-question tool plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            apply(&context)?;
            Ok(())
        })
    })
}

/// Registers the package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-tool-ask-user", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let registration = register_invariant(&registry).unwrap();
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.unwrap();
        register_invariant(&registry).unwrap();
    }
}
