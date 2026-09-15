//! Reconnect budget, generation, synchronization, and disposal conformance.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, LogExporter, LogMessage};
use seekdeep_llm::AbortSignal;
use seekdeep_mcp_client::{
    Config, ConnectionRuntime, McpClient, McpClientFactory, McpClientSignals, McpTiming, McpTool,
    McpToolPage, ResolvedReconnectPolicy, start_connection,
};
use seekdeep_tools::{ToolRuntime, ToolRuntimeConfig};
use serde_json::{Map, Value, json};

#[derive(Debug)]
struct SleepGate {
    milliseconds: f64,
    released: AtomicBool,
    notify: tokio::sync::Notify,
}

impl SleepGate {
    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.released.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Debug, Default)]
struct ManualTiming {
    now: Mutex<f64>,
    sleeps: Mutex<Vec<Arc<SleepGate>>>,
}

impl ManualTiming {
    fn advance(&self, milliseconds: f64) {
        *self.now.lock() += milliseconds;
    }

    async fn delay(&self, milliseconds: f64) -> Arc<SleepGate> {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(gate) = self
                    .sleeps
                    .lock()
                    .iter()
                    .find(|gate| {
                        (gate.milliseconds - milliseconds).abs() < f64::EPSILON
                            && !gate.released.load(Ordering::Acquire)
                    })
                    .cloned()
                {
                    return gate;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("expected policy delay")
    }
}

#[async_trait]
impl McpTiming for ManualTiming {
    fn now_ms(&self) -> f64 {
        *self.now.lock()
    }

    async fn sleep(&self, milliseconds: f64) {
        let gate = Arc::new(SleepGate {
            milliseconds,
            released: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        });
        self.sleeps.lock().push(Arc::clone(&gate));
        gate.wait().await;
    }
}

#[derive(Debug)]
struct ListGate {
    started: AtomicBool,
    released: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Default for ListGate {
    fn default() -> Self {
        Self {
            started: AtomicBool::new(false),
            released: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }
}

impl ListGate {
    async fn wait(&self) {
        self.started.store(true, Ordering::Release);
        loop {
            let notified = self.notify.notified();
            if self.released.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
}

#[derive(Debug)]
struct FakeClient {
    connect_error: Option<String>,
    tools: Mutex<Vec<McpTool>>,
    list_error: Mutex<Option<String>>,
    list_gate: Mutex<Option<Arc<ListGate>>>,
    list_calls: AtomicUsize,
    close_hangs: bool,
    close_calls: AtomicUsize,
    signals: Arc<McpClientSignals>,
}

impl FakeClient {
    fn connected(name: &str) -> Arc<Self> {
        Arc::new(Self {
            connect_error: None,
            tools: Mutex::new(vec![tool(name)]),
            list_error: Mutex::new(None),
            list_gate: Mutex::new(None),
            list_calls: AtomicUsize::new(0),
            close_hangs: false,
            close_calls: AtomicUsize::new(0),
            signals: Arc::new(McpClientSignals::default()),
        })
    }

    fn failed(message: &str) -> Arc<Self> {
        Arc::new(Self {
            connect_error: Some(message.to_owned()),
            tools: Mutex::new(Vec::new()),
            list_error: Mutex::new(None),
            list_gate: Mutex::new(None),
            list_calls: AtomicUsize::new(0),
            close_hangs: false,
            close_calls: AtomicUsize::new(0),
            signals: Arc::new(McpClientSignals::default()),
        })
    }

    fn failed_with_hung_close(message: &str) -> Arc<Self> {
        let mut client = Self::failed(message);
        Arc::get_mut(&mut client).unwrap().close_hangs = true;
        client
    }

    fn replace_tools(&self, names: &[&str]) {
        *self.tools.lock() = names.iter().map(|name| tool(name)).collect();
        self.signals.tools_changed();
    }

    fn fail_listing(&self, message: &str) {
        *self.list_error.lock() = Some(message.to_owned());
        self.signals.tools_changed();
    }

    fn block_listing(&self, gate: Arc<ListGate>) {
        *self.list_gate.lock() = Some(gate);
        self.signals.tools_changed();
    }

    fn disconnect(&self) {
        self.signals.close();
    }
}

#[async_trait]
impl McpClient for FakeClient {
    async fn connect(&self) -> anyhow::Result<()> {
        match &self.connect_error {
            Some(error) => Err(anyhow::anyhow!(error.clone())),
            None => Ok(()),
        }
    }

    async fn list_tools(&self, _cursor: Option<&str>) -> anyhow::Result<McpToolPage> {
        self.list_calls.fetch_add(1, Ordering::AcqRel);
        let gate = self.list_gate.lock().take();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        if let Some(error) = self.list_error.lock().take() {
            anyhow::bail!(error);
        }
        Ok(McpToolPage {
            tools: self.tools.lock().clone(),
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        _raw_name: &str,
        _arguments: Map<String, Value>,
        _signal: AbortSignal,
    ) -> anyhow::Result<Value> {
        Ok(json!({"content":[{"type":"text","text":"ok"}]}))
    }

    async fn close(&self) -> anyhow::Result<()> {
        self.close_calls.fetch_add(1, Ordering::AcqRel);
        if self.close_hangs {
            futures::future::pending::<()>().await;
        }
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
struct FakeFactory {
    clients: Mutex<VecDeque<Result<Arc<FakeClient>, String>>>,
    creates: AtomicUsize,
}

impl FakeFactory {
    fn new(clients: impl IntoIterator<Item = Result<Arc<FakeClient>, String>>) -> Arc<Self> {
        Arc::new(Self {
            clients: Mutex::new(clients.into_iter().collect()),
            creates: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl McpClientFactory for FakeFactory {
    async fn create(&self, _config: &Config) -> anyhow::Result<Arc<dyn McpClient>> {
        self.creates.fetch_add(1, Ordering::AcqRel);
        self.clients
            .lock()
            .pop_front()
            .unwrap_or_else(|| Err("factory script exhausted".to_owned()))
            .map(|client| client as Arc<dyn McpClient>)
            .map_err(anyhow::Error::msg)
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

fn config() -> Config {
    Config::Stdio {
        server_name: "srv".to_owned(),
        command: "fixture".to_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: String::new(),
        tool_call_timeout_ms: 60_000.0,
        fail_on_startup_error: false,
        reconnect: None,
    }
}

fn policy(initial: f64, maximum: f64, attempts: u64) -> ResolvedReconnectPolicy {
    ResolvedReconnectPolicy {
        enabled: true,
        initial_delay_ms: initial,
        max_delay_ms: maximum,
        max_attempts: attempts,
    }
}

fn registry() -> (Context, Arc<ToolRuntime>, Arc<Mutex<Vec<LogMessage>>>) {
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let logs = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&logs);
    let mut exporter = LogExporter::new(move |message| observed.lock().push(message));
    exporter.levels.insert(
        "default".to_owned(),
        seekdeep_cordis::LoggerLevel::Debug as i32,
    );
    context
        .logger_service()
        .exporter(&context, exporter)
        .unwrap();
    (context, tools, logs)
}

fn runtime(factory: Arc<FakeFactory>, timing: Arc<ManualTiming>) -> ConnectionRuntime {
    ConnectionRuntime { factory, timing }
}

async fn wait_tool(tools: &ToolRuntime, name: &str, present: bool) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while tools.get(name, None).is_some() != present {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("tool registry reached expected state");
}

fn messages(logs: &Mutex<Vec<LogMessage>>) -> Vec<String> {
    logs.lock()
        .iter()
        .filter_map(|message| {
            message
                .args
                .first()
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

#[tokio::test]
async fn reconnect_swaps_generations_serves_notifications_and_ignores_stale_sources() {
    let (context, tools, logs) = registry();
    let first = FakeClient::connected("remote");
    let second = FakeClient::connected("revived");
    let factory = FakeFactory::new([Ok(Arc::clone(&first)), Ok(Arc::clone(&second))]);
    let timing = Arc::new(ManualTiming::default());
    let handle = start_connection(
        &context,
        config(),
        policy(5.0, 40.0, 5),
        runtime(Arc::clone(&factory), Arc::clone(&timing)),
    );
    assert_eq!(handle.initial_error().await, None);
    wait_tool(&tools, "mcp__srv__remote", true).await;

    first.disconnect();
    let delay = timing.delay(5.0).await;
    delay.release();
    wait_tool(&tools, "mcp__srv__revived", true).await;
    assert!(tools.get("mcp__srv__remote", None).is_none());
    assert_eq!(factory.creates.load(Ordering::Acquire), 2);
    assert!(
        messages(&logs)
            .iter()
            .any(|line| line.contains("reconnecting in 5ms (attempt 1/5)"))
    );
    assert!(
        messages(&logs)
            .iter()
            .any(|line| line.contains("reconnected and re-synced tools"))
    );

    let calls = first.list_calls.load(Ordering::Acquire);
    first.replace_tools(&["stale"]);
    tokio::task::yield_now().await;
    assert_eq!(first.list_calls.load(Ordering::Acquire), calls);

    second.replace_tools(&["updated"]);
    wait_tool(&tools, "mcp__srv__updated", true).await;
    assert!(tools.get("mcp__srv__revived", None).is_none());
    handle.dispose().await.unwrap();
    wait_tool(&tools, "mcp__srv__updated", false).await;
}

#[tokio::test]
async fn failed_resync_retains_last_good_generation_and_disposal_quiesces_it() {
    let (context, tools, _) = registry();
    let client = FakeClient::connected("stable");
    let factory = FakeFactory::new([Ok(Arc::clone(&client))]);
    let timing = Arc::new(ManualTiming::default());
    let handle = start_connection(
        &context,
        config(),
        policy(5.0, 40.0, 5),
        runtime(factory, timing),
    );
    assert_eq!(handle.initial_error().await, None);
    client.fail_listing("flaky server");
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while client.list_calls.load(Ordering::Acquire) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(tools.get("mcp__srv__stable", None).is_some());
    handle.dispose().await.unwrap();
    assert!(tools.get("mcp__srv__stable", None).is_none());
}

#[tokio::test]
async fn failure_budget_unregisters_tools_and_stops_after_exact_attempt_cap() {
    let (context, tools, logs) = registry();
    let live = FakeClient::connected("remote");
    let failed_one = FakeClient::failed("server gone");
    let failed_two = FakeClient::failed("server gone");
    let factory = FakeFactory::new([Ok(Arc::clone(&live)), Ok(failed_one), Ok(failed_two)]);
    let timing = Arc::new(ManualTiming::default());
    let handle = start_connection(
        &context,
        config(),
        policy(2.0, 8.0, 2),
        runtime(Arc::clone(&factory), Arc::clone(&timing)),
    );
    assert_eq!(handle.initial_error().await, None);
    live.disconnect();
    timing.delay(2.0).await.release();
    timing.delay(4.0).await.release();
    wait_tool(&tools, "mcp__srv__remote", false).await;
    assert_eq!(factory.creates.load(Ordering::Acquire), 3);
    assert!(
        messages(&logs).iter().any(|line| {
            line.contains("giving up after 2 consecutive failed reconnect attempts")
        })
    );
    handle.dispose().await.unwrap();
}

#[tokio::test]
async fn stable_uptime_resets_budget_while_a_crash_loop_exhausts_it() {
    let (context, tools, _) = registry();
    let first = FakeClient::connected("remote");
    let second = FakeClient::connected("remote");
    let third = FakeClient::connected("remote");
    let factory = FakeFactory::new([
        Ok(Arc::clone(&first)),
        Ok(Arc::clone(&second)),
        Ok(Arc::clone(&third)),
    ]);
    let timing = Arc::new(ManualTiming::default());
    let handle = start_connection(
        &context,
        config(),
        policy(2.0, 30.0, 1),
        runtime(Arc::clone(&factory), Arc::clone(&timing)),
    );
    assert_eq!(handle.initial_error().await, None);
    first.disconnect();
    timing.delay(2.0).await.release();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while factory.creates.load(Ordering::Acquire) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    timing.advance(40.0);
    second.disconnect();
    timing.delay(2.0).await.release();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while factory.creates.load(Ordering::Acquire) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(tools.get("mcp__srv__remote", None).is_some());
    handle.dispose().await.unwrap();

    let (context, tools, _) = registry();
    let first = FakeClient::connected("remote");
    let second = FakeClient::connected("remote");
    let factory = FakeFactory::new([Ok(Arc::clone(&first)), Ok(Arc::clone(&second))]);
    let timing = Arc::new(ManualTiming::default());
    let handle = start_connection(
        &context,
        config(),
        policy(2.0, 10_000.0, 1),
        runtime(factory, Arc::clone(&timing)),
    );
    assert_eq!(handle.initial_error().await, None);
    first.disconnect();
    timing.delay(2.0).await.release();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while tools.get("mcp__srv__remote", None).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    second.disconnect();
    wait_tool(&tools, "mcp__srv__remote", false).await;
    handle.dispose().await.unwrap();
}

#[tokio::test]
async fn disabled_reconnect_retains_tools_until_disposal_and_backoff_is_abortable() {
    let (context, tools, logs) = registry();
    let client = FakeClient::connected("remote");
    let factory = FakeFactory::new([Ok(Arc::clone(&client))]);
    let timing = Arc::new(ManualTiming::default());
    let mut disabled = policy(5.0, 40.0, 5);
    disabled.enabled = false;
    let handle = start_connection(&context, config(), disabled, runtime(factory, timing));
    assert_eq!(handle.initial_error().await, None);
    client.disconnect();
    tokio::task::yield_now().await;
    assert!(tools.get("mcp__srv__remote", None).is_some());
    assert!(
        messages(&logs)
            .iter()
            .any(|line| line.contains("reconnect is disabled"))
    );
    handle.dispose().await.unwrap();
    assert!(tools.get("mcp__srv__remote", None).is_none());

    let (context, tools, _) = registry();
    let client = FakeClient::connected("remote");
    let factory = FakeFactory::new([Ok(Arc::clone(&client))]);
    let timing = Arc::new(ManualTiming::default());
    let handle = start_connection(
        &context,
        config(),
        policy(60_000.0, 60_000.0, 5),
        runtime(factory, Arc::clone(&timing)),
    );
    assert_eq!(handle.initial_error().await, None);
    client.disconnect();
    let _delay = timing.delay(60_000.0).await;
    handle.dispose().await.unwrap();
    assert!(tools.get("mcp__srv__remote", None).is_none());
}

#[tokio::test]
async fn disposal_during_sync_leaks_nothing_and_hung_close_stops_overlap() {
    let (context, tools, _) = registry();
    let client = FakeClient::connected("remote");
    let factory = FakeFactory::new([Ok(Arc::clone(&client))]);
    let timing = Arc::new(ManualTiming::default());
    let handle = start_connection(
        &context,
        config(),
        policy(2.0, 8.0, 5),
        runtime(factory, timing),
    );
    assert_eq!(handle.initial_error().await, None);
    let gate = Arc::new(ListGate::default());
    client.block_listing(Arc::clone(&gate));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !gate.started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    handle.dispose().await.unwrap();
    gate.release();
    assert!(tools.get("mcp__srv__remote", None).is_none());

    let (context, _, logs) = registry();
    let failed = FakeClient::failed_with_hung_close("initialize failed");
    let factory = FakeFactory::new([Ok(failed)]);
    let timing = Arc::new(ManualTiming::default());
    let handle = start_connection(
        &context,
        config(),
        policy(2.0, 8.0, 2),
        runtime(Arc::clone(&factory), Arc::clone(&timing)),
    );
    assert!(handle.initial_error().await.is_some());
    timing.delay(5_000.0).await.release();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !messages(&logs)
            .iter()
            .any(|line| line.contains("reconnect stopped to avoid overlapping"))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(factory.creates.load(Ordering::Acquire), 1);
    handle.dispose().await.unwrap();
}
