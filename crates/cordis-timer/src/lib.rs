//! Fiber-owned timer helpers over an injected clock and scheduling driver.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use serde_json::Value;

/// Asynchronous callback executed by one timer.
pub type TimerCallback = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// Prepared timer that cannot fire before [`PreparedTimer::start`].
pub trait PreparedTimer: Send + Sync + 'static {
    /// Arms the timer exactly once.
    fn start(&self);
    /// Prevents future callbacks; a callback already running may finish.
    fn cancel(&self);
}

/// Injectable clock and scheduling policy used by [`TimerService`].
pub trait TimerDriver: Send + Sync + 'static {
    /// Monotonic time used by throttle calculations.
    fn now(&self) -> Duration;
    /// Prepares a one-shot timer without arming it.
    fn prepare_timeout(&self, delay: Duration, callback: TimerCallback) -> Arc<dyn PreparedTimer>;
    /// Prepares a repeating timer without arming it.
    fn prepare_interval(&self, delay: Duration, callback: TimerCallback) -> Arc<dyn PreparedTimer>;
}

/// Cordis service slot inherited as `ctx.timer`.
pub const TIMER: ServiceKey<TimerService> = ServiceKey::new("timer");
/// Loader plugin identity.
pub const NAME: &str = "timer";
/// Timer plugin has no service prerequisites.
pub const INJECT: &[&str] = &[];

/// Error returned when a pending delay loses its owning Context.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("Context has been disposed")]
pub struct ContextDisposed;

/// One result from the interval async-iterator form.
#[derive(Clone, Debug, PartialEq)]
pub enum TimerTick {
    /// A scheduled interval elapsed.
    Tick,
    /// The consumer returned with this value.
    Done(Value),
}

/// Terminal failure observed by a pending interval `next()`.
#[derive(Clone, Debug, thiserror::Error, PartialEq)]
pub enum TimerTickError {
    /// The owning Fiber disposed first.
    #[error(transparent)]
    ContextDisposed(#[from] ContextDisposed),
    /// The consumer explicitly threw this JSON reason.
    #[error("interval iterator was thrown: {0}")]
    Thrown(Value),
}

#[derive(Clone, Debug)]
enum TimerTermination {
    Returned,
    Thrown,
    ContextDisposed,
}

type TimerTickResult = Result<TimerTick, TimerTickError>;
type TimerTickSender = tokio::sync::mpsc::UnboundedSender<TimerTickResult>;
type SharedTimerTickSender = Arc<Mutex<Option<TimerTickSender>>>;

/// Disposable timer helpers bound to an injected driver.
pub struct TimerService {
    driver: Arc<dyn TimerDriver>,
}

impl std::fmt::Debug for TimerService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TimerService")
            .finish_non_exhaustive()
    }
}

impl TimerService {
    /// Creates an unprovided service over `driver`.
    #[must_use]
    pub fn new(driver: Arc<dyn TimerDriver>) -> Arc<Self> {
        Arc::new(Self { driver })
    }

    /// Provides the timer Service for the lifetime of `context`.
    ///
    /// # Errors
    ///
    /// Returns ordinary duplicate-Service or inactive-owner failures.
    pub fn install(
        context: &Context,
        driver: Arc<dyn TimerDriver>,
    ) -> Result<Arc<Self>, seekdeep_cordis::CordisError> {
        let service = Self::new(driver);
        context.provide(TIMER, service.clone())?;
        Ok(service)
    }

    /// Runs `callback` once after `delay`, unless its Fiber disposes first.
    ///
    /// # Errors
    ///
    /// Rejects registration on an inactive Fiber.
    pub fn timeout(
        &self,
        context: &Context,
        callback: TimerCallback,
        delay: Duration,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        let holder = Arc::new(Mutex::new(None::<EffectHandle>));
        let callback_holder = holder.clone();
        let callback = Arc::new(move || {
            let callback = callback.clone();
            let effect = callback_holder.lock().take();
            Box::pin(async move {
                if let Some(effect) = effect {
                    let _ = effect.dispose().await;
                }
                callback().await;
            }) as BoxFuture<'static, ()>
        });
        let timer = self.driver.prepare_timeout(delay, callback);
        let cancellation = timer.clone();
        let effect = EffectHandle::synchronous("ctx.timeout()", move || {
            cancellation.cancel();
            Ok(())
        });
        let effect = context.own(effect)?;
        *holder.lock() = Some(effect.clone());
        timer.start();
        Ok(effect)
    }

    /// Alias for [`Self::timeout`] retained for source compatibility.
    ///
    /// # Errors
    ///
    /// Returns the same inactive-Fiber error as [`Self::timeout`].
    pub fn set_timeout(
        &self,
        context: &Context,
        callback: TimerCallback,
        delay: Duration,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        self.timeout(context, callback, delay)
    }

    /// Resolves after `delay` or rejects when `context` disposes first.
    ///
    /// # Errors
    ///
    /// Rejects registration on an inactive Fiber.
    pub fn delay(
        &self,
        context: &Context,
        delay: Duration,
    ) -> Result<TimerDelay, seekdeep_cordis::CordisError> {
        let sender = Arc::new(Mutex::new(None));
        let (result, receiver) = tokio::sync::oneshot::channel();
        *sender.lock() = Some(result);
        let firing = Arc::new(AtomicBool::new(false));
        let holder = Arc::new(Mutex::new(None::<EffectHandle>));
        let callback_sender = sender.clone();
        let callback_firing = firing.clone();
        let callback_holder = holder.clone();
        let callback = Arc::new(move || {
            callback_firing.store(true, Ordering::Release);
            let effect = callback_holder.lock().take();
            let sender = callback_sender.clone();
            Box::pin(async move {
                if let Some(effect) = effect {
                    let _ = effect.dispose().await;
                }
                if let Some(sender) = sender.lock().take() {
                    let _ = sender.send(Ok(()));
                }
            }) as BoxFuture<'static, ()>
        });
        let timer = self.driver.prepare_timeout(delay, callback);
        let cancellation = timer.clone();
        let dispose_sender = sender.clone();
        let effect = EffectHandle::synchronous("ctx.timeout()", move || {
            cancellation.cancel();
            if !firing.load(Ordering::Acquire)
                && let Some(sender) = dispose_sender.lock().take()
            {
                let _ = sender.send(Err(ContextDisposed));
            }
            Ok(())
        });
        let effect = context.own(effect)?;
        *holder.lock() = Some(effect.clone());
        timer.start();
        Ok(TimerDelay {
            receiver,
            _effect: effect,
        })
    }

    /// Runs `callback` after every `delay` until disposal.
    ///
    /// # Errors
    ///
    /// Rejects registration on an inactive Fiber.
    pub fn interval(
        &self,
        context: &Context,
        callback: TimerCallback,
        delay: Duration,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        let timer = self.driver.prepare_interval(delay, callback);
        let cancellation = timer.clone();
        let effect = EffectHandle::synchronous("ctx.interval()", move || {
            cancellation.cancel();
            Ok(())
        });
        let effect = context.own(effect)?;
        timer.start();
        Ok(effect)
    }

    /// Alias for [`Self::interval`] retained for source compatibility.
    ///
    /// # Errors
    ///
    /// Returns the same inactive-Fiber error as [`Self::interval`].
    pub fn set_interval(
        &self,
        context: &Context,
        callback: TimerCallback,
        delay: Duration,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        self.interval(context, callback, delay)
    }

    /// Returns a Fiber-owned stream of interval ticks.
    ///
    /// # Errors
    ///
    /// Rejects registration on an inactive Fiber.
    pub fn ticks(
        &self,
        context: &Context,
        delay: Duration,
    ) -> Result<TimerTicks, seekdeep_cordis::CordisError> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let termination = Arc::new(Mutex::new(None::<TimerTermination>));
        let callback_sender = sender.clone();
        let callback_termination = termination.clone();
        let callback = Arc::new(move || {
            let sender = callback_sender.clone();
            let termination = callback_termination.clone();
            Box::pin(async move {
                if termination.lock().is_none()
                    && let Some(sender) = sender.lock().as_ref()
                {
                    let _ = sender.send(Ok(TimerTick::Tick));
                }
            }) as BoxFuture<'static, ()>
        });
        let timer = self.driver.prepare_interval(delay, callback);
        let cancellation = timer.clone();
        let dispose_sender = sender.clone();
        let dispose_termination = termination.clone();
        let effect = EffectHandle::synchronous("ctx.interval()", move || {
            cancellation.cancel();
            let mut termination = dispose_termination.lock();
            if termination.is_none() {
                *termination = Some(TimerTermination::ContextDisposed);
                if let Some(sender) = dispose_sender.lock().take() {
                    let _ = sender.send(Err(TimerTickError::ContextDisposed(ContextDisposed)));
                }
            }
            Ok(())
        });
        let effect = context.own(effect)?;
        timer.start();
        Ok(TimerTicks {
            receiver,
            effect,
            sender,
            termination,
        })
    }

    /// Returns a Fiber-owned debounced function.
    #[must_use]
    pub fn debounce(
        self: &Arc<Self>,
        context: Context,
        callback: ValueTimerCallback,
        delay: Duration,
    ) -> Debounced {
        Debounced {
            service: self.clone(),
            context,
            callback,
            delay,
            pending: Mutex::new(None),
            disposed: AtomicBool::new(false),
        }
    }

    /// Returns a Fiber-owned throttled function.
    #[must_use]
    pub fn throttle(
        self: &Arc<Self>,
        context: Context,
        callback: ValueTimerCallback,
        delay: Duration,
        no_trailing: bool,
    ) -> Throttled {
        Throttled {
            service: self.clone(),
            context,
            callback,
            delay,
            last_call: Arc::new(Mutex::new(None)),
            pending: Mutex::new(None),
            trailing_disabled: AtomicBool::new(no_trailing),
        }
    }
}

/// Builds the native Loader-compatible timer plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            TimerService::install(&context, Arc::new(TokioTimerDriver::default()))?;
            Ok(())
        })
    })
}

/// Future returned by [`TimerService::delay`].
pub struct TimerDelay {
    receiver: tokio::sync::oneshot::Receiver<Result<(), ContextDisposed>>,
    _effect: EffectHandle,
}

impl Future for TimerDelay {
    type Output = Result<(), ContextDisposed>;

    fn poll(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver)
            .poll(context)
            .map(|result| result.unwrap_or(Err(ContextDisposed)))
    }
}

/// Async interval ticks that reject after Context disposal.
pub struct TimerTicks {
    receiver: tokio::sync::mpsc::UnboundedReceiver<TimerTickResult>,
    effect: EffectHandle,
    sender: SharedTimerTickSender,
    termination: Arc<Mutex<Option<TimerTermination>>>,
}

impl TimerTicks {
    /// Waits for the next tick or disposal.
    ///
    /// # Errors
    ///
    /// Returns [`ContextDisposed`] after the owning Fiber is disposed.
    pub async fn next(&mut self) -> Result<(), ContextDisposed> {
        match self.next_result().await {
            Ok(TimerTick::Tick) => Ok(()),
            Ok(TimerTick::Done(_)) | Err(_) => Err(ContextDisposed),
        }
    }

    /// Waits for a tick, explicit return, explicit throw, or owner disposal.
    ///
    /// # Errors
    ///
    /// Returns the explicit throw reason or [`ContextDisposed`].
    pub async fn next_result(&mut self) -> Result<TimerTick, TimerTickError> {
        self.receiver
            .recv()
            .await
            .unwrap_or(Err(TimerTickError::ContextDisposed(ContextDisposed)))
    }

    /// Implements async-iterator `return(value)` and cancels future ticks.
    ///
    /// # Errors
    ///
    /// Returns a timer cleanup failure.
    pub async fn return_value(&self, value: Value) -> anyhow::Result<TimerTick> {
        {
            let mut termination = self.termination.lock();
            if termination.is_none() {
                *termination = Some(TimerTermination::Returned);
                if let Some(sender) = self.sender.lock().take() {
                    let _ = sender.send(Ok(TimerTick::Done(value.clone())));
                }
            }
        }
        self.effect.dispose().await?;
        Ok(TimerTick::Done(value))
    }

    /// Implements async-iterator `throw(reason)` and rejects a pending `next()`.
    ///
    /// # Errors
    ///
    /// Returns a timer cleanup failure.
    pub async fn throw_reason(&self, reason: Value) -> anyhow::Result<TimerTick> {
        {
            let mut termination = self.termination.lock();
            if termination.is_none() {
                *termination = Some(TimerTermination::Thrown);
                if let Some(sender) = self.sender.lock().take() {
                    let _ = sender.send(Err(TimerTickError::Thrown(reason)));
                }
            }
        }
        self.effect.dispose().await?;
        Ok(TimerTick::Done(Value::Null))
    }

    /// Ends the iterator and cancels its timer.
    ///
    /// # Errors
    ///
    /// Returns a timer cleanup failure.
    pub async fn finish(&self) -> anyhow::Result<()> {
        self.return_value(Value::Null).await.map(|_| ())
    }
}

/// Callback accepted by throttle and debounce helpers.
pub type ValueTimerCallback = Arc<dyn Fn(Value) -> BoxFuture<'static, ()> + Send + Sync + 'static>;

/// Fiber-owned debounced callback.
pub struct Debounced {
    service: Arc<TimerService>,
    context: Context,
    callback: ValueTimerCallback,
    delay: Duration,
    pending: Mutex<Option<EffectHandle>>,
    disposed: AtomicBool,
}

impl Debounced {
    /// Replaces the trailing call with `value`.
    ///
    /// # Errors
    ///
    /// Returns cleanup or inactive-Fiber failures.
    pub async fn call(&self, value: Value) -> anyhow::Result<()> {
        if self.disposed.load(Ordering::Acquire) {
            return Ok(());
        }
        let pending = { self.pending.lock().take() };
        if let Some(effect) = pending {
            effect.dispose().await?;
        }
        let callback = self.callback.clone();
        let effect = self.service.timeout(
            &self.context,
            Arc::new(move || callback(value.clone())),
            self.delay,
        )?;
        *self.pending.lock() = Some(effect);
        Ok(())
    }

    /// Cancels the trailing call and ignores later calls.
    ///
    /// # Errors
    ///
    /// Returns a timer cleanup failure.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.disposed.store(true, Ordering::Release);
        let pending = { self.pending.lock().take() };
        if let Some(effect) = pending {
            effect.dispose().await?;
        }
        Ok(())
    }
}

/// Fiber-owned throttled callback.
pub struct Throttled {
    service: Arc<TimerService>,
    context: Context,
    callback: ValueTimerCallback,
    delay: Duration,
    last_call: Arc<Mutex<Option<Duration>>>,
    pending: Mutex<Option<EffectHandle>>,
    trailing_disabled: AtomicBool,
}

impl Throttled {
    /// Executes immediately when due, otherwise replaces the trailing call.
    ///
    /// # Errors
    ///
    /// Returns cleanup or inactive-Fiber failures.
    pub async fn call(&self, value: Value) -> anyhow::Result<()> {
        let now = self.service.driver.now();
        let remaining = self.last_call.lock().map_or(Duration::ZERO, |last| {
            self.delay.saturating_sub(now.saturating_sub(last))
        });
        if remaining.is_zero() {
            *self.last_call.lock() = Some(now);
            (self.callback)(value).await;
            return Ok(());
        }
        if self.trailing_disabled.load(Ordering::Acquire) {
            return Ok(());
        }
        let pending = { self.pending.lock().take() };
        if let Some(effect) = pending {
            effect.dispose().await?;
        }
        let callback = self.callback.clone();
        let last_call = self.last_call.clone();
        let driver = self.service.driver.clone();
        let effect = self.service.timeout(
            &self.context,
            Arc::new(move || {
                *last_call.lock() = Some(driver.now());
                callback(value.clone())
            }),
            remaining,
        )?;
        *self.pending.lock() = Some(effect);
        Ok(())
    }

    /// Cancels the trailing call; immediately due calls remain permitted.
    ///
    /// # Errors
    ///
    /// Returns a timer cleanup failure.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.trailing_disabled.store(true, Ordering::Release);
        let pending = { self.pending.lock().take() };
        if let Some(effect) = pending {
            effect.dispose().await?;
        }
        Ok(())
    }
}

/// Tokio-backed boundary driver used by native applications.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct TokioTimerDriver {
    origin: tokio::time::Instant,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for TokioTimerDriver {
    fn default() -> Self {
        Self {
            origin: tokio::time::Instant::now(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl TimerDriver for TokioTimerDriver {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn prepare_timeout(&self, delay: Duration, callback: TimerCallback) -> Arc<dyn PreparedTimer> {
        TokioPreparedTimer::new(delay, callback, false)
    }

    fn prepare_interval(&self, delay: Duration, callback: TimerCallback) -> Arc<dyn PreparedTimer> {
        TokioPreparedTimer::new(delay, callback, true)
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct TokioPreparedTimer {
    delay: Duration,
    callback: TimerCallback,
    repeat: bool,
    started: AtomicBool,
    cancelled: Arc<AtomicBool>,
    firing: Arc<AtomicBool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl TokioPreparedTimer {
    fn new(delay: Duration, callback: TimerCallback, repeat: bool) -> Arc<Self> {
        Arc::new(Self {
            delay,
            callback,
            repeat,
            started: AtomicBool::new(false),
            cancelled: Arc::new(AtomicBool::new(false)),
            firing: Arc::new(AtomicBool::new(false)),
            task: Mutex::new(None),
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl PreparedTimer for TokioPreparedTimer {
    fn start(&self) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let delay = self.delay;
        let callback = self.callback.clone();
        let repeat = self.repeat;
        let cancelled = self.cancelled.clone();
        let firing = self.firing.clone();
        *self.task.lock() = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(delay).await;
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                firing.store(true, Ordering::Release);
                callback().await;
                firing.store(false, Ordering::Release);
                if !repeat || cancelled.load(Ordering::Acquire) {
                    return;
                }
            }
        }));
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if !self.firing.load(Ordering::Acquire)
            && let Some(task) = self.task.lock().take()
        {
            task.abort();
        }
    }
}
