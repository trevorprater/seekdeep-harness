//! Source-parity oracle for fail-closed per-call execution classification.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_tools::{
    ContentToolFixtureOptions, ToolConcurrencyClassifier, ToolDefinition, ToolExecutionInput,
    ToolExecutionMode, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
    assert_supported_json_schema, define_content_tool_fixture,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

fn setup() -> (Context, Arc<ToolRuntime>) {
    let context = Context::new();
    let runtime = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
    (context, runtime)
}

fn input(name: &str, arguments: Value) -> ToolExecutionInput {
    ToolExecutionInput::new(CallId::new("c1"), name, arguments, AbortSignal::default())
}

fn raw_definition(name: &str, classifier: Option<ToolConcurrencyClassifier>) -> ToolDefinition {
    let mut definition = ToolDefinition::new(
        name,
        name,
        Map::from_iter([
            ("type".to_owned(), json!("object")),
            ("properties".to_owned(), json!({})),
        ]),
        ToolOutputDefinition::new(
            Arc::new(assert_supported_json_schema(json!({"type": "null"})).expect("schema")),
            Arc::new(|_, _| Ok(Vec::new())),
        ),
        Arc::new(|_, _| Box::pin(async { Ok(Value::Null) })),
    );
    definition.is_concurrency_safe = classifier;
    definition
}

fn empty_fixture(name: &str) -> ContentToolFixtureOptions<Value> {
    ContentToolFixtureOptions::new(
        name,
        name,
        json!({}),
        Arc::new(|_: Value, _| Box::pin(async { Ok(Vec::<ContentBlock>::new()) })),
    )
}

#[test]
fn returns_parallel_only_for_an_explicit_true_classifier() {
    let (context, runtime) = setup();
    runtime
        .register(
            &context,
            define_content_tool_fixture(empty_fixture("safe").concurrency_safe(Arc::new(|_| true)))
                .expect("fixture"),
        )
        .expect("register");
    assert_eq!(
        runtime.execution_mode(&input("safe", json!({}))),
        ToolExecutionMode::Parallel
    );
}

#[test]
fn defaults_to_exclusive_without_a_classifier() {
    let (context, runtime) = setup();
    runtime
        .register(
            &context,
            define_content_tool_fixture(empty_fixture("plain")).expect("fixture"),
        )
        .expect("register");
    assert_eq!(
        runtime.execution_mode(&input("plain", json!({}))),
        ToolExecutionMode::Exclusive
    );
}

#[test]
fn returns_exclusive_for_an_unknown_tool() {
    let (_, runtime) = setup();
    assert_eq!(
        runtime.execution_mode(&input("nonexistent", json!({}))),
        ToolExecutionMode::Exclusive
    );
}

#[derive(Deserialize)]
struct ModeArgs {
    mode: String,
}

#[test]
fn classifier_may_choose_by_validated_arguments() {
    let (context, runtime) = setup();
    let fixture = ContentToolFixtureOptions::new(
        "rw",
        "read or write",
        json!({"mode": {"type": "string", "required": true}}),
        Arc::new(|_: ModeArgs, _| Box::pin(async { Ok(Vec::<ContentBlock>::new()) })),
    )
    .concurrency_safe(Arc::new(|args| args.mode == "read"));
    runtime
        .register(
            &context,
            define_content_tool_fixture(fixture).expect("fixture"),
        )
        .expect("register");
    assert_eq!(
        runtime.execution_mode(&input("rw", json!({"mode": "read"}))),
        ToolExecutionMode::Parallel
    );
    assert_eq!(
        runtime.execution_mode(&input("rw", json!({"mode": "write"}))),
        ToolExecutionMode::Exclusive
    );
}

#[test]
fn invalid_typed_fixture_arguments_are_exclusive_without_panicking() {
    let (context, runtime) = setup();
    let fixture = ContentToolFixtureOptions::new(
        "needs-mode",
        "requires mode",
        json!({"mode": {"type": "string", "required": true}}),
        Arc::new(|_: ModeArgs, _| Box::pin(async { Ok(Vec::<ContentBlock>::new()) })),
    )
    .concurrency_safe(Arc::new(|_| true));
    runtime
        .register(
            &context,
            define_content_tool_fixture(fixture).expect("fixture"),
        )
        .expect("register");
    assert_eq!(
        runtime.execution_mode(&input("needs-mode", json!({}))),
        ToolExecutionMode::Exclusive
    );
}

#[test]
fn throwing_raw_classifier_is_exclusive() {
    let (context, runtime) = setup();
    runtime
        .register(
            &context,
            raw_definition("thrower", Some(Arc::new(|_| panic!("boom")))),
        )
        .expect("register");
    assert_eq!(
        runtime.execution_mode(&input("thrower", json!({}))),
        ToolExecutionMode::Exclusive
    );
}

#[test]
fn non_boolean_classifier_results_are_unrepresentable() {
    fn accepts_classifier(_: ToolConcurrencyClassifier) {}
    accepts_classifier(Arc::new(|_| true));
    // Unlike JavaScript's forged truthy string, the Rust callback's return type
    // is statically `bool`; only exact true can opt in.
}

#[test]
fn raw_classifier_receives_the_exact_parsed_arguments() {
    let (context, runtime) = setup();
    let seen = Arc::new(Mutex::new(None));
    let capture = seen.clone();
    runtime
        .register(
            &context,
            raw_definition(
                "raw-safe",
                Some(Arc::new(move |arguments| {
                    *capture.lock() = Some(arguments.clone());
                    true
                })),
            ),
        )
        .expect("register");
    assert_eq!(
        runtime.execution_mode(&input("raw-safe", json!({"anything": 1}))),
        ToolExecutionMode::Parallel
    );
    assert_eq!(*seen.lock(), Some(json!({"anything": 1})));
}

#[test]
fn classifier_never_reaches_model_facing_schemas() {
    let (context, runtime) = setup();
    runtime
        .register(
            &context,
            define_content_tool_fixture(
                ContentToolFixtureOptions::new(
                    "safe",
                    "parallel-safe",
                    json!({"x": {"type": "string", "required": true}}),
                    Arc::new(|_: Value, _| Box::pin(async { Ok(Vec::<ContentBlock>::new()) })),
                )
                .concurrency_safe(Arc::new(|_| true)),
            )
            .expect("fixture"),
        )
        .expect("register");
    let schema = serde_json::to_value(&runtime.schemas(None)[0]).expect("schema");
    let mut keys = schema
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, ["description", "name", "parameters"]);
}

#[test]
fn execution_mode_has_the_object_tagged_union_wire_contract() {
    assert_eq!(
        serde_json::to_value(ToolExecutionMode::Parallel).expect("parallel"),
        json!({"kind": "parallel"})
    );
    assert_eq!(
        serde_json::to_value(ToolExecutionMode::Exclusive).expect("exclusive"),
        json!({"kind": "exclusive"})
    );
}
