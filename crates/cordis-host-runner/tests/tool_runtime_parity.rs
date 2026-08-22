//! Sandboxed Tool definition, schema view, execution, rendering, and teardown parity.

use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, SessionId};
use seekdeep_tools::{
    TOOLS, ToolExecutionInput, ToolExecutionResult, ToolRuntime, ToolRuntimeConfig,
};
use serde_json::json;

#[tokio::test]
async fn dynamic_tool_executes_and_renders_in_its_worker_then_unregisters_at_stop() {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "tools".to_owned(),
            },
            name: "reverse".to_owned(),
            purpose: "reverse text".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "return { inject: ['tools'], apply(ctx) {",
                        "harness.registerTool(ctx, harness.defineTool({",
                        "name: 'reverse_text', description: 'Reverse a string.',",
                        "parameters: { text: { type: 'string', required: true } },",
                        "output: { schema: { type: 'string' },",
                        "render(_args, value) { return [{ type: 'text', text: value }]; },",
                        "presentationMeta(_args, value) { return { length: value.length }; } },",
                        "async execute(args) { return args.text.split('').reverse().join(''); }",
                        "})); } };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let started = runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(started, DynamicCordisRunResponse::Success { .. }));
    let schema = tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == "reverse_text")
        .unwrap();
    assert_eq!(schema.description, "Reverse a string.");
    assert_eq!(schema.parameters["required"], json!(["text"]));

    let result = tools
        .execute(ToolExecutionInput::new(
            CallId::new("call-1"),
            "reverse_text",
            json!({"text": "stressed"}),
            AbortSignal::default(),
        ))
        .await;
    match result {
        ToolExecutionResult::Success(success) => {
            assert_eq!(success.value, json!("desserts"));
            assert_eq!(success.meta, Some(json!({"length": 8})));
            assert_eq!(
                success.content,
                [ContentBlock::Text {
                    text: "desserts".to_owned()
                }]
            );
        }
        ToolExecutionResult::Failure(failure) => {
            panic!("dynamic tool failed: {failure:?}")
        }
    }

    runner.stop(&session, &defined.plugin_id).await;
    assert!(tools.get("reverse_text", None).is_none());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn tool_facade_returns_schema_only_and_rejects_unmarked_definitions() {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "tools".to_owned(),
            },
            name: "inspect tools".to_owned(),
            purpose: "check facade".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "return { inject: ['tools'], apply(ctx) {",
                        "const schema = ctx.tools.get('missing');",
                        "if (schema !== undefined) throw new Error('unexpected schema');",
                        "ctx.tools.register({ name: 'raw' });",
                        "} };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
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
            if message.contains("accepts only a definition returned by harness.defineTool")
    ));
    assert!(tools.get("raw", None).is_none());
    assert!(context.get(TOOLS).is_some());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn duplicate_dynamic_tool_teaches_stop_then_run_and_leaves_second_plugin_inactive() {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let code = concat!(
        "return { inject: ['tools'], apply(ctx) {",
        "harness.registerTool(ctx, harness.defineTool({",
        "name: 'collision', description: 'Colliding tool.', parameters: {},",
        "output: { schema: { type: 'null' }, render() { return []; } },",
        "async execute() { return null; }",
        "})); } };",
    );
    let define = |prefix: &str| {
        runner
            .define(DynamicCordisDefineRequest {
                session_id: session.clone(),
                plugin: DynamicCordisPluginSelector::New {
                    id_prefix: prefix.to_owned(),
                },
                name: prefix.to_owned(),
                purpose: "collision".to_owned(),
                code: DynamicCordisCode {
                    host: Some(code.to_owned()),
                    client: None,
                },
            })
            .unwrap()
    };
    let first = define("first");
    let second = define("secon");
    assert!(matches!(
        runner
            .run(
                &session,
                &first.plugin_id,
                &first.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    let failed = runner
        .run(
            &session,
            &second.plugin_id,
            &second.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(
        failed,
        DynamicCordisRunResponse::Failure { ref message, .. }
            if message.contains("first cordis_stop that package's id")
    ));
    assert!(runner.inventory()[1].active_run.is_none());
    runner.stop(&session, &first.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn tool_get_returns_read_only_schema_metadata_without_the_execute_function() {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let producer = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "tools".to_owned(),
            },
            name: "producer".to_owned(),
            purpose: "register schema".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "return { inject: ['tools'], apply(ctx) {",
                        "harness.registerTool(ctx, harness.defineTool({ name: 'visible_schema',",
                        "description: 'Visible schema.', parameters: {},",
                        "output: { schema: { type: 'null' }, render: () => [] },",
                        "execute: async () => null })); } };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    assert!(matches!(
        runner
            .run(
                &session,
                &producer.plugin_id,
                &producer.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    let inspector = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "viewr".to_owned(),
            },
            name: "inspector".to_owned(),
            purpose: "read schema".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "let view; harness.handle('view', async () => view);",
                        "return { inject: ['tools'], apply(ctx) {",
                        "const schema = ctx.tools.get('visible_schema');",
                        "view = { name: schema.name, description: schema.description,",
                        "execute: typeof schema.execute, missing: ctx.tools.get('missing') === undefined };",
                        "} };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let run = runner
        .run(
            &session,
            &inspector.plugin_id,
            &inspector.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let run_id = match run {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failure @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected inspector failure: {failure:?}")
        }
    };
    assert_eq!(
        runner
            .invoke(
                &inspector.plugin_id,
                &run_id,
                "view",
                serde_json::json!(null)
            )
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success {
            value: serde_json::json!({
                "name": "visible_schema",
                "description": "Visible schema.",
                "execute": "undefined",
                "missing": true,
            })
        }
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn merge_extensible_content_shape_accepts_an_incomplete_known_tag_as_unknown() {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "merge".to_owned(),
            },
            name: "merge".to_owned(),
            purpose: "unknown content".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "return { inject: ['tools'], apply(ctx) {",
                        "harness.registerTool(ctx, harness.defineTool({ name: 'merge_block',",
                        "description: 'Return merge block.', parameters: {},",
                        "output: { schema: { type: 'null' }, render: () => [{ type: 'text' }] },",
                        "execute: async () => null })); } };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
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
            CallId::new("merge"),
            "merge_block",
            serde_json::json!({}),
            AbortSignal::default(),
        ))
        .await;
    assert!(
        matches!(
            result,
            ToolExecutionResult::Success(ref success)
                if success.content == [ContentBlock::Unknown {
                    block_type: "text".to_owned(),
                    fields: serde_json::Map::new(),
                }]
        ),
        "{result:?}"
    );
    context.fiber().dispose().await.unwrap();
}
