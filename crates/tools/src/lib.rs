//! Tool schemas, registration, policy, scheduling, execution, and presentation.

/// The enforced lossless JSON Schema subset.
pub mod json_schema;
/// Scoped tool registry and staged execution pipeline.
pub mod runtime;
/// Author-facing value-schema DSL.
pub mod schema;
/// TypeScript Code Mode SDK projection.
pub mod ts_types;

pub use json_schema::{
    JsonSchemaError, JsonSchemaNode, ObjectJsonSchema, assert_object_json_schema,
    assert_supported_json_schema, check_object_json_schema, check_supported_json_schema,
    validate_json_schema_value, validate_json_schema_value_at,
};
pub use runtime::{
    ExecuteToolNext, PostToolDecision, PostToolNext, PreToolDecision, PreToolNext, TOOL_ABORTED,
    TOOL_ABORTED_BEFORE_DISPATCH, ToolDefinition, ToolErrorInfo, ToolExecution,
    ToolExecutionFailure, ToolExecutionInput, ToolExecutionMode, ToolExecutionResult,
    ToolExecutionSuccess, ToolExecutionToken, ToolFailure, ToolGuard, ToolOutputDefinition,
    ToolPresentationMode, ToolRestriction, ToolRuntime, ToolRuntimeConfig, ToolRuntimeError,
};
pub use schema::{
    ToolArgsError, parameter_schema_spec_to_json_schema, validate_args,
    value_schema_spec_to_json_schema,
};
pub use ts_types::{ToolSdkSchema, json_schema_to_ts, render_tools_sdk};
