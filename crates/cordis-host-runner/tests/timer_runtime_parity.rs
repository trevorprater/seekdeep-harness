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
use seekdeep_llm::SessionId;
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
