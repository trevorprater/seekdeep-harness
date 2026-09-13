//! Plugin export, startup readiness, reservation, and strict-failure parity.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use seekdeep_cordis::Context;
use seekdeep_llm::AbortSignal;
use seekdeep_mcp_client::{
    Config, ConnectionRuntime, INJECT, McpClient, McpClientFactory, McpClientSignals, McpTiming,
    McpTool, McpToolPage, NAME, apply_with_runtime, plugin,
};
use seekdeep_tools::{
    ToolDefinition, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
    assert_supported_json_schema,
};
use serde_json::{Map, Value, json};

#[derive(Debug, Default)]
struct ImmediateTiming;

#[async_trait]
impl McpTiming for ImmediateTiming {
    fn now_ms(&self) -> f64 {
        0.0
    }

    async fn sleep(&self, _milliseconds: f64) {
        futures::future::pending::<()>().await;
    }
}

#[derive(Debug)]
struct StartupClient {
    started: AtomicBool,
    release: tokio::sync::Notify,
    blocked: bool,
    connect_error: Option<String>,
    tools: Vec<McpTool>,
    signals: Arc<McpClientSignals>,
}

impl StartupClient {
    fn success(name: &str) -> Arc<Self> {
        Arc::new(Self {
            started: AtomicBool::new(false),
            release: tokio::sync::Notify::new(),
            blocked: false,
            connect_error: None,
            tools: vec![tool(name)],
            signals: Arc::new(McpClientSignals::default()),
        })
    }

    fn blocked(name: &str) -> Arc<Self> {
        Arc::new(Self {
            started: AtomicBool::new(false),
            release: tokio::sync::Notify::new(),
            blocked: true,
            connect_error: None,
            tools: vec![tool(name)],
            signals: Arc::new(McpClientSignals::default()),
        })
    }

    fn failed(message: &str) -> Arc<Self> {
        Arc::new(Self {
            started: AtomicBool::new(false),
            release: tokio::sync::Notify::new(),
            blocked: false,
            connect_error: Some(message.to_owned()),
            tools: Vec::new(),
            signals: Arc::new(McpClientSignals::default()),
        })
    }

    fn release(&self) {
        self.release.notify_waiters();
    }
}

#[async_trait]
impl McpClient for StartupClient {
    async fn connect(&self) -> anyhow::Result<()> {
        self.started.store(true, Ordering::Release);
        if self.blocked {
            self.release.notified().await;
        }
        match &self.connect_error {
            Some(error) => Err(anyhow::anyhow!(error.clone())),
            None => Ok(()),
        }
    }

    async fn list_tools(&self, _cursor: Option<&str>) -> anyhow::Result<McpToolPage> {
        Ok(McpToolPage {
            tools: self.tools.clone(),
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        _raw_name: &str,
        _arguments: Map<String, Value>,
        _signal: AbortSignal,
    ) -> anyhow::Result<Value> {
        Ok(json!({"content":[]}))
    }

    async fn close(&self) -> anyhow::Result<()> {
        self.signals.close();
        Ok(())
    }

    fn closed_signal(&self) -> AbortSignal {
        self.signals.closed_signal()
    }

    fn list_change_generation(&self) -> u64 {
        self.signals.list_change_generation()
    }

    async fn wait_list_change(&self, after: u64) {
        self.signals.wait_list_change(after).await;
    }
}

#[derive(Debug)]
struct OneClientFactory(Arc<StartupClient>);

#[async_trait]
impl McpClientFactory for OneClientFactory {
    async fn create(&self, _config: &Config) -> anyhow::Result<Arc<dyn McpClient>> {
        Ok(Arc::clone(&self.0) as Arc<dyn McpClient>)
    }
}

fn runtime(client: Arc<StartupClient>) -> ConnectionRuntime {
    ConnectionRuntime {
        factory: Arc::new(OneClientFactory(client)),
        timing: Arc::new(ImmediateTiming),
    }
}

fn config(server_name: &str, fatal: bool) -> Config {
    Config::Stdio {
        server_name: server_name.to_owned(),
        command: "fixture".to_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: String::new(),
        tool_call_timeout_ms: 60_000.0,
        fail_on_startup_error: fatal,
        reconnect: Some(seekdeep_mcp_client::ReconnectConfig {
            enabled: Some(false),
            ..seekdeep_mcp_client::ReconnectConfig::default()
        }),
    }
}

fn tool(name: &str) -> McpTool {
    McpTool {
        name: name.to_owned(),
        description: Some("remote".to_owned()),
        input_schema: json!({"type":"object"}),
        output_schema: None,
        execution: None,
    }
}

fn registry() -> (Context, Arc<ToolRuntime>) {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    (context, tools)
}

#[test]
fn namespace_plugin_keeps_name_inject_and_validator_shape() {
    let definition = plugin();
    assert_eq!(definition.name(), NAME);
    assert_eq!(definition.inject(), INJECT);
    assert_eq!(NAME, "mcp-client");
    assert_eq!(INJECT, ["tools"]);
}

#[tokio::test]
async fn activation_waits_for_connect_and_discovery_before_tools_are_visible() {
    let (context, tools) = registry();
    let client = StartupClient::blocked("remote");
    let apply_context = context.clone();
    let apply_client = Arc::clone(&client);
    let applying = tokio::spawn(async move {
        apply_with_runtime(&apply_context, config("srv", false), runtime(apply_client)).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !client.started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!applying.is_finished());
    assert!(tools.get("mcp__srv__remote", None).is_none());
    client.release();
    let handle = applying.await.unwrap().unwrap();
    assert!(tools.get("mcp__srv__remote", None).is_some());
    handle.dispose().await.unwrap();
    assert!(tools.get("mcp__srv__remote", None).is_none());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn duplicate_names_are_root_scoped_and_leave_the_first_instance_intact() {
    let (context, tools) = registry();
    let first = apply_with_runtime(
        &context,
        config("srv", false),
        runtime(StartupClient::success("remote")),
    )
    .await
    .unwrap();
    let error = apply_with_runtime(
        &context,
        config("srv", false),
        runtime(StartupClient::success("other")),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("serverName \"srv\" is already in use")
    );
    assert!(tools.get("mcp__srv__remote", None).is_some());

    let (other, other_tools) = registry();
    let sibling = apply_with_runtime(
        &other,
        config("srv", false),
        runtime(StartupClient::success("other")),
    )
    .await
    .unwrap();
    assert!(other_tools.get("mcp__srv__other", None).is_some());
    first.dispose().await.unwrap();
    sibling.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
    other.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn startup_failure_is_optional_or_strict_and_strict_conflicts_roll_back() {
    let (context, tools) = registry();
    let optional = apply_with_runtime(
        &context,
        config("optional", false),
        runtime(StartupClient::failed("connection refused")),
    )
    .await
    .unwrap();
    assert!(tools.get("mcp__optional__remote", None).is_none());
    optional.dispose().await.unwrap();

    let (strict_context, strict_tools) = registry();
    let error = apply_with_runtime(
        &strict_context,
        config("strict", true),
        runtime(StartupClient::failed("connection refused")),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "mcp-client(strict): initial connection or tool synchronization failed"
    );
    assert!(format!("{error:#}").contains("connection refused"));
    assert!(strict_tools.get("mcp__strict__remote", None).is_none());

    let (conflict_context, conflict_tools) = registry();
    conflict_tools
        .register(
            &conflict_context,
            ToolDefinition::new(
                "mcp__conflict__remote",
                "foreign",
                Map::from_iter([("type".to_owned(), json!("object"))]),
                ToolOutputDefinition::new(
                    Arc::new(assert_supported_json_schema(json!({})).unwrap()),
                    Arc::new(|_, _| Ok(Vec::new())),
                ),
                Arc::new(|_, _| Box::pin(async { Ok(Value::Null) })),
            ),
        )
        .unwrap();
    let error = apply_with_runtime(
        &conflict_context,
        config("conflict", true),
        runtime(StartupClient::success("remote")),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("initial connection or tool synchronization failed")
    );
    assert_eq!(
        conflict_tools
            .get("mcp__conflict__remote", None)
            .unwrap()
            .description,
        "foreign"
    );
    context.fiber().dispose().await.unwrap();
    strict_context.fiber().dispose().await.unwrap();
    conflict_context.fiber().dispose().await.unwrap();
}
