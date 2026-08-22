//! Dynamically named event dispatch with Cordis ordering semantics.

use std::{
    any::Any,
    collections::HashMap,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
};

use futures::{FutureExt, future::join_all, task::noop_waker_ref};
use parking_lot::RwLock;
use uuid::Uuid;

use crate::{
    Context,
    fiber::{CordisError, EffectHandle},
};

/// Type-erased event argument.
pub type EventValue = Arc<dyn Any + Send + Sync>;

/// Opaque identity derived from a scoped event's payload subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EventSubjectToken(Uuid);

impl EventSubjectToken {
    /// Wraps one process-local subject identity.
    #[must_use]
    pub const fn new(value: Uuid) -> Self {
        Self(value)
    }

    /// Returns the underlying opaque identity.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

#[derive(Default)]
struct EventArgsInner {
    values: Vec<EventValue>,
    scope_subject: Option<EventSubjectToken>,
}

/// Ordered event argument tuple.
#[derive(Clone, Default)]
pub struct EventArgs(Arc<EventArgsInner>);

impl std::fmt::Debug for EventArgs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("EventArgs")
            .field(&self.0.values.len())
            .field(&self.0.scope_subject)
            .finish()
    }
}

impl EventArgs {
    /// Creates an empty argument list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an argument list from erased values.
    #[must_use]
    pub fn from_values(values: Vec<EventValue>) -> Self {
        Self(Arc::new(EventArgsInner {
            values,
            scope_subject: None,
        }))
    }

    /// Creates a single-value argument list.
    #[must_use]
    pub fn one<T: Any + Send + Sync>(value: T) -> Self {
        Self::from_values(vec![Arc::new(value)])
    }

    /// Creates a single-value list without adding another [`Arc`] layer.
    #[must_use]
    pub fn one_shared<T: Any + Send + Sync>(value: Arc<T>) -> Self {
        Self::from_values(vec![value])
    }

    /// Returns a cloned typed argument.
    #[must_use]
    pub fn get<T: Any + Send + Sync>(&self, index: usize) -> Option<Arc<T>> {
        Arc::downcast::<T>(self.0.values.get(index)?.clone()).ok()
    }

    /// Attaches the payload-derived subject used by scoped dispatch invariants.
    #[must_use]
    pub fn with_scope_subject(self, subject: EventSubjectToken) -> Self {
        Self(Arc::new(EventArgsInner {
            values: self.0.values.clone(),
            scope_subject: Some(subject),
        }))
    }

    /// Returns the payload-derived subject for a scope-filtered event.
    #[must_use]
    pub fn scope_subject(&self) -> Option<EventSubjectToken> {
        self.0.scope_subject
    }

    /// Number of arguments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.values.len()
    }

    /// Whether no arguments are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.values.is_empty()
    }
}

/// Listener return value with JavaScript bail-value distinctions preserved.
#[derive(Clone, Default)]
pub enum EventReply {
    /// JavaScript `undefined`.
    #[default]
    Undefined,
    /// JavaScript `null`.
    Null,
    /// Boolean false.
    False,
    /// Any other value, including boolean true and zero.
    Value(EventValue),
}

impl std::fmt::Debug for EventReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Undefined => formatter.write_str("Undefined"),
            Self::Null => formatter.write_str("Null"),
            Self::False => formatter.write_str("False"),
            Self::Value(_) => formatter.write_str("Value(..)"),
        }
    }
}

impl EventReply {
    /// Returns true unless this is null, false, or undefined.
    #[must_use]
    pub fn is_bailed(&self) -> bool {
        matches!(self, Self::Value(_))
    }

    /// Extracts a cloned typed value.
    #[must_use]
    pub fn downcast<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        match self {
            Self::Value(value) => Arc::downcast::<T>(value.clone()).ok(),
            Self::Undefined | Self::Null | Self::False => None,
        }
    }
}

/// Boxed listener computation.
pub type ListenerFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<EventReply>> + Send + 'static>>;

type Listener = Arc<dyn Fn(Context, EventArgs) -> ListenerFuture + Send + Sync>;
type WaterfallListener = Arc<dyn Fn(Context, EventArgs, Next) -> ListenerFuture + Send + Sync>;

/// One-shot continuation passed to waterfall middleware.
pub struct Next(Box<dyn FnOnce(Option<EventArgs>) -> ListenerFuture + Send>);

impl Next {
    /// Invokes the remaining middleware or the innermost behavior.
    pub fn run(self) -> ListenerFuture {
        (self.0)(None)
    }

    /// Invokes the remaining middleware with replacement event arguments.
    ///
    /// This is the Rust counterpart of mutating a JavaScript waterfall's
    /// shared argument object before calling `next()`.
    pub fn run_with(self, args: EventArgs) -> ListenerFuture {
        (self.0)(Some(args))
    }
}

/// Event dispatch strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchMode {
    /// Synchronous invocation; returned asynchronous work is detached.
    Emit,
    /// All listeners run and settle together.
    Parallel,
    /// Listeners run in order until one bails.
    Serial,
    /// Synchronous-order bail dispatch.
    Bail,
    /// Around-middleware composition.
    Waterfall,
}

/// Listener registration options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventOptions {
    /// Places the listener before existing registrations.
    pub prepend: bool,
    /// Bypasses dispatch-context filtering.
    pub global: bool,
}

#[derive(Clone)]
struct Hook {
    id: Uuid,
    owner: Context,
    options: EventOptions,
    listener: Listener,
}

#[derive(Clone)]
struct WaterfallHook {
    id: Uuid,
    owner: Context,
    options: EventOptions,
    listener: WaterfallListener,
}

/// Root-owned event registry.
#[derive(Default)]
pub struct EventBus {
    hooks: Arc<RwLock<HashMap<String, Vec<Hook>>>>,
    waterfall_hooks: Arc<RwLock<HashMap<String, Vec<WaterfallHook>>>>,
}

/// Immutable listener snapshot prepared at a transactional dispatch boundary.
///
/// Preparing and invoking are separate so a caller can resolve listener
/// visibility before committing state, then notify those exact listeners only
/// after the commit becomes observable.
pub struct PreparedEmission {
    context: Context,
    args: EventArgs,
    hooks: Vec<Hook>,
}

impl PreparedEmission {
    /// Invokes the captured listeners, propagating the first synchronous
    /// failure while detaching asynchronous work.
    ///
    /// # Errors
    ///
    /// Returns the first synchronous listener failure or panic.
    pub fn emit(self) -> anyhow::Result<()> {
        for hook in self.hooks {
            invoke_emitted_hook(&hook, &self.context, &self.args)?;
        }
        Ok(())
    }

    /// Invokes every captured observer and contains synchronous failures.
    pub fn emit_contained(self, mut on_error: impl FnMut(anyhow::Error)) {
        for hook in self.hooks {
            if let Err(error) = invoke_emitted_hook(&hook, &self.context, &self.args) {
                on_error(error);
            }
        }
    }

    /// Invokes every captured observer, containing immediate failures through
    /// `on_error` and failures from detached listener work through
    /// `on_async_error`.
    ///
    /// This preserves JavaScript emit semantics for callers that attach a
    /// rejection handler to promise-like listener returns while still needing
    /// to distinguish failures thrown before the first asynchronous yield.
    pub fn emit_contained_with_async_errors(
        self,
        mut on_error: impl FnMut(anyhow::Error),
        on_async_error: &Arc<dyn Fn(anyhow::Error) + Send + Sync>,
    ) {
        for hook in self.hooks {
            if let Err(error) = invoke_emitted_hook_with_async_error(
                &hook,
                &self.context,
                &self.args,
                on_async_error.clone(),
            ) {
                on_error(error);
            }
        }
    }
}

impl EventBus {
    /// Creates an empty event bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an asynchronous listener owned by `context`.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] if the owning context is inactive.
    pub fn on(
        &self,
        context: &Context,
        name: impl Into<String>,
        listener: impl Fn(Context, EventArgs) -> ListenerFuture + Send + Sync + 'static,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError> {
        let name = name.into();
        let id = Uuid::now_v7();
        let hook = Hook {
            id,
            owner: context.clone(),
            options,
            listener: Arc::new(listener),
        };
        let mut hooks = self.hooks.write();
        let entries = hooks.entry(name.clone()).or_default();
        if options.prepend {
            entries.insert(0, hook);
        } else {
            entries.push(hook);
        }
        drop(hooks);

        let registry = self.hooks.clone();
        let effect = EffectHandle::synchronous(format!("ctx.on({name:?})"), move || {
            remove_hook(&registry, &name, id);
            Ok(())
        });
        match context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                // The owner became inactive between insertion and ownership.
                // The effect has not escaped, so synchronous removal is safe.
                futures::executor::block_on(effect.dispose()).ok();
                Err(error)
            }
        }
    }

    /// Registers a synchronous listener.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] if the owning context is inactive.
    pub fn on_sync(
        &self,
        context: &Context,
        name: impl Into<String>,
        listener: impl Fn(Context, EventArgs) -> anyhow::Result<EventReply> + Send + Sync + 'static,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError> {
        let listener = Arc::new(listener);
        self.on(
            context,
            name,
            move |context, args| {
                let listener = listener.clone();
                let result = listener(context, args);
                Box::pin(async move { result })
            },
            options,
        )
    }

    /// Registers waterfall middleware.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] if the owning context is inactive.
    pub fn on_waterfall(
        &self,
        context: &Context,
        name: impl Into<String>,
        listener: impl Fn(Context, EventArgs, Next) -> ListenerFuture + Send + Sync + 'static,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError> {
        let name = name.into();
        let id = Uuid::now_v7();
        let hook = WaterfallHook {
            id,
            owner: context.clone(),
            options,
            listener: Arc::new(listener),
        };
        let mut hooks = self.waterfall_hooks.write();
        let entries = hooks.entry(name.clone()).or_default();
        if options.prepend {
            entries.insert(0, hook);
        } else {
            entries.push(hook);
        }
        drop(hooks);

        let registry = self.waterfall_hooks.clone();
        let effect = EffectHandle::synchronous(format!("ctx.on({name:?})"), move || {
            remove_waterfall_hook(&registry, &name, id);
            Ok(())
        });
        match context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                futures::executor::block_on(effect.dispose()).ok();
                Err(error)
            }
        }
    }

    /// Invokes listeners immediately and detaches returned asynchronous work.
    ///
    /// # Errors
    ///
    /// Returns a synchronous listener failure or panic before detaching pending work.
    pub fn emit(
        &self,
        dispatch_context: &Context,
        name: &str,
        args: &EventArgs,
    ) -> anyhow::Result<()> {
        self.prepare_emit(dispatch_context, name, args)?.emit()
    }

    /// Captures the exact listener set visible at this instant.
    ///
    /// # Errors
    ///
    /// Returns a synchronous `internal/dispatch` interception failure.
    pub fn prepare_emit(
        &self,
        dispatch_context: &Context,
        name: &str,
        args: &EventArgs,
    ) -> anyhow::Result<PreparedEmission> {
        self.notify_internal_dispatch(dispatch_context, DispatchMode::Emit, name, args)?;
        Ok(PreparedEmission {
            context: dispatch_context.clone(),
            args: args.clone(),
            hooks: self.selected(dispatch_context, name),
        })
    }

    /// Awaits all selected listeners and aggregates failures.
    ///
    /// # Errors
    ///
    /// Returns an aggregate error after every selected listener settles when any fail.
    pub async fn parallel(
        &self,
        dispatch_context: &Context,
        name: &str,
        args: &EventArgs,
    ) -> anyhow::Result<()> {
        self.notify_internal_dispatch(dispatch_context, DispatchMode::Parallel, name, args)?;
        let futures = self
            .selected(dispatch_context, name)
            .into_iter()
            .map(|hook| {
                let future = catch_unwind(AssertUnwindSafe(|| {
                    (hook.listener)(dispatch_context.clone(), args.clone())
                }));
                async move {
                    let future = future.map_err(|payload| panic_error(&payload))?;
                    AssertUnwindSafe(future)
                        .catch_unwind()
                        .await
                        .map_err(|payload| panic_error(&payload))?
                }
            });
        let errors = join_all(futures)
            .await
            .into_iter()
            .filter_map(Result::err)
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("\n")))
        }
    }

    /// Awaits listeners in order and returns the first bail value.
    ///
    /// # Errors
    ///
    /// Returns the first listener failure.
    pub async fn serial(
        &self,
        dispatch_context: &Context,
        name: &str,
        args: &EventArgs,
    ) -> anyhow::Result<EventReply> {
        self.notify_internal_dispatch(dispatch_context, DispatchMode::Serial, name, args)?;
        for hook in self.selected(dispatch_context, name) {
            let reply = (hook.listener)(dispatch_context.clone(), args.clone()).await?;
            if reply.is_bailed() {
                return Ok(reply);
            }
        }
        Ok(EventReply::Undefined)
    }

    /// Composes waterfall listeners around `inner`.
    ///
    /// # Errors
    ///
    /// Returns a middleware or innermost-operation failure.
    pub async fn waterfall(
        &self,
        dispatch_context: &Context,
        name: &str,
        args: &EventArgs,
        inner: impl FnOnce() -> ListenerFuture + Send + 'static,
    ) -> anyhow::Result<EventReply> {
        self.waterfall_with_args(dispatch_context, name, args, move |_| inner())
            .await
    }

    /// Composes waterfall listeners around an argument-aware inner operation.
    ///
    /// Replacement arguments supplied through [`Next::run_with`] reach both
    /// downstream listeners and this innermost boundary.
    ///
    /// # Errors
    ///
    /// Returns a middleware or innermost-operation failure.
    pub async fn waterfall_with_args(
        &self,
        dispatch_context: &Context,
        name: &str,
        args: &EventArgs,
        inner: impl FnOnce(EventArgs) -> ListenerFuture + Send + 'static,
    ) -> anyhow::Result<EventReply> {
        self.notify_internal_dispatch(dispatch_context, DispatchMode::Waterfall, name, args)?;
        let hooks = self.selected_waterfall(dispatch_context, name);
        let hooks: Arc<[WaterfallHook]> = Arc::from(hooks);
        build_waterfall(
            &hooks,
            0,
            dispatch_context.clone(),
            args.clone(),
            Box::new(inner),
        )
        .await
    }

    /// Returns the number of listeners visible from one dispatch context.
    #[must_use]
    pub fn listener_count(&self, dispatch_context: &Context, name: &str) -> usize {
        self.selected(dispatch_context, name).len()
    }

    fn notify_internal_dispatch(
        &self,
        dispatch_context: &Context,
        mode: DispatchMode,
        name: &str,
        args: &EventArgs,
    ) -> anyhow::Result<()> {
        if name.starts_with("internal/") {
            return Ok(());
        }
        let diagnostic_args = EventArgs::from_values(vec![
            Arc::new(mode),
            Arc::new(name.to_owned()),
            Arc::new(args.clone()),
            Arc::new(dispatch_context.clone()),
        ]);
        for hook in self.selected(dispatch_context, "internal/dispatch") {
            invoke_emitted_hook(&hook, dispatch_context, &diagnostic_args)?;
        }
        Ok(())
    }

    fn selected(&self, dispatch_context: &Context, name: &str) -> Vec<Hook> {
        self.hooks
            .read()
            .get(name)
            .into_iter()
            .flatten()
            .filter(|hook| hook.options.global || dispatch_context.accepts_listener(&hook.owner))
            .cloned()
            .collect()
    }

    fn selected_waterfall(&self, dispatch_context: &Context, name: &str) -> Vec<WaterfallHook> {
        self.waterfall_hooks
            .read()
            .get(name)
            .into_iter()
            .flatten()
            .filter(|hook| hook.options.global || dispatch_context.accepts_listener(&hook.owner))
            .cloned()
            .collect()
    }
}

fn invoke_emitted_hook(
    hook: &Hook,
    dispatch_context: &Context,
    args: &EventArgs,
) -> anyhow::Result<()> {
    let mut future = catch_unwind(AssertUnwindSafe(|| {
        (hook.listener)(dispatch_context.clone(), args.clone())
    }))
    .map_err(|payload| panic_error(&payload))?;
    let mut task_context = TaskContext::from_waker(noop_waker_ref());
    match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut task_context)))
        .map_err(|payload| panic_error(&payload))?
    {
        Poll::Ready(result) => {
            result?;
        }
        Poll::Pending => detach_listener(future),
    }
    Ok(())
}

fn invoke_emitted_hook_with_async_error(
    hook: &Hook,
    dispatch_context: &Context,
    args: &EventArgs,
    on_async_error: Arc<dyn Fn(anyhow::Error) + Send + Sync>,
) -> anyhow::Result<()> {
    let mut future = catch_unwind(AssertUnwindSafe(|| {
        (hook.listener)(dispatch_context.clone(), args.clone())
    }))
    .map_err(|payload| panic_error(&payload))?;
    let mut task_context = TaskContext::from_waker(noop_waker_ref());
    match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut task_context)))
        .map_err(|payload| panic_error(&payload))?
    {
        Poll::Ready(result) => result.map(|_| ()),
        Poll::Pending => {
            detach_listener_with_error_handler(future, on_async_error);
            Ok(())
        }
    }
}

fn build_waterfall(
    hooks: &Arc<[WaterfallHook]>,
    index: usize,
    context: Context,
    args: EventArgs,
    inner: Box<dyn FnOnce(EventArgs) -> ListenerFuture + Send>,
) -> ListenerFuture {
    let Some(hook) = hooks.get(index).cloned() else {
        return inner(args);
    };
    let next_hooks = hooks.clone();
    let next_context = context.clone();
    let next_args = args.clone();
    let next = Next(Box::new(move |replacement| {
        build_waterfall(
            &next_hooks,
            index + 1,
            next_context,
            replacement.unwrap_or(next_args),
            inner,
        )
    }));
    (hook.listener)(context, args, next)
}

fn detach_listener(future: ListenerFuture) {
    let run = async move {
        if let Err(error) = future.await {
            tracing::error!(%error, "detached event listener failed");
        }
    };
    spawn_detached(run);
}

fn detach_listener_with_error_handler(
    future: ListenerFuture,
    on_error: Arc<dyn Fn(anyhow::Error) + Send + Sync>,
) {
    let run = async move {
        match AssertUnwindSafe(future).catch_unwind().await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => on_error(error),
            Err(payload) => on_error(panic_error(&payload)),
        }
    };
    spawn_detached(run);
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_detached(future: impl Future<Output = ()> + Send + 'static) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(future);
    } else {
        std::thread::spawn(move || futures::executor::block_on(future));
    }
}

#[cfg(target_arch = "wasm32")]
fn spawn_detached(future: impl Future<Output = ()> + Send + 'static) {
    wasm_bindgen_futures::spawn_local(future);
}

fn panic_error(payload: &Box<dyn Any + Send>) -> anyhow::Error {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("event listener panicked");
    anyhow::anyhow!(message.to_owned())
}

fn remove_hook(registry: &RwLock<HashMap<String, Vec<Hook>>>, name: &str, id: Uuid) {
    let mut hooks = registry.write();
    let Some(entries) = hooks.get_mut(name) else {
        return;
    };
    entries.retain(|hook| hook.id != id);
    if entries.is_empty() {
        hooks.remove(name);
    }
}

fn remove_waterfall_hook(
    registry: &RwLock<HashMap<String, Vec<WaterfallHook>>>,
    name: &str,
    id: Uuid,
) {
    let mut hooks = registry.write();
    let Some(entries) = hooks.get_mut(name) else {
        return;
    };
    entries.retain(|hook| hook.id != id);
    if entries.is_empty() {
        hooks.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::Fiber;

    use super::*;

    #[tokio::test]
    async fn serial_stops_on_first_bail_value() {
        let context = Context::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        for (label, reply) in [
            ("first", EventReply::False),
            ("second", EventReply::Value(Arc::new(7_u32))),
            ("third", EventReply::Undefined),
        ] {
            let calls = calls.clone();
            context
                .events()
                .on_sync(
                    &context,
                    "event",
                    move |_, _| {
                        calls.lock().expect("calls lock").push(label);
                        Ok(reply.clone())
                    },
                    EventOptions::default(),
                )
                .expect("active root");
        }

        let result = context
            .events()
            .serial(&context, "event", &EventArgs::new())
            .await
            .expect("dispatch succeeds");
        assert_eq!(result.downcast::<u32>().as_deref(), Some(&7));
        assert_eq!(*calls.lock().expect("calls lock"), ["first", "second"]);
    }

    #[tokio::test]
    async fn contained_emit_routes_detached_rejection_to_the_supplied_handler() {
        let context = Context::new();
        let events = context.events();
        events
            .on(
                &context,
                "example",
                |_, _| {
                    Box::pin(async {
                        tokio::task::yield_now().await;
                        anyhow::bail!("async observer failed")
                    })
                },
                EventOptions::default(),
            )
            .expect("listener");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let sender = Arc::new(parking_lot::Mutex::new(Some(sender)));
        let sender_for_handler = sender.clone();
        let handler: Arc<dyn Fn(anyhow::Error) + Send + Sync> = Arc::new(move |error| {
            if let Some(sender) = sender_for_handler.lock().take() {
                let _ = sender.send(error.to_string());
            }
        });

        events
            .prepare_emit(&context, "example", &EventArgs::new())
            .expect("prepare")
            .emit_contained_with_async_errors(
                |error| panic!("unexpected immediate error: {error:#}"),
                &handler,
            );

        assert_eq!(
            receiver.await.expect("handler called"),
            "async observer failed"
        );
    }

    #[tokio::test]
    async fn waterfall_delegates_only_when_next_is_called() {
        let context = Context::new();
        context
            .events()
            .on_waterfall(
                &context,
                "event",
                |_, _, next| {
                    Box::pin(async move {
                        let value = next.run().await?.downcast::<u32>().expect("inner number");
                        Ok(EventReply::Value(Arc::new(*value + 1)))
                    })
                },
                EventOptions::default(),
            )
            .expect("active root");

        let result = context
            .events()
            .waterfall(&context, "event", &EventArgs::new(), || {
                Box::pin(async { Ok(EventReply::Value(Arc::new(41_u32))) })
            })
            .await
            .expect("dispatch succeeds");
        assert_eq!(result.downcast::<u32>().as_deref(), Some(&42));
    }

    #[tokio::test]
    async fn waterfall_can_replace_arguments_before_delegating() {
        let context = Context::new();
        context
            .events()
            .on_waterfall(
                &context,
                "event",
                |_, _, next| Box::pin(async move { next.run_with(EventArgs::one(41_u32)).await }),
                EventOptions::default(),
            )
            .expect("first listener");
        context
            .events()
            .on_waterfall(
                &context,
                "event",
                |_, args, _| {
                    Box::pin(async move {
                        let value = args.get::<u32>(0).expect("replacement argument");
                        Ok(EventReply::Value(Arc::new(*value + 1)))
                    })
                },
                EventOptions::default(),
            )
            .expect("second listener");

        let result = context
            .events()
            .waterfall(&context, "event", &EventArgs::one(0_u32), || {
                Box::pin(async { anyhow::bail!("short-circuited listener must win") })
            })
            .await
            .expect("dispatch succeeds");
        assert_eq!(result.downcast::<u32>().as_deref(), Some(&42));

        let inner_context = Context::new();
        inner_context
            .events()
            .on_waterfall(
                &inner_context,
                "event",
                |_, _, next| Box::pin(async move { next.run_with(EventArgs::one(41_u32)).await }),
                EventOptions::default(),
            )
            .expect("replacement listener");
        let result = inner_context
            .events()
            .waterfall_with_args(&inner_context, "event", &EventArgs::one(0_u32), |args| {
                Box::pin(async move {
                    Ok(EventReply::Value(
                        args.get::<u32>(0).expect("inner argument"),
                    ))
                })
            })
            .await
            .expect("argument-aware inner succeeds");
        assert_eq!(result.downcast::<u32>().as_deref(), Some(&41));
    }

    #[tokio::test]
    async fn disposing_owner_removes_listeners_in_reverse_order() {
        let root = Context::new();
        let fiber = Fiber::child("plugin");
        let plugin = root.with_fiber(fiber.clone());
        plugin
            .events()
            .on_sync(
                &plugin,
                "event",
                |_, _| Ok(EventReply::Undefined),
                EventOptions::default(),
            )
            .expect("active plugin");
        fiber.dispose().await.expect("dispose succeeds");
        assert!(plugin.events().selected(&root, "event").is_empty());
    }
}
