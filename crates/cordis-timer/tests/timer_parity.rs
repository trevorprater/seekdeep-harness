//! Deterministic timeout, interval, delay, throttle, debounce, and teardown parity.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Fiber};
use seekdeep_cordis_timer::{
    PreparedTimer, TimerCallback, TimerDriver, TimerService, TimerTick, TimerTickError,
    ValueTimerCallback,
};
use serde_json::{Value, json};

struct FakeTimer {
    delay: Duration,
    callback: TimerCallback,
    repeat: bool,
    started: AtomicBool,
    cancelled: AtomicBool,
}

impl PreparedTimer for FakeTimer {
    fn start(&self) {
        self.started.store(true, Ordering::Release);
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl FakeTimer {
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
struct FakeDriver {
    now: Mutex<Duration>,
    timers: Mutex<Vec<Arc<FakeTimer>>>,
}

impl FakeDriver {
    fn timer(&self, index: usize) -> Arc<FakeTimer> {
        self.timers.lock()[index].clone()
    }

    fn set_now(&self, now: Duration) {
        *self.now.lock() = now;
    }

    fn prepare(&self, delay: Duration, callback: TimerCallback, repeat: bool) -> Arc<FakeTimer> {
        let timer = Arc::new(FakeTimer {
            delay,
            callback,
            repeat,
            started: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        });
        self.timers.lock().push(timer.clone());
        timer
    }
}

impl TimerDriver for FakeDriver {
    fn now(&self) -> Duration {
        *self.now.lock()
    }

    fn prepare_timeout(&self, delay: Duration, callback: TimerCallback) -> Arc<dyn PreparedTimer> {
        self.prepare(delay, callback, false)
    }

    fn prepare_interval(&self, delay: Duration, callback: TimerCallback) -> Arc<dyn PreparedTimer> {
        self.prepare(delay, callback, true)
    }
}

fn increment(counter: &Arc<AtomicUsize>) -> TimerCallback {
    let counter = counter.clone();
    Arc::new(move || {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::AcqRel);
        })
    })
}

#[tokio::test]
async fn timeout_interval_and_ticks_are_armed_after_ownership_and_stop_at_disposal() {
    let context = Context::new();
    let driver = Arc::new(FakeDriver::default());
    let service = TimerService::install(&context, driver.clone()).unwrap();
    let timeout_count = Arc::new(AtomicUsize::new(0));
    let interval_count = Arc::new(AtomicUsize::new(0));
    service
        .timeout(
            &context,
            increment(&timeout_count),
            Duration::from_millis(5),
        )
        .unwrap();
    service
        .interval(
            &context,
            increment(&interval_count),
            Duration::from_millis(7),
        )
        .unwrap();
    let mut ticks = service.ticks(&context, Duration::from_millis(9)).unwrap();
    assert!(driver.timer(0).started.load(Ordering::Acquire));
    assert_eq!(driver.timer(0).delay, Duration::from_millis(5));

    driver.timer(0).fire().await;
    driver.timer(0).fire().await;
    driver.timer(1).fire().await;
    driver.timer(1).fire().await;
    driver.timer(2).fire().await;
    assert_eq!(timeout_count.load(Ordering::Acquire), 1);
    assert_eq!(interval_count.load(Ordering::Acquire), 2);
    ticks.next().await.unwrap();

    context.fiber().dispose().await.unwrap();
    driver.timer(1).fire().await;
    assert_eq!(interval_count.load(Ordering::Acquire), 2);
    assert!(ticks.next().await.is_err());
}

#[tokio::test]
async fn delay_resolves_on_fire_and_rejects_when_its_exact_owner_disposes() {
    let context = Context::new();
    let driver = Arc::new(FakeDriver::default());
    let service = TimerService::install(&context, driver.clone()).unwrap();
    let delay = service.delay(&context, Duration::from_millis(5)).unwrap();
    driver.timer(0).fire().await;
    delay.await.unwrap();

    let child_fiber = Fiber::active_child("timer-child");
    let child = context.with_fiber(child_fiber.clone());
    let disposed = service.delay(&child, Duration::from_millis(10)).unwrap();
    child_fiber.dispose().await.unwrap();
    assert!(disposed.await.is_err());
    assert!(driver.timer(1).cancelled.load(Ordering::Acquire));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn debounce_replaces_trailing_values_and_throttle_uses_the_injected_clock() {
    let context = Context::new();
    let driver = Arc::new(FakeDriver::default());
    let service = TimerService::install(&context, driver.clone()).unwrap();
    let values = Arc::new(Mutex::new(Vec::<Value>::new()));
    let callback: ValueTimerCallback = {
        let values = values.clone();
        Arc::new(move |value| {
            let values = values.clone();
            Box::pin(async move {
                values.lock().push(value);
            })
        })
    };
    let debounce = service.debounce(context.clone(), callback.clone(), Duration::from_millis(10));
    debounce.call(json!("first")).await.unwrap();
    debounce.call(json!("second")).await.unwrap();
    driver.timer(0).fire().await;
    driver.timer(1).fire().await;
    assert_eq!(*values.lock(), [json!("second")]);

    let throttle = service.throttle(context.clone(), callback, Duration::from_millis(10), false);
    throttle.call(json!("immediate")).await.unwrap();
    driver.set_now(Duration::from_millis(5));
    throttle.call(json!("old-trailing")).await.unwrap();
    throttle.call(json!("new-trailing")).await.unwrap();
    driver.set_now(Duration::from_millis(10));
    driver.timer(2).fire().await;
    driver.timer(3).fire().await;
    assert_eq!(
        *values.lock(),
        [json!("second"), json!("immediate"), json!("new-trailing")]
    );
    throttle.dispose().await.unwrap();
    debounce.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn interval_iterator_return_and_throw_preserve_the_first_terminal_value() {
    let context = Context::new();
    let driver = Arc::new(FakeDriver::default());
    let service = TimerService::install(&context, driver).unwrap();
    let mut returned = service.ticks(&context, Duration::from_millis(1)).unwrap();
    assert_eq!(
        returned.return_value(json!({"done": true})).await.unwrap(),
        TimerTick::Done(json!({"done": true}))
    );
    assert_eq!(
        returned.next_result().await.unwrap(),
        TimerTick::Done(json!({"done": true}))
    );

    let mut thrown = service.ticks(&context, Duration::from_millis(1)).unwrap();
    assert_eq!(
        thrown.throw_reason(json!("stop")).await.unwrap(),
        TimerTick::Done(Value::Null)
    );
    assert_eq!(
        thrown.next_result().await.unwrap_err(),
        TimerTickError::Thrown(json!("stop"))
    );
    context.fiber().dispose().await.unwrap();
}
