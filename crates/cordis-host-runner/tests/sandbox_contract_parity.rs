//! Node traps, plugin-shape teaching, codecs, Tool declarations, and realm parity.

use seekdeep_cordis::{Context, ServiceKey};
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
};
use seekdeep_llm::{AbortSignal, CallId, SessionId};
use seekdeep_tools::{ToolExecutionInput, ToolExecutionResult, ToolRuntime, ToolRuntimeConfig};
use serde_json::{Value, json};

const CODEC: ServiceKey<Value> = ServiceKey::new("codecResult");
const LEAK: ServiceKey<Value> = ServiceKey::new("leakResult");

fn define(
    runner: &DynamicCordisRunner,
    session: &SessionId,
    prefix: &str,
    body: &str,
) -> seekdeep_cordis_host_runner::DynamicCordisDefineReceipt {
    runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: prefix.to_owned(),
            },
            name: prefix.to_owned(),
            purpose: "sandbox contract".to_owned(),
            code: DynamicCordisCode {
                host: Some(body.to_owned()),
                client: None,
            },
        })
        .unwrap()
}

async fn run_failure(
    runner: &DynamicCordisRunner,
    session: &SessionId,
    receipt: &seekdeep_cordis_host_runner::DynamicCordisDefineReceipt,
) -> String {
    match runner
        .run(
            session,
            &receipt.plugin_id,
            &receipt.package_id,
            DynamicCordisRunMode::Run,
        )
        .await
    {
        DynamicCordisRunResponse::Failure { message, .. } => message,
        success @ DynamicCordisRunResponse::Success { .. } => {
            panic!("expected sandbox failure, got {success:?}")
        }
    }
}

#[tokio::test]
async fn node_api_traps_name_the_cordis_alternative_and_leave_no_run() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 50);
    let session = SessionId::new("session-a");
    for (prefix, invocation, expected, redirect) in [
        (
            "req",
            "require('fs')",
            "require is not available",
            "inject: ['fs']",
        ),
        (
            "timer",
            "setTimeout(() => {}, 5)",
            "setTimeout is not available",
            "ctx.timeout / ctx.interval",
        ),
        (
            "fetch",
            "fetch('https://example.com')",
            "fetch is not available",
            "ctx.web",
        ),
    ] {
        let defined = define(
            &runner,
            &session,
            prefix,
            &format!("{invocation}; return {{ apply() {{}} }};"),
        );
        let message = run_failure(&runner, &session, &defined).await;
        assert!(message.contains(expected), "{message}");
        assert!(message.contains(redirect), "{message}");
        assert!(
            runner
                .registry()
                .get(&defined.plugin_id)
                .unwrap()
                .lock()
                .run
                .is_none()
        );
    }
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn isolated_globals_utf8_codecs_and_text_encoders_stay_inside_each_worker() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let leaking = define(
        &runner,
        &session,
        "leak",
        "globalThis.__seekdeep_leak = 'leaked'; return { apply() {} };",
    );
    let codec = define(
        &runner,
        &session,
        "codec",
        concat!(
            "const round = atob(btoa('hé'));",
            "const bytes = new TextEncoder().encode(round);",
            "const decoded = new TextDecoder().decode(bytes);",
            "return { apply(ctx) {",
            "ctx.provide('codecResult', { decoded, process: typeof process, buffer: typeof Buffer });",
            "ctx.provide('leakResult', typeof globalThis.__seekdeep_leak);",
            "} };",
        ),
    );
    assert!(matches!(
        runner
            .run(
                &session,
                &leaking.plugin_id,
                &leaking.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    let codec_run = runner
        .run(
            &session,
            &codec.plugin_id,
            &codec.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(
        matches!(codec_run, DynamicCordisRunResponse::Success { .. }),
        "{codec_run:?}"
    );
    assert_eq!(
        context.get(CODEC).as_deref(),
        Some(&json!({"decoded": "hé", "process": "undefined", "buffer": "undefined"}))
    );
    assert_eq!(context.get(LEAK).as_deref(), Some(&json!("undefined")));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn plugin_shape_runtime_errors_and_synchronous_timeout_keep_the_plugin_inactive() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 1);
    let session = SessionId::new("session-a");
    for (prefix, body, expected) in [
        (
            "throw",
            "throw new Error('boom in sandbox')",
            "boom in sandbox",
        ),
        ("plain", "throw 'plain-string-throw'", "plain-string-throw"),
        ("number", "return 42", "must return a Plugin"),
        (
            "forget",
            "const plugin = ctx => {}",
            "did you forget `return`?",
        ),
        (
            "apply",
            "return { apply() { throw new Error('apply exploded'); } };",
            "apply exploded",
        ),
        (
            "syntax",
            "throw new SyntaxError('user-crafted')",
            "user-crafted",
        ),
        ("loop", "while (true) {}", "timed out"),
    ] {
        let defined = define(&runner, &session, prefix, body);
        let message = run_failure(&runner, &session, &defined).await;
        assert!(message.contains(expected), "{prefix}: {message}");
        assert!(runner.inventory().last().unwrap().active_run.is_none());
    }
    let null = define(&runner, &session, "nullx", "throw null");
    assert!(!run_failure(&runner, &session, &null).await.is_empty());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn tool_declaration_errors_and_renderer_preview_match_the_teaching_contract() {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    for (prefix, expression, expected) in [
        ("obj", "42", "options must be an object"),
        (
            "out",
            "{ parameters: {} }",
            "output must declare { schema, render, presentationMeta? }",
        ),
        (
            "rend",
            "{ parameters: {}, output: { schema: { type: 'json' } }, execute: async () => null }",
            "output.render must be a function",
        ),
        (
            "exec",
            "{ parameters: {}, output: { schema: { type: 'json' }, render: () => [] }, execute: true }",
            "execute must be a function",
        ),
        (
            "meta",
            "{ parameters: {}, output: { schema: { type: 'json' }, render: () => [], presentationMeta: true }, execute: async () => null }",
            "output.presentationMeta must be a function",
        ),
    ] {
        let body = format!("harness.defineTool({expression}); return {{ apply() {{}} }};");
        let defined = define(&runner, &session, prefix, &body);
        let message = run_failure(&runner, &session, &defined).await;
        assert!(message.contains(expected), "{prefix}: {message}");
    }

    let invalid = define(
        &runner,
        &session,
        "blob",
        concat!(
            "return { inject: ['tools'], apply(ctx) {",
            "harness.registerTool(ctx, harness.defineTool({ name: 'bad_render',",
            "description: 'bad renderer', parameters: {},",
            "output: { schema: { type: 'string' }, render: () => ['x'.repeat(500)] },",
            "execute: async () => 'ok' })); } };",
        ),
    );
    assert!(matches!(
        runner
            .run(
                &session,
                &invalid.plugin_id,
                &invalid.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    let result = tools
        .execute(ToolExecutionInput::new(
            CallId::new("render"),
            "bad_render",
            json!({}),
            AbortSignal::default(),
        ))
        .await;
    assert!(matches!(
        result,
        ToolExecutionResult::Failure(ref failure)
            if failure.error.message.contains("output.render returned [\"xxx")
                && failure.error.message.contains('…')
    ));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn tool_arguments_and_worker_values_share_array_and_object_instanceof_semantics() {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = define(
        &runner,
        &session,
        "realm",
        concat!(
            "return { inject: ['tools'], apply(ctx) {",
            "harness.registerTool(ctx, harness.defineTool({ name: 'realm_check',",
            "description: 'check values',",
            "parameters: { items: { type: 'array', required: true, items: { type: 'string' } } },",
            "output: { schema: { type: 'object' }, render: () => [] },",
            "execute: async args => ({ hostArray: args.items instanceof Array,",
            "hostObject: args instanceof Object, vmArray: [] instanceof Array,",
            "vmObject: ({}) instanceof Object }) })); } };",
        ),
    );
    assert!(matches!(
        runner
            .run(
                &session,
                &defined.plugin_id,
                &defined.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    let result = tools
        .execute(ToolExecutionInput::new(
            CallId::new("realm"),
            "realm_check",
            json!({"items": ["a"]}),
            AbortSignal::default(),
        ))
        .await;
    assert!(matches!(
        result,
        ToolExecutionResult::Success(ref success)
            if success.value == json!({
                "hostArray": true,
                "hostObject": true,
                "vmArray": true,
                "vmObject": true,
            })
    ));
    context.fiber().dispose().await.unwrap();
}
