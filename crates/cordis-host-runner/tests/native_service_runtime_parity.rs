//! Explicit Rust Service adapters, optional lookup, async data, and Context-return denial.

use std::sync::Arc;

use futures::future::BoxFuture;
use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
};
use seekdeep_llm::{AbortSignal, CallId, SessionId};
use seekdeep_loader::{
    SandboxServiceDispatcher, SandboxServiceRegistration, SandboxServiceRegistry,
};
use seekdeep_tools::{ToolExecutionInput, ToolExecutionResult, ToolRuntime, ToolRuntimeConfig};
use serde_json::{Value, json};

#[derive(Debug)]
struct NativeService;

#[derive(Debug)]
struct InlineDispatcher;

impl SandboxServiceDispatcher for InlineDispatcher {
    fn dispatch(&self, future: BoxFuture<'static, anyhow::Result<Value>>) -> anyhow::Result<Value> {
        futures::executor::block_on(future)
    }
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
            purpose: "native Service adapter".to_owned(),
            code: DynamicCordisCode {
                host: Some(body.to_owned()),
                client: None,
            },
        })
        .unwrap()
}

#[tokio::test]
async fn declared_and_optional_native_service_methods_return_only_adapter_owned_json() {
    let context = Context::new();
    context
        .provide_named("hostAsync", Arc::new(NativeService))
        .unwrap();
    let adapters = SandboxServiceRegistry::install(&context, Arc::new(InlineDispatcher)).unwrap();
    adapters
        .register(
            &context,
            SandboxServiceRegistration::new("hostAsync", json!({"kind": "native"})).method(
                "grab",
                Arc::new(|_| Box::pin(async { Ok(json!("host-fetched")) })),
            ),
        )
        .unwrap();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let declared = define(
        &runner,
        &session,
        "fetch",
        concat!(
            "return { inject: ['hostAsync', 'tools'], apply(ctx) {",
            "harness.registerTool(ctx, harness.defineTool({ name: 'native_fetch',",
            "description: 'Fetch native data.', parameters: {},",
            "output: { schema: { type: 'string' }, render: () => [] },",
            "execute: async () => ctx.hostAsync.grab() })); } };",
        ),
    );
    let optional = define(
        &runner,
        &session,
        "optio",
        concat!(
            "return { inject: ['tools'], apply(ctx) {",
            "harness.registerTool(ctx, harness.defineTool({ name: 'optional_fetch',",
            "description: 'Fetch optional native data.', parameters: {},",
            "output: { schema: { type: 'string' }, render: () => [] },",
            "execute: async () => ctx.get('hostAsync').grab() })); } };",
        ),
    );
    for defined in [&declared, &optional] {
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
    }
    for (call, tool) in [("declared", "native_fetch"), ("optional", "optional_fetch")] {
        let result = tools
            .execute(ToolExecutionInput::new(
                CallId::new(call),
                tool,
                json!({}),
                AbortSignal::default(),
            ))
            .await;
        assert!(matches!(
            result,
            ToolExecutionResult::Success(ref success) if success.value == json!("host-fetched")
        ));
    }
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn adapter_failure_blocks_a_context_escape_before_any_registration_lands() {
    let context = Context::new();
    context
        .provide_named("escape", Arc::new(NativeService))
        .unwrap();
    let adapters = SandboxServiceRegistry::install(&context, Arc::new(InlineDispatcher)).unwrap();
    adapters
        .register(
            &context,
            SandboxServiceRegistration::new("escape", json!({})).method(
                "ctx",
                Arc::new(|_| {
                    Box::pin(async {
                        anyhow::bail!(
                            "service \"escape\" returned a cordis Context, which the sandbox does not expose"
                        )
                    })
                }),
            ),
        )
        .unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = define(
        &runner,
        &session,
        "guard",
        "return { inject: ['escape'], apply(ctx) { ctx.escape.ctx(); } };",
    );
    let failed = runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(
        failed,
        DynamicCordisRunResponse::Failure { ref message, .. }
            if message.contains("returned a cordis Context, which the sandbox does not expose")
    ));
    assert!(runner.inventory()[0].active_run.is_none());
    context.fiber().dispose().await.unwrap();
}
