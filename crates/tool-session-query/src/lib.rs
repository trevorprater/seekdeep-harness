//! Model-facing, workspace-authorized session-history search and read tools.

pub mod input;
pub mod operations;
pub mod presentation;
pub mod service_boundary;
pub mod workspace_access;

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin};
use seekdeep_llm::ContentBlock;
use seekdeep_llm::HarnessError;
use seekdeep_schemastery::Schema;
use seekdeep_system_prompt::{PromptSection, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, TOOLS, ToolCallView, ToolRuntime, define_tool,
};
use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use input::{
    EventReadArgs, EventSearchArgs, EventTargetArgs, SessionSearchArgs, SessionTargetArgs,
};

/// Cordis plugin name used by Loader diagnostics.
pub const NAME: &str = "tool-session-query";
/// Capability services required by the model-facing consumer.
pub const INJECT: &[&str] = &["tools", "systemPrompt", "sessionQuery"];
/// Default maximum number of authorized search hits returned by one call.
pub const DEFAULT_MAX_SEARCH_RESULTS: f64 = 100.0;
/// Default cooperative deadline for either full-text search tool.
pub const DEFAULT_SEARCH_TIMEOUT_MS: f64 = 30_000.0;

const PROMPT_TEXT: &str = "Use session_search to find relevant work from prior sessions, or session_event_search to search earlier events in one session. Search results are cursor-free and workspace-scoped. Follow a useful hit with session_trace, session_event_trace, or session_event_read when you need lineage, relationships, or exact data.";

/// Deployment-owned search count and timeout bounds.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Maximum authorized hits returned by one search call.
    pub max_search_results: Option<f64>,
    /// Cooperative full-text search deadline in milliseconds.
    pub search_timeout_ms: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ResolvedConfig {
    max_search_results: usize,
    search_timeout_ms: f64,
}

/// Source-compatible Loader configuration schema.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        (
            "maxSearchResults",
            Schema::number()
                .min(1.0)
                .with_default(DEFAULT_MAX_SEARCH_RESULTS),
        ),
        (
            "searchTimeoutMs",
            Schema::number()
                .min(1.0)
                .max(MAX_TIMER_DELAY_MS)
                .with_default(DEFAULT_SEARCH_TIMEOUT_MS),
        ),
    ])
}

/// Loader-facing namespace-style Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, value| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(value)?;
            apply(&context, &config)
        })
    })
    .with_config_validator(|value| {
        let resolved = config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let config: Config = serde_json::from_value(resolved.clone())?;
        resolve_config(&config)?;
        Ok(resolved)
    })
}

/// Registers all five tools and their shared model guidance.
///
/// # Errors
///
/// Returns invalid configuration, missing capability, schema, prompt, or
/// duplicate-registration failures.
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let resolved = resolve_config(config)?;
    let tools: Arc<ToolRuntime> = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-session-query requires tools"))?;
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-session-query requires systemPrompt"))?;
    prompt.section(
        context,
        PromptSection::new("tool:session-query", 113.0, PROMPT_TEXT),
    )?;
    tools.register(context, session_search_definition(context, resolved)?)?;
    tools.register(context, event_search_definition(context, resolved)?)?;
    tools.register(context, session_trace_definition(context)?)?;
    tools.register(context, event_trace_definition(context)?)?;
    tools.register(context, event_read_definition(context)?)?;
    Ok(())
}

fn session_search_definition(
    context: &Context,
    config: ResolvedConfig,
) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let execution_context = context.clone();
    let output = text_output::<SessionSearchArgs>();
    define_tool(
        DefineToolOptions::new(
            "session_search",
            "Search prior sessions in the caller workspace and return the strongest matching event from each session.",
            Value::Object(input::session_search_parameters()),
            output,
            Arc::new(move |args: SessionSearchArgs, run| {
                let context = execution_context.clone();
                Box::pin(async move {
                    operations::execute_session_search(
                        &context,
                        &args,
                        &run,
                        config.max_search_results,
                    )
                    .await
                    .map_err(translate_local_error)
                })
            }),
        )
        .timeout_ms(config.search_timeout_ms)
        .present_call(Arc::new(|args| {
            Some(ToolCallView::Generic(
                presentation::present_session_search_call(args),
            ))
        })),
    )
}

fn event_search_definition(
    context: &Context,
    config: ResolvedConfig,
) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let execution_context = context.clone();
    define_tool(
        DefineToolOptions::new(
            "session_event_search",
            "Search prior events in one authorized session; the current session excludes the step performing this call.",
            Value::Object(input::event_search_parameters()),
            text_output::<EventSearchArgs>(),
            Arc::new(move |args: EventSearchArgs, run| {
                let context = execution_context.clone();
                Box::pin(async move {
                    operations::execute_event_search(
                        &context,
                        &args,
                        &run,
                        config.max_search_results,
                    )
                    .await
                    .map_err(translate_local_error)
                })
            }),
        )
        .timeout_ms(config.search_timeout_ms)
        .present_call(Arc::new(|args| {
            Some(ToolCallView::Generic(
                presentation::present_event_search_call(args),
            ))
        })),
    )
}

fn session_trace_definition(context: &Context) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let execution_context = context.clone();
    define_tool(
        DefineToolOptions::new(
            "session_trace",
            "Read the authorized session lineage around one session, including complete visible ancestor and descendant relationships.",
            Value::Object(input::target_session_parameters()),
            text_output::<SessionTargetArgs>(),
            Arc::new(move |args: SessionTargetArgs, run| {
                let context = execution_context.clone();
                Box::pin(async move {
                    operations::execute_session_trace(&context, &args, &run)
                        .await
                        .map_err(translate_local_error)
                })
            }),
        )
        .concurrency_safe(Arc::new(|_| true))
        .present_call(Arc::new(|args| {
            Some(ToolCallView::Generic(
                presentation::present_session_trace_call(args),
            ))
        })),
    )
}

fn event_trace_definition(context: &Context) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let execution_context = context.clone();
    let mut parameters = input::target_session_parameters();
    parameters.insert(
        "seq".to_owned(),
        json!({"type":"integer","required":true,"description":"Target event sequence number."}),
    );
    define_tool(
        DefineToolOptions::new(
            "session_event_trace",
            "Read every direct replacement and relationship to a cited source event for one event in an authorized session.",
            Value::Object(parameters),
            text_output::<EventTargetArgs>(),
            Arc::new(move |args: EventTargetArgs, run| {
                let context = execution_context.clone();
                Box::pin(async move {
                    operations::execute_event_trace(&context, &args, &run)
                        .await
                        .map_err(translate_local_error)
                })
            }),
        )
        .concurrency_safe(Arc::new(|_| true))
        .present_call(Arc::new(|args| {
            Some(ToolCallView::Generic(
                presentation::present_event_target_call("Trace event", args),
            ))
        })),
    )
}

fn event_read_definition(context: &Context) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let execution_context = context.clone();
    let mut parameters = input::target_session_parameters();
    parameters.extend(Map::from_iter([
        (
            "seq".to_owned(),
            json!({"type":"integer","required":true,"description":"Target event sequence number."}),
        ),
        (
            "before".to_owned(),
            json!({"type":"integer","description":"Number of preceding raw events to summarize. Omit for none."}),
        ),
        (
            "after".to_owned(),
            json!({"type":"integer","description":"Number of following raw events to summarize. Omit for none."}),
        ),
    ]));
    define_tool(
        DefineToolOptions::new(
            "session_event_read",
            "Read one full unabridged event and optional neighboring raw-event summaries from an authorized session.",
            Value::Object(parameters),
            text_output::<EventReadArgs>(),
            Arc::new(move |args: EventReadArgs, run| {
                let context = execution_context.clone();
                Box::pin(async move {
                    operations::execute_event_read(&context, &args, &run)
                        .await
                        .map_err(translate_local_error)
                })
            }),
        )
        .concurrency_safe(Arc::new(|_| true))
        .present_call(Arc::new(|args| {
            let target = EventTargetArgs {
                session_id: args.session_id.clone(),
                seq: args.seq,
            };
            Some(ToolCallView::Generic(
                presentation::present_event_target_call("Read event", &target),
            ))
        })),
    )
}

fn text_output<Args>() -> DefineToolOutput<Args, String> {
    DefineToolOutput::new(
        json!({"type":"string"}),
        Arc::new(|_, value| {
            Ok(vec![ContentBlock::Text {
                text: value.clone(),
            }])
        }),
    )
}

fn resolve_config(config: &Config) -> anyhow::Result<ResolvedConfig> {
    let max_search_results = config
        .max_search_results
        .unwrap_or(DEFAULT_MAX_SEARCH_RESULTS);
    let search_timeout_ms = config
        .search_timeout_ms
        .unwrap_or(DEFAULT_SEARCH_TIMEOUT_MS);
    anyhow::ensure!(
        max_search_results.is_finite()
            && max_search_results.fract() == 0.0
            && (1.0..=9_007_199_254_740_991.0).contains(&max_search_results),
        "tool-session-query: maxSearchResults must be a positive safe integer"
    );
    anyhow::ensure!(
        search_timeout_ms.is_finite()
            && search_timeout_ms.fract() == 0.0
            && (1.0..=MAX_TIMER_DELAY_MS).contains(&search_timeout_ms),
        "tool-session-query: searchTimeoutMs must be a positive integer no greater than {MAX_TIMER_DELAY_MS}"
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(ResolvedConfig {
        max_search_results: max_search_results as usize,
        search_timeout_ms,
    })
}

fn translate_local_error(error: anyhow::Error) -> anyhow::Error {
    match error.downcast::<seekdeep_session_query::SessionQueryError>() {
        Ok(error) => {
            HarnessError::named("SessionQueryError", error.message, error.code.as_str()).into()
        }
        Err(error) => error,
    }
}
