//! Dynamic Host timer injection, late effects, aliases, and teardown parity.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, ServiceKey};
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
};
use seekdeep_cordis_timer::{PreparedTimer, TimerCallback, TimerDriver, TimerService};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, SessionId};
use seekdeep_tools::{ToolExecutionInput, ToolExecutionResult, ToolRuntime, ToolRuntimeConfig};
use serde_json::{Value, json};

const FIRED: ServiceKey<Value> = ServiceKey::new("timerFired");

struct ManualTimer {
    callback: TimerCallback,
    repeat: bool,
    started: AtomicBool,
    cancelled: AtomicBool,
}

impl PreparedTimer for ManualTimer {
    fn start(&self) {
        self.started.store(true, Ordering::Release);
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl ManualTimer {
    async fn fire(&self) {
        if !self.started.load(Ordering::Acquire) || self.cancelled.load(Ordering::Acquire) {
            return;
        }
        (self.callback)().await;
        if !self.repeat {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

#[derive(Default)]
struct ManualDriver {
    timers: Mutex<Vec<Arc<ManualTimer>>>,
}

impl ManualDriver {
    fn prepare(&self, callback: TimerCallback, repeat: bool) -> Arc<ManualTimer> {
        let timer = Arc::new(ManualTimer {
            callback,
            repeat,
            started: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        });
        self.timers.lock().push(timer.clone());
        timer
    }

    fn timer(&self, index: usize) -> Arc<ManualTimer> {
        self.timers.lock()[index].clone()
    }
}

impl TimerDriver for ManualDriver {
    fn now(&self) -> Duration {
        Duration::ZERO
    }

    fn prepare_timeout(&self, _delay: Duration, callback: TimerCallback) -> Arc<dyn PreparedTimer> {
        self.prepare(callback, false)
    }

    fn prepare_interval(
        &self,
        _delay: Duration,
        callback: TimerCallback,
    ) -> Arc<dyn PreparedTimer> {
        self.prepare(callback, true)
    }
}

async fn wait_for_timer(driver: &ManualDriver, index: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if driver.timers.lock().len() > index {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timer registration");
}

#[tokio::test]
async fn timer_callbacks_apply_late_commands_and_stop_cancels_every_owned_timer() {
    let context = Context::new();
    let driver = Arc::new(ManualDriver::default());
    TimerService::install(&context, driver.clone()).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "timer".to_owned(),
            },
            name: "timer".to_owned(),
            purpose: "schedule work".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "let count = 0; harness.handle('count', async () => count);",
                        "return { inject: ['timer'], apply(ctx) {",
                        "ctx.setTimeout(() => { count += 1; ctx.provide('timerFired', true); }, 5);",
                        "ctx.setInterval(() => { count += 10; }, 7);",
                        "} };",
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
    let run_id = match started {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    };

    driver.timer(0).fire().await;
    driver.timer(1).fire().await;
    assert_eq!(context.get(FIRED).as_deref(), Some(&json!(true)));
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "count", json!(null))
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success { value: json!(11) }
    );

    runner.stop(&session, &defined.plugin_id).await;
    assert!(context.get(FIRED).is_none());
    assert!(driver.timer(1).cancelled.load(Ordering::Acquire));
    driver.timer(1).fire().await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn timer_helpers_require_the_declared_service_even_when_the_provider_is_live() {
    let context = Context::new();
    TimerService::install(&context, Arc::new(ManualDriver::default())).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "timer".to_owned(),
            },
            name: "timer".to_owned(),
            purpose: "missing inject".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    "return { apply(ctx) { ctx.timeout(() => undefined, 1); } };".to_owned(),
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
            if message.contains("service \"timer\" is not injected")
    ));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn late_timer_and_effect_disposers_remove_the_exact_live_generation_once() {
    let context = Context::new();
    let driver = Arc::new(ManualDriver::default());
    TimerService::install(&context, driver.clone()).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-late-dispose");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "timer".to_owned(),
            },
            name: "late disposer".to_owned(),
            purpose: "dispose published effects".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "let count = 0; let disposed = 0; let stopTimer; let stopEffect;",
                        "harness.handle('count', async () => count);",
                        "harness.handle('dispose', async () => {",
                        "stopTimer(); stopEffect(); stopEffect(); return disposed; });",
                        "return { inject: ['timer'], apply(ctx) {",
                        "stopTimer = ctx.interval(() => { count += 1; }, 5);",
                        "stopEffect = ctx.effect(() => () => { disposed += 1; });",
                        "} };",
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
    let run_id = match started {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    };
    assert!(driver.timer(0).started.load(Ordering::Acquire));
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "dispose", Value::Null)
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success { value: json!(1) }
    );
    assert!(driver.timer(0).cancelled.load(Ordering::Acquire));
    driver.timer(0).fire().await;
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "count", Value::Null)
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success { value: json!(0) }
    );
    runner.stop(&session, &defined.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn throttle_and_debounce_wrappers_replace_trailing_calls_and_follow_fiber_disposal() {
    let context = Context::new();
    let driver = Arc::new(ManualDriver::default());
    TimerService::install(&context, driver.clone()).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-wrapper-timers");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "timer".to_owned(),
            },
            name: "timer wrappers".to_owned(),
            purpose: "throttle and debounce callbacks".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "const calls = []; let throttled; let debounced; let noTrailing;",
                        "harness.handle('call', async args => {",
                        "if (args.kind === 'throttle') throttled(args.value);",
                        "else if (args.kind === 'debounce') debounced(args.value);",
                        "else noTrailing(args.value); return calls.slice(); });",
                        "harness.handle('calls', async () => calls.slice());",
                        "return { inject: ['timer'], apply(ctx) {",
                        "throttled = ctx.throttle(value => calls.push(value), 1000);",
                        "debounced = ctx.debounce(value => calls.push(value), 1000);",
                        "noTrailing = ctx.throttle(value => calls.push(value), 1000, true);",
                        "} };",
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
    let run_id = match started {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    };
    let invoke = |kind: &str, value: &str| {
        runner.invoke(
            &defined.plugin_id,
            &run_id,
            "call",
            json!({"kind": kind, "value": value}),
        )
    };
    invoke("debounce", "old").await;
    invoke("debounce", "debounced").await;
    assert!(driver.timer(0).cancelled.load(Ordering::Acquire));
    invoke("throttle", "immediate").await;
    invoke("throttle", "old-trailing").await;
    invoke("throttle", "new-trailing").await;
    assert!(driver.timer(2).cancelled.load(Ordering::Acquire));
    invoke("no-trailing", "no-trailing-immediate").await;
    invoke("no-trailing", "suppressed").await;
    assert_eq!(driver.timers.lock().len(), 4);

    driver.timer(1).fire().await;
    driver.timer(3).fire().await;
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "calls", Value::Null)
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success {
            value: json!([
                "immediate",
                "no-trailing-immediate",
                "debounced",
                "new-trailing"
            ])
        }
    );
    runner.stop(&session, &defined.plugin_id).await;
    assert!(driver.timer(1).cancelled.load(Ordering::Acquire));
    assert!(driver.timer(3).cancelled.load(Ordering::Acquire));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn timeout_promise_can_suspend_apply_until_the_host_timer_fires() {
    let context = Context::new();
    let driver = Arc::new(ManualDriver::default());
    TimerService::install(&context, driver.clone()).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-timeout-promise");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "timer".to_owned(),
            },
            name: "timeout promise".to_owned(),
            purpose: "await host timer during apply".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "let waited = false; harness.handle('waited', async () => waited);",
                        "return { inject: ['timer'], async apply(ctx) {",
                        "await ctx.timeout(5); waited = true; } };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let run_runner = runner.clone();
    let run_session = session.clone();
    let plugin_id = defined.plugin_id.clone();
    let package_id = defined.package_id.clone();
    let running = tokio::spawn(async move {
        run_runner
            .run(
                &run_session,
                &plugin_id,
                &package_id,
                DynamicCordisRunMode::Run,
            )
            .await
    });
    wait_for_timer(&driver, 0).await;
    driver.timer(0).fire().await;
    let run_id = match running.await.unwrap() {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    };
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "waited", Value::Null)
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success { value: json!(true) }
    );
    runner.stop(&session, &defined.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one iterator instance carries the tick, return, and throw terminal sequence"
)]
async fn interval_iterator_ticks_and_preserves_explicit_return_and_throw_terminals() {
    let context = Context::new();
    let driver = Arc::new(ManualDriver::default());
    TimerService::install(&context, driver.clone()).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-interval-iterator");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "timer".to_owned(),
            },
            name: "interval iterator".to_owned(),
            purpose: "iterate host timer ticks".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "let ticks; let thrown;",
                        "const shape = result => ({ done: result.done, value: result.value ?? null });",
                        "harness.handle('next', async () => shape(await ticks.next()));",
                        "harness.handle('finish', async () => shape(await ticks.return('finished')));",
                        "harness.handle('after', async () => shape(await ticks.next()));",
                        "harness.handle('throw', async () => shape(await thrown.throw('boom')));",
                        "harness.handle('afterThrow', async () => {",
                        "try { await thrown.next(); return 'missing rejection'; }",
                        "catch (reason) { return String(reason); } });",
                        "return { inject: ['timer'], apply(ctx) {",
                        "ticks = ctx.interval(5); thrown = ctx.interval(7); } };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let run_id = match runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await
    {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    };
    wait_for_timer(&driver, 1).await;
    let invoke_runner = runner.clone();
    let invoke_plugin = defined.plugin_id.clone();
    let invoke_run = run_id.clone();
    let next = tokio::spawn(async move {
        invoke_runner
            .invoke(&invoke_plugin, &invoke_run, "next", Value::Null)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !next.is_finished() {
            driver.timer(0).fire().await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pending iterator next settles on a tick");
    assert_eq!(
        next.await.unwrap(),
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success {
            value: json!({"done": false, "value": null})
        }
    );
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "finish", Value::Null)
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success {
            value: json!({"done": true, "value": "finished"})
        }
    );
    assert!(driver.timer(0).cancelled.load(Ordering::Acquire));
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "after", Value::Null)
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success {
            value: json!({"done": true, "value": "finished"})
        }
    );
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "throw", Value::Null)
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success {
            value: json!({"done": true, "value": null})
        }
    );
    assert!(driver.timer(1).cancelled.load(Ordering::Acquire));
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "afterThrow", Value::Null)
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success {
            value: json!("boom")
        }
    );
    runner.stop(&session, &defined.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn stopping_a_generation_rejects_its_pending_timeout_promise() {
    let context = Context::new();
    let driver = Arc::new(ManualDriver::default());
    TimerService::install(&context, driver.clone()).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-timeout-cancel");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "timer".to_owned(),
            },
            name: "timeout cancellation".to_owned(),
            purpose: "reject pending delay at stop".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "let liveCtx;",
                        "harness.handle('wait', async () => {",
                        "await liveCtx.timeout(5); return 'unexpected'; });",
                        "return { inject: ['timer'], apply(ctx) { liveCtx = ctx; } };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let run_id = match runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await
    {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    };
    let invoke_runner = runner.clone();
    let invoke_plugin = defined.plugin_id.clone();
    let invoke_run = run_id;
    let pending = tokio::spawn(async move {
        invoke_runner
            .invoke(&invoke_plugin, &invoke_run, "wait", Value::Null)
            .await
    });
    wait_for_timer(&driver, 0).await;
    runner.stop(&session, &defined.plugin_id).await;
    assert!(driver.timer(0).cancelled.load(Ordering::Acquire));
    assert!(matches!(
        pending.await.unwrap(),
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Failure {
            code: seekdeep_cordis_host_runner::DynamicCordisInvokeErrorCode::HandlerError,
            ref error,
        } if error.message.contains("Context has been disposed")
    ));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn dynamic_tool_execution_can_await_a_generation_owned_timeout() {
    let context = Context::new();
    let driver = Arc::new(ManualDriver::default());
    TimerService::install(&context, driver.clone()).unwrap();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).unwrap();
    tools.provide(&context).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-tool-timeout");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "timer".to_owned(),
            },
            name: "tool timeout".to_owned(),
            purpose: "await a timer from tool execution".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "let liveCtx; return { inject: ['timer', 'tools'], apply(ctx) {",
                        "liveCtx = ctx; harness.registerTool(ctx, harness.defineTool({",
                        "name: 'wait_for_tick', description: 'Wait for one tick.', parameters: {},",
                        "output: { schema: { type: 'string' },",
                        "render: (_args, value) => [{ type: 'text', text: value }] },",
                        "async execute() { await liveCtx.timeout(5); return 'ticked'; }",
                        "})); } };",
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
    let execute_tools = tools.clone();
    let execution = tokio::spawn(async move {
        execute_tools
            .execute(ToolExecutionInput::new(
                CallId::new("timer-tool"),
                "wait_for_tick",
                json!({}),
                AbortSignal::default(),
            ))
            .await
    });
    wait_for_timer(&driver, 0).await;
    driver.timer(0).fire().await;
    assert!(matches!(
        execution.await.unwrap(),
        ToolExecutionResult::Success(ref success)
            if success.value == json!("ticked")
                && success.content == [ContentBlock::Text { text: "ticked".to_owned() }]
    ));
    runner.stop(&session, &defined.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn timer_callback_can_await_another_generation_owned_timer() {
    let context = Context::new();
    let driver = Arc::new(ManualDriver::default());
    TimerService::install(&context, driver.clone()).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-nested-timer");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "timer".to_owned(),
            },
            name: "nested timer".to_owned(),
            purpose: "await a timeout from an interval callback".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "let count = 0; harness.handle('count', async () => count);",
                        "return { inject: ['timer'], apply(ctx) {",
                        "ctx.interval(async () => { await ctx.timeout(5); count += 1; }, 10);",
                        "} };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let run_id = match runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await
    {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    };
    let outer_timer = driver.timer(0);
    let outer = tokio::spawn(async move { outer_timer.fire().await });
    wait_for_timer(&driver, 1).await;
    driver.timer(1).fire().await;
    outer.await.unwrap();
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "count", Value::Null)
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success { value: json!(1) }
    );
    runner.stop(&session, &defined.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}
