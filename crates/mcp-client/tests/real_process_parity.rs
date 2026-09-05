//! Real stdio MCP process, crash recovery, and declarative Loader composition.

use std::{collections::BTreeMap, sync::Arc};

use seekdeep_cordis::{Context, Plugin};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_loader::PluginCatalog;
use seekdeep_mcp_client::{Config, ReconnectConfig, apply, plugin, public_tool_name};
use seekdeep_tools::{ToolExecutionInput, ToolRuntime, ToolRuntimeConfig};
use serde_json::{Value, json};

fn fixture() -> String {
    env!("CARGO_BIN_EXE_seekdeep-mcp-server-fixture").to_owned()
}

fn config(server_name: &str) -> Config {
    Config::Stdio {
        server_name: server_name.to_owned(),
        command: fixture(),
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: String::new(),
        tool_call_timeout_ms: 5_000.0,
        fail_on_startup_error: true,
        reconnect: Some(ReconnectConfig {
            enabled: Some(true),
            initial_delay_ms: Some(25.0),
            max_delay_ms: Some(100.0),
            max_attempts: Some(20.0),
        }),
    }
}

fn registry() -> (Context, Arc<ToolRuntime>) {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    (context, tools)
}

fn input(name: &str, arguments: Value) -> ToolExecutionInput {
    ToolExecutionInput::new(
        CallId::new(format!("real-{name}")),
        name,
        arguments,
        AbortSignal::default(),
    )
}

fn text(result: &seekdeep_tools::ToolExecutionResult) -> &str {
    let Some(ContentBlock::Text { text }) = result.content().first() else {
        panic!("expected one text result")
    };
    text
}

#[tokio::test]
async fn real_stdio_discovers_executes_recovers_and_unregisters_exactly_once() {
    let (context, tools) = registry();
    let connection = apply(&context, config("fixture")).await.unwrap();
    for name in ["add", "greet", "fail", "image", "crash"] {
        assert!(tools.get(&format!("mcp__fixture__{name}"), None).is_some());
    }
    let dotted = public_tool_name("fixture", "admin.reset");
    assert!(tools.get(&dotted, None).is_some());

    let add = tools
        .execute(input("mcp__fixture__add", json!({"a":2,"b":3})))
        .await;
    assert!(!add.is_error());
    assert_eq!(text(&add), "5");
    let greet = tools
        .execute(input("mcp__fixture__greet", json!({"name":"World"})))
        .await;
    assert_eq!(text(&greet), "Hello, World!");
    let dotted_result = tools.execute(input(&dotted, json!({}))).await;
    assert_eq!(text(&dotted_result), "reset done");
    let image = tools.execute(input("mcp__fixture__image", json!({}))).await;
    assert!(text(&image).contains("[image: image/png, content discarded]"));
    let failed = tools.execute(input("mcp__fixture__fail", json!({}))).await;
    assert!(failed.is_error());
    assert_eq!(failed.error().unwrap().message, "Something went wrong");

    let crash = tools.execute(input("mcp__fixture__crash", json!({}))).await;
    assert!(!crash.is_error());
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let recovered = tools
                .execute(input("mcp__fixture__add", json!({"a":20,"b":22})))
                .await;
            if !recovered.is_error() && text(&recovered) == "42" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("real child recovered after crash");
    assert_eq!(
        tools
            .schemas(None)
            .iter()
            .filter(|schema| schema.name == "mcp__fixture__add")
            .count(),
        1
    );

    connection.dispose().await.unwrap();
    assert!(tools.get("mcp__fixture__add", None).is_none());
    context.fiber().dispose().await.unwrap();
}

fn tools_plugin() -> Plugin {
    Plugin::new("tools", std::iter::empty::<&str>(), |context, _| {
        Box::pin(async move {
            let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default())?;
            tools.provide(&context)?;
            Ok(())
        })
    })
}

fn loader_yaml() -> String {
    format!(
        concat!(
            "- id: tools\n",
            "  name: tools\n",
            "- id: fixture\n",
            "  name: seekdeep-mcp-client\n",
            "  config:\n",
            "    transport: stdio\n",
            "    serverName: loader\n",
            "    command: {}\n",
            "    failOnStartupError: true\n",
            "    reconnect:\n",
            "      enabled: false\n",
        ),
        serde_json::to_string(&fixture()).unwrap()
    )
}

#[tokio::test]
async fn loader_mounts_real_plugin_path_and_releases_namespace_for_reload() {
    let context = Context::new();
    let catalog = PluginCatalog::new();
    catalog.register_named("tools", tools_plugin()).unwrap();
    catalog
        .register_named("seekdeep-mcp-client", plugin())
        .unwrap();
    let first = catalog.load_yaml(&context, &loader_yaml()).await.unwrap();
    let tools = context.get(seekdeep_tools::TOOLS).unwrap();
    assert!(tools.get("mcp__loader__add", None).is_some());
    let result = tools
        .execute(input("mcp__loader__add", json!({"a":40,"b":2})))
        .await;
    assert_eq!(text(&result), "42");
    first.dispose().await.unwrap();
    assert!(context.get(seekdeep_tools::TOOLS).is_none());

    let second = catalog.load_yaml(&context, &loader_yaml()).await.unwrap();
    assert!(
        context
            .get(seekdeep_tools::TOOLS)
            .unwrap()
            .get("mcp__loader__add", None)
            .is_some()
    );
    second.dispose().await.unwrap();
}
