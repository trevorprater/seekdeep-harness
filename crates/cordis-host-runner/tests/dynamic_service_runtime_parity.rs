//! Cross-package function Service composition through Rust-owned worker bridges.

use std::time::Duration;

use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, SessionId};
use seekdeep_tools::{ToolExecutionInput, ToolExecutionResult, ToolRuntime, ToolRuntimeConfig};
use serde_json::json;

async fn eventually(mut condition: impl FnMut() -> bool, message: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

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
            purpose: "composition".to_owned(),
            code: DynamicCordisCode {
                host: Some(body.to_owned()),
                client: None,
            },
        })
        .unwrap()
}

async fn run(
    runner: &DynamicCordisRunner,
    session: &SessionId,
    receipt: &seekdeep_cordis_host_runner::DynamicCordisDefineReceipt,
) {
    assert!(matches!(
        runner
            .run(
                session,
                &receipt.plugin_id,
                &receipt.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
}

#[tokio::test]
async fn consumer_calls_provider_method_and_tracks_provider_stop_and_restart() {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let provider = define(
        &runner,
        &session,
        "clock",
        "return { apply(ctx) { ctx.provide('greeter', { greet: name => 'hi ' + name }); } };",
    );
    let consumer = define(
        &runner,
        &session,
        "greet",
        concat!(
            "return { inject: ['greeter', 'tools'], apply(ctx) {",
            "harness.registerTool(ctx, harness.defineTool({",
            "name: 'greet_name', description: 'Greet a name.',",
            "parameters: { name: { type: 'string', required: true } },",
            "output: { schema: { type: 'string' }, render(_args, value) {",
            "return [{ type: 'text', text: value }]; } },",
            "async execute(args) { return ctx.greeter.greet(args.name); }",
            "})); } };",
        ),
    );
    run(&runner, &session, &provider).await;
    run(&runner, &session, &consumer).await;

    let result = tools
        .execute(ToolExecutionInput::new(
            CallId::new("call-1"),
            "greet_name",
            json!({"name": "harness"}),
            AbortSignal::default(),
        ))
        .await;
    assert!(matches!(
        result,
        ToolExecutionResult::Success(ref success)
            if success.value == json!("hi harness")
                && success.content == [ContentBlock::Text { text: "hi harness".to_owned() }]
    ));

    runner.stop(&session, &provider.plugin_id).await;
    eventually(
        || tools.get("greet_name", None).is_none(),
        "consumer Tool survived provider stop",
    )
    .await;
    run(&runner, &session, &provider).await;
    eventually(
        || tools.get("greet_name", None).is_some(),
        "consumer Tool did not return after provider restart",
    )
    .await;
    runner.stop(&session, &consumer.plugin_id).await;
    assert!(tools.get("greet_name", None).is_none());
    assert!(context.has_named("greeter"));
    runner.stop(&session, &provider.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn consumer_first_parks_without_a_tool_then_activates_when_provider_appears() {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let consumer = define(
        &runner,
        &session,
        "greet",
        concat!(
            "return { inject: ['greeter', 'tools'], apply(ctx) {",
            "harness.registerTool(ctx, harness.defineTool({",
            "name: 'late_greet', description: 'Greet a name.',",
            "parameters: { name: { type: 'string', required: true } },",
            "output: { schema: { type: 'string' }, render(_args, value) {",
            "return [{ type: 'text', text: value }]; } },",
            "async execute(args) { return ctx.greeter.greet(args.name); }",
            "})); } };",
        ),
    );
    let waiting = runner
        .run(
            &session,
            &consumer.plugin_id,
            &consumer.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(
        waiting,
        DynamicCordisRunResponse::Success { ref waiting_for, .. }
            if waiting_for == &["greeter"]
    ));
    assert!(tools.get("late_greet", None).is_none());

    let provider = define(
        &runner,
        &session,
        "clock",
        "return { apply(ctx) { ctx.provide('greeter', { greet: name => 'late ' + name }); } };",
    );
    run(&runner, &session, &provider).await;
    eventually(
        || tools.get("late_greet", None).is_some(),
        "consumer did not activate after provider appeared",
    )
    .await;
    let result = tools
        .execute(ToolExecutionInput::new(
            CallId::new("call-late"),
            "late_greet",
            json!({"name": "arrival"}),
            AbortSignal::default(),
        ))
        .await;
    assert!(matches!(
        result,
        ToolExecutionResult::Success(ref success) if success.value == json!("late arrival")
    ));
    runner.stop(&session, &consumer.plugin_id).await;
    assert!(context.has_named("greeter"));
    runner.stop(&session, &provider.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}
