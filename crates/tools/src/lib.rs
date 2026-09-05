//! Tool schemas, registration, policy, scheduling, execution, and presentation.

/// Package-owned execution-pipeline and nested-dispatch invariants.
pub mod invariant;
/// The enforced lossless JSON Schema subset.
pub mod json_schema;
/// Provider-neutral pending and completed tool render intents.
pub mod presentation;
/// Python Code Mode SDK projection.
pub mod py_types;
/// Scoped tool registry and staged execution pipeline.
pub mod runtime;
/// Author-facing value-schema DSL.
pub mod schema;
/// Canonical typed definition fixtures for repository tests.
pub mod testing;
/// TypeScript Code Mode SDK projection.
pub mod ts_types;

pub use invariant::register_invariant;
pub use json_schema::{
    JsonSchemaError, JsonSchemaNode, ObjectJsonSchema, UNSUPPORTED_SCHEMA,
    assert_object_json_schema, assert_supported_json_schema, check_object_json_schema,
    check_supported_json_schema, validate_json_schema_value, validate_json_schema_value_at,
};
pub use presentation::{
    DiffCallView, DiffResultView, FileDiff, FileLocation, GenericCallView, GenericResultView,
    ReadFileLine, ReadResultView, SearchFileMatches, SearchLineMatch, SearchMatchesResultView,
    SearchPathsResultView, SearchResultView, TerminalCallView, TerminalResultView, ToolCallKind,
    ToolCallView, ToolResult, ToolResultView, WebFetchResultView, WebResultView,
    WebSearchResultView, WebSource,
};
pub use py_types::{json_schema_to_py, render_tools_sdk_py};
pub use runtime::{
    CodeDispatchEventData, CodeDispatchLog, CodeDispatchLogNext, CodeDispatchStartEventData,
    ExecuteToolNext, PostToolDecision, PostToolNext, PreToolDecision, PreToolNext, RUN_CODE_NAME,
    ScheduledToolDispatch, ScheduledToolPreparation, TOOL_ABORTED, TOOL_ABORTED_BEFORE_DISPATCH,
    TOOLS, ToolCallPresenter, ToolConcurrencyClassifier, ToolContentFinalizer, ToolDefinition,
    ToolDispatchExecution, ToolErrorInfo, ToolExecute, ToolExecuteFuture, ToolExecution,
    ToolExecutionFailure, ToolExecutionInput, ToolExecutionMode, ToolExecutionResult,
    ToolExecutionSuccess, ToolExecutionToken, ToolFailure, ToolGuard, ToolOutputDefinition,
    ToolPresentationMode, ToolRestriction, ToolResultPresenter, ToolRunContext, ToolRuntime,
    ToolRuntimeConfig, ToolRuntimeError, install,
};
pub use schema::{
    DefineToolCallPresenter, DefineToolConcurrencyClassifier, DefineToolExecute, DefineToolFuture,
    DefineToolOptions, DefineToolOutput, DefineToolPresentationMeta, DefineToolRender,
    DefineToolResultPresenter, INVALID_ARGS, ToolArgsError, define_tool,
    parameter_schema_spec_to_json_schema, validate_args, value_schema_spec_to_json_schema,
};
pub use testing::{ContentToolFixtureOptions, define_content_tool_fixture};
pub use ts_types::{ToolSdkSchema, json_schema_to_ts, render_tools_sdk};

/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "tools";
/// Tool registry prompt integration requires the system prompt service.
pub const PLUGIN_INJECT: &[&str] = &["systemPrompt"];

/// Builds the Loader-compatible tool registry plugin.
#[must_use]
pub fn plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(
        PLUGIN_NAME,
        PLUGIN_INJECT.iter().copied(),
        |context, config| {
            Box::pin(async move {
                let system_prompt = context
                    .get(seekdeep_system_prompt::SYSTEM_PROMPT)
                    .ok_or_else(|| anyhow::anyhow!("tools requires systemPrompt"))?;
                install(&context, &system_prompt, serde_json::from_value(config)?)?;
                Ok(())
            })
        },
    )
}
