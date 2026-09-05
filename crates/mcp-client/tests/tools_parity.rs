//! Source-differential MCP naming, generation, execution, and rendering contracts.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_mcp_client::{
    McpClient, McpClientSignals, McpTool, McpToolExecution, McpToolPage, RegistrationFailure,
    ToolBridgeOptions, ToolDisposers, extract_text, public_tool_name, sync_tools,
};
use seekdeep_tools::{
    ToolDefinition, ToolExecutionInput, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
    assert_supported_json_schema,
};
use serde_json::{Map, Value, json};

#[derive(Debug)]
struct FakeClient {
    pages: Mutex<VecDeque<Result<McpToolPage, String>>>,
    results: Mutex<BTreeMap<String, Value>>,
    calls: Mutex<Vec<(String, Map<String, Value>)>>,
    signals: Arc<McpClientSignals>,
}

impl FakeClient {
    fn new(tools: Vec<McpTool>) -> Arc<Self> {
        Arc::new(Self {
            pages: Mutex::new(VecDeque::from([Ok(McpToolPage {
                tools,
                next_cursor: None,
            })])),
            results: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(Vec::new()),
            signals: Arc::new(McpClientSignals::default()),
        })
    }

    fn set_pages(&self, pages: impl IntoIterator<Item = Result<McpToolPage, String>>) {
        *self.pages.lock() = pages.into_iter().collect();
    }

    fn result(&self, name: &str, value: Value) {
        self.results.lock().insert(name.to_owned(), value);
    }
}

#[async_trait]
impl McpClient for FakeClient {
    async fn connect(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn list_tools(&self, _cursor: Option<&str>) -> anyhow::Result<McpToolPage> {
        self.pages
            .lock()
            .pop_front()
            .unwrap_or_else(|| Err("tool page script exhausted".to_owned()))
            .map_err(anyhow::Error::msg)
    }

    async fn call_tool(
        &self,
        raw_name: &str,
        arguments: Map<String, Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<Value> {
        if raw_name == "slow" {
            signal.cancelled().await;
            anyhow::bail!("slow call cancelled");
        }
        self.calls.lock().push((raw_name.to_owned(), arguments));
        self.results
            .lock()
            .get(raw_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing result for {raw_name}"))
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

fn tool(name: &str) -> McpTool {
    McpTool {
        name: name.to_owned(),
        description: None,
        input_schema: json!({"type":"object"}),
        output_schema: None,
        execution: None,
    }
}

fn options(server_name: &str) -> ToolBridgeOptions {
    ToolBridgeOptions {
        registration_failure: RegistrationFailure::Contain,
        server_name: server_name.to_owned(),
        tool_call_timeout_ms: 60_000.0,
    }
}

fn registry() -> (Context, Arc<ToolRuntime>) {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    (context, tools)
}

fn input(name: &str, arguments: Value, signal: AbortSignal) -> ToolExecutionInput {
    ToolExecutionInput::new(CallId::new(format!("call-{name}")), name, arguments, signal)
}

#[test]
fn public_names_are_exact_deterministic_and_collision_isolated() {
    assert_eq!(
        public_tool_name("github", "create_issue"),
        "mcp__github__create_issue"
    );
    assert_eq!(
        public_tool_name("everything", "get-sum"),
        "mcp__everything__get-sum"
    );
    let dotted = public_tool_name("srv", "admin.reset");
    assert_eq!(dotted, "mcp__srv__admin_reset_3b185f786768");
    assert_eq!(dotted.len(), 34);
    assert_eq!(dotted, public_tool_name("srv", "admin.reset"));
    assert_ne!(dotted, public_tool_name("srv", "admin_reset"));
    let long = public_tool_name("srv", &"a".repeat(80));
    assert_eq!(long.len(), 64);
    assert_eq!(long.as_bytes()[51], b'_');
    let emoji = public_tool_name("srv", "a😀b");
    assert_eq!(emoji, "mcp__srv__a__b_608dab871d6a");
}

#[tokio::test]
async fn sync_is_paginated_namespaced_and_fetch_failure_preserves_the_generation() {
    let (context, tools) = registry();
    let client = FakeClient::new(Vec::new());
    client.set_pages([
        Ok(McpToolPage {
            tools: vec![tool("search")],
            next_cursor: Some("next".to_owned()),
        }),
        Ok(McpToolPage {
            tools: vec![tool("other")],
            next_cursor: None,
        }),
    ]);
    let mut generation = ToolDisposers::new();
    sync_tools(
        Arc::clone(&client) as Arc<dyn McpClient>,
        &context,
        &options("github"),
        &mut generation,
    )
    .await
    .unwrap();
    assert_eq!(generation.len(), 2);
    assert!(tools.get("mcp__github__search", None).is_some());
    assert!(tools.get("mcp__github__other", None).is_some());
    assert!(tools.get("search", None).is_none());

    client.set_pages([Err("network down".to_owned())]);
    let error = sync_tools(
        Arc::clone(&client) as Arc<dyn McpClient>,
        &context,
        &options("github"),
        &mut generation,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("network down"));
    assert_eq!(generation.len(), 2);
    assert!(tools.get("mcp__github__search", None).is_some());
}

#[tokio::test]
async fn duplicate_lists_and_foreign_namespace_conflicts_fail_atomically() {
    let (context, tools) = registry();
    let duplicate = FakeClient::new(vec![tool("dup"), tool("dup")]);
    let mut generation = ToolDisposers::new();
    let error = sync_tools(
        duplicate as Arc<dyn McpClient>,
        &context,
        &options("srv"),
        &mut generation,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("listed tool \"dup\" more than once")
    );
    assert!(generation.is_empty());

    let squatter = ToolDefinition::new(
        "mcp__srv__taken",
        "foreign",
        Map::from_iter([("type".to_owned(), json!("object"))]),
        ToolOutputDefinition::new(
            Arc::new(assert_supported_json_schema(json!({})).unwrap()),
            Arc::new(|_, _| Ok(Vec::new())),
        ),
        Arc::new(|_, _| Box::pin(async { Ok(Value::Null) })),
    );
    tools.register(&context, squatter).unwrap();
    let conflicted = FakeClient::new(vec![tool("free"), tool("taken")]);
    sync_tools(
        conflicted as Arc<dyn McpClient>,
        &context,
        &options("srv"),
        &mut generation,
    )
    .await
    .unwrap();
    assert!(generation.is_empty());
    assert!(tools.get("mcp__srv__free", None).is_none());
    assert_eq!(
        tools.get("mcp__srv__taken", None).unwrap().description,
        "foreign"
    );
}

#[tokio::test]
async fn execution_preserves_wire_identity_canonical_blocks_and_native_placeholders() {
    let (context, tools) = registry();
    let client = FakeClient::new(vec![tool("admin.reset"), tool("primitive")]);
    client.result(
        "admin.reset",
        json!({"content":[
            {"type":"text","text":"before"},
            {"type":"image","mimeType":"image/png","data":"bytes"},
            {"type":"audio"},
            {"type":"resource"},
            {"type":"video"}
        ],"structuredContent":{"answer":42}}),
    );
    client.result("primitive", json!({"content":[42,null,["nested"]]}));
    let mut generation = ToolDisposers::new();
    sync_tools(
        Arc::clone(&client) as Arc<dyn McpClient>,
        &context,
        &options("srv"),
        &mut generation,
    )
    .await
    .unwrap();
    let public = public_tool_name("srv", "admin.reset");
    let result = tools
        .execute(input(&public, json!({"x":1}), AbortSignal::default()))
        .await;
    assert!(!result.is_error());
    assert_eq!(
        result.content(),
        [ContentBlock::Text {
            text: "before\n[image: image/png, content discarded]\n[audio: unknown, content discarded]\n[resource: content discarded]\n[unsupported content type: video]".to_owned()
        }]
    );
    assert_eq!(
        result.value().unwrap(),
        &json!({"content":[
            {"type":"text","text":"before"},
            {"type":"image","mimeType":"image/png","data":"bytes"},
            {"type":"audio"},{"type":"resource"},{"type":"video"}
        ],"structuredContent":{"answer":42}})
    );
    assert_eq!(client.calls.lock()[0].0, "admin.reset");

    let primitive = tools
        .execute(input(
            "mcp__srv__primitive",
            Value::Null,
            AbortSignal::default(),
        ))
        .await;
    assert!(!primitive.is_error());
    assert_eq!(
        primitive.content()[0],
        ContentBlock::Text {
            text: "[unsupported content type: unknown]\n[unsupported content type: unknown]\n[unsupported content type: unknown]".to_owned()
        }
    );
    assert!(client.calls.lock()[1].1.is_empty());
}

#[tokio::test]
async fn structured_legacy_error_task_and_cancellation_boundaries_are_fail_closed() {
    let (context, tools) = registry();
    let mut structured = tool("structured");
    structured.output_schema = Some(json!({
        "type":"object","additionalProperties":false,
        "properties":{"answer":{"type":"integer"}},"required":["answer"]
    }));
    let mut task = tool("task-only");
    task.execution = Some(McpToolExecution {
        task_support: Some("required".to_owned()),
    });
    let client = FakeClient::new(vec![
        structured,
        tool("legacy"),
        tool("fail"),
        task,
        tool("slow"),
    ]);
    client.result(
        "structured",
        json!({"content":[],"structuredContent":{"answer":"wrong"}}),
    );
    client.result("legacy", json!({"toolResult":{"key":"value"}}));
    client.result(
        "fail",
        json!({"content":[{"type":"text","text":"nope"}],"isError":true}),
    );
    let mut generation = ToolDisposers::new();
    sync_tools(
        Arc::clone(&client) as Arc<dyn McpClient>,
        &context,
        &options("srv"),
        &mut generation,
    )
    .await
    .unwrap();

    let invalid = tools
        .execute(input(
            "mcp__srv__structured",
            json!({}),
            AbortSignal::default(),
        ))
        .await;
    assert!(invalid.is_error());
    assert_eq!(
        invalid.error().unwrap().info.as_ref().unwrap().code,
        "INVALID_TOOL_OUTPUT"
    );
    let legacy = tools
        .execute(input("mcp__srv__legacy", json!({}), AbortSignal::default()))
        .await;
    assert_eq!(
        legacy.content()[0],
        ContentBlock::Text {
            text: "{\"key\":\"value\"}".to_owned()
        }
    );
    let failed = tools
        .execute(input("mcp__srv__fail", json!({}), AbortSignal::default()))
        .await;
    assert!(failed.is_error());
    assert_eq!(failed.error().unwrap().message, "nope");
    let task = tools
        .execute(input(
            "mcp__srv__task-only",
            json!({}),
            AbortSignal::default(),
        ))
        .await;
    assert!(
        task.error()
            .unwrap()
            .message
            .contains("requires task-based execution")
    );

    let signal = AbortSignal::default();
    let tools_for_call = Arc::clone(&tools);
    let call_signal = signal.clone();
    let pending = tokio::spawn(async move {
        tools_for_call
            .execute(input("mcp__srv__slow", json!({}), call_signal))
            .await
    });
    tokio::task::yield_now().await;
    signal.abort_with_reason(json!("cancel"));
    assert!(pending.await.unwrap().is_error());
}

#[test]
fn text_projection_matches_every_trust_boundary_fallback() {
    assert_eq!(
        extract_text(&[], "empty"),
        "(empty returned no text content)"
    );
    assert_eq!(
        extract_text(&[json!({"type":"text"})], "missing"),
        "(missing returned no text content)"
    );
    assert_eq!(
        extract_text(&[json!({"type":"image"})], "image"),
        "[image: unknown, content discarded]"
    );
    assert_eq!(
        extract_text(&[json!({})], "unknown"),
        "[unsupported content type: undefined]"
    );
}
