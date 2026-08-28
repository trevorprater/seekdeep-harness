//! Portable popup-select controller with binding-identity settlement ownership.

use std::{
    any::Any,
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

use futures::{FutureExt as _, future::LocalBoxFuture};
use serde_json::Value;

use crate::{PopupState, PopupStatus, SelectOption, filter_options};
use seekdeep_client_ui_input_trigger::TokenSpan;

/// Open-time command token guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PopupTokenSegment {
    /// Menu pick carries a revision-stamped span.
    Menu(TokenSpan),
    /// Bare Enter carries the exact token.
    Enter(String),
}

/// One options request's cancellation handle.
pub trait PopupAbortHandle: Any {
    /// Aborts the request once.
    fn abort(&self);
    /// Downcast support for target adapters.
    fn as_any(&self) -> &dyn Any;
}

/// Creates target-specific cancellation handles.
pub trait PopupAbortFactory {
    /// Creates one fresh binding signal.
    fn create(&self) -> Rc<dyn PopupAbortHandle>;
}

/// Business callbacks captured by one popup binding.
pub trait PopupBusinessSpec {
    /// Loads option rows once per open/retry.
    fn options(
        &self,
        context: Value,
        signal: Rc<dyn PopupAbortHandle>,
    ) -> LocalBoxFuture<'static, Result<Vec<SelectOption>, String>>;

    /// Applies one admitted option.
    fn on_select(
        &self,
        option: SelectOption,
        context: Value,
    ) -> LocalBoxFuture<'static, Result<(), String>>;
}

/// Session wiring callbacks.
pub trait PopupSelectDeps {
    /// Consumes the open-time token after business success.
    fn consume(&self, segment: &PopupTokenSegment) -> bool;
    /// Restores composer focus.
    fn focus_composer(&self);
}

/// Owns fire-and-forget options requests.
pub trait PopupTaskSpawner {
    /// Owns one local task until settlement.
    fn spawn(&self, task: LocalBoxFuture<'static, ()>);
}

type Listener = Rc<dyn Fn()>;

struct StateStore {
    snapshot: Rc<PopupState>,
    listeners: BTreeMap<u64, Listener>,
    next_listener: u64,
}

struct Binding {
    id: u64,
    command: String,
    spec: Rc<dyn PopupBusinessSpec>,
    context: Value,
    segment: PopupTokenSegment,
    abort: Rc<dyn PopupAbortHandle>,
}

/// One Session's shared popup shell controller.
pub struct PopupSelectController {
    deps: Rc<dyn PopupSelectDeps>,
    abort_factory: Rc<dyn PopupAbortFactory>,
    spawner: Rc<dyn PopupTaskSpawner>,
    state: RefCell<StateStore>,
    binding: RefCell<Option<Binding>>,
    next_binding: RefCell<u64>,
}

impl PopupSelectController {
    /// Creates a closed controller.
    #[must_use]
    pub fn new(
        deps: Rc<dyn PopupSelectDeps>,
        abort_factory: Rc<dyn PopupAbortFactory>,
        spawner: Rc<dyn PopupTaskSpawner>,
    ) -> Rc<Self> {
        Rc::new(Self {
            deps,
            abort_factory,
            spawner,
            state: RefCell::new(StateStore {
                snapshot: Rc::new(PopupState::default()),
                listeners: BTreeMap::new(),
                next_listener: 0,
            }),
            binding: RefCell::new(None),
            next_binding: RefCell::new(0),
        })
    }

    /// Returns stable snapshot identity until mutation.
    #[must_use]
    pub fn snapshot(&self) -> Rc<PopupState> {
        self.state.borrow().snapshot.clone()
    }

    /// Subscribes to every synchronous replacement.
    ///
    /// # Panics
    ///
    /// Panics after exhausting every `u64` listener id rather than aliasing listeners.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: Listener) -> PopupSubscription {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state
                .next_listener
                .checked_add(1)
                .expect("popup listener id exhausted");
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        PopupSubscription {
            controller: Rc::downgrade(self),
            id: Some(id),
        }
    }

    /// Opens and starts the first options request, superseding any prior binding.
    ///
    /// # Panics
    ///
    /// Panics after exhausting every `u64` binding id rather than aliasing settlement rights.
    pub fn open(
        self: &Rc<Self>,
        command: String,
        spec: Rc<dyn PopupBusinessSpec>,
        context: Value,
        segment: PopupTokenSegment,
    ) {
        if let Some(binding) = self.binding.borrow_mut().take() {
            binding.abort.abort();
        }
        let id = {
            let mut next = self.next_binding.borrow_mut();
            *next = next.checked_add(1).expect("popup binding id exhausted");
            *next
        };
        let binding = Binding {
            id,
            command: command.clone(),
            spec,
            context,
            segment,
            abort: self.abort_factory.create(),
        };
        *self.binding.borrow_mut() = Some(binding);
        self.set(PopupState {
            open: true,
            command: Some(command),
            ..PopupState::default()
        });
        self.load(id);
    }

    /// Retries only a failed options request while retaining search.
    pub fn retry(self: &Rc<Self>) {
        let Some(id) = self.binding.borrow().as_ref().map(|binding| binding.id) else {
            return;
        };
        let state = self.snapshot();
        if !state.open || state.status != PopupStatus::Failed {
            return;
        }
        self.update(|state| {
            state.status = PopupStatus::Pending;
            state.error = None;
        });
        self.load(id);
    }

    /// Replaces local search and rebases highlight.
    pub fn set_search(&self, search: String) {
        let state = self.snapshot();
        if !state.open || state.submitting || state.confirming.is_some() || state.search == search {
            return;
        }
        self.update(move |state| {
            state.search = search;
            state.active = 0;
        });
    }

    /// Cycles highlight over filtered rows.
    pub fn move_highlight(&self, direction: i8) {
        let state = self.snapshot();
        if !state.open
            || state.status != PopupStatus::Ready
            || state.submitting
            || state.confirming.is_some()
        {
            return;
        }
        let rows = filter_options(&state.options, &state.search);
        if rows.is_empty() {
            return;
        }
        let len = rows.len();
        let active = if direction < 0 {
            (state.active + len - 1) % len
        } else {
            (state.active + 1) % len
        };
        self.update(|state| state.active = active);
    }

    /// Sets one in-range filtered highlight.
    pub fn highlight(&self, index: usize) {
        let state = self.snapshot();
        if !state.open
            || state.status != PopupStatus::Ready
            || state.submitting
            || state.confirming.is_some()
            || index >= filter_options(&state.options, &state.search).len()
            || index == state.active
        {
            return;
        }
        self.update(|state| state.active = index);
    }

    /// Selects one filtered row or opens its confirmation gate.
    #[must_use]
    pub fn select(self: &Rc<Self>, index: usize) -> LocalBoxFuture<'static, ()> {
        let state = self.snapshot();
        let Some(binding_id) = self.binding.borrow().as_ref().map(|binding| binding.id) else {
            return futures::future::ready(()).boxed_local();
        };
        if !state.open
            || state.status != PopupStatus::Ready
            || state.submitting
            || state.confirming.is_some()
        {
            return futures::future::ready(()).boxed_local();
        }
        let option = filter_options(&state.options, &state.search)
            .get(index)
            .cloned();
        let Some(option) = option else {
            return futures::future::ready(()).boxed_local();
        };
        if option.confirmation.is_some() {
            self.update(move |state| {
                state.confirming = Some(option);
                state.acknowledged = false;
                state.error = None;
            });
            return futures::future::ready(()).boxed_local();
        }
        self.settle(binding_id, &option)
    }

    /// Updates the active risk acknowledgement.
    pub fn acknowledge(&self, acknowledged: bool) {
        let state = self.snapshot();
        if !state.open
            || state.submitting
            || state.confirming.is_none()
            || state.acknowledged == acknowledged
        {
            return;
        }
        self.update(|state| state.acknowledged = acknowledged);
    }

    /// Cancels only the confirmation gate.
    pub fn cancel_confirmation(&self) {
        let state = self.snapshot();
        if !state.open || state.submitting || state.confirming.is_none() {
            return;
        }
        self.update(|state| {
            state.confirming = None;
            state.acknowledged = false;
        });
    }

    /// Settles the gated option only after acknowledgement.
    #[must_use]
    pub fn confirm(self: &Rc<Self>) -> LocalBoxFuture<'static, ()> {
        let state = self.snapshot();
        let Some(binding_id) = self.binding.borrow().as_ref().map(|binding| binding.id) else {
            return futures::future::ready(()).boxed_local();
        };
        if !state.open || state.submitting || !state.acknowledged {
            return futures::future::ready(()).boxed_local();
        }
        let Some(option) = state.confirming.clone() else {
            return futures::future::ready(()).boxed_local();
        };
        self.settle(binding_id, &option)
    }

    /// Closes and optionally restores composer focus.
    pub fn dismiss(&self, focus_composer: bool) {
        let Some(binding) = self.binding.borrow_mut().take() else {
            return;
        };
        binding.abort.abort();
        self.set(PopupState::default());
        if focus_composer {
            self.deps.focus_composer();
        }
    }

    /// Scope teardown without focus side effects.
    pub fn dispose(&self) {
        if let Some(binding) = self.binding.borrow_mut().take() {
            binding.abort.abort();
        }
        self.set(PopupState::default());
    }

    fn load(self: &Rc<Self>, binding_id: u64) {
        let call = self.binding.borrow().as_ref().and_then(|binding| {
            (binding.id == binding_id).then(|| {
                (
                    binding.spec.clone(),
                    binding.context.clone(),
                    binding.abort.clone(),
                    binding.command.clone(),
                )
            })
        });
        let Some((spec, context, abort, _command)) = call else {
            return;
        };
        let controller = Rc::downgrade(self);
        self.spawner.spawn(
            async move {
                let result = spec.options(context, abort).await;
                let Some(controller) = controller.upgrade() else {
                    return;
                };
                if controller.binding_id() != Some(binding_id) {
                    return;
                }
                match result {
                    Ok(options) => controller.update(move |state| {
                        state.status = PopupStatus::Ready;
                        state.options = options;
                        state.active = 0;
                        state.error = None;
                    }),
                    Err(error) => controller.update(move |state| {
                        state.status = PopupStatus::Failed;
                        state.options.clear();
                        state.active = 0;
                        state.error = Some(error);
                    }),
                }
            }
            .boxed_local(),
        );
    }

    fn settle(
        self: &Rc<Self>,
        binding_id: u64,
        option: &SelectOption,
    ) -> LocalBoxFuture<'static, ()> {
        let call = self.binding.borrow().as_ref().and_then(|binding| {
            (binding.id == binding_id).then(|| {
                (
                    binding.spec.clone(),
                    binding.context.clone(),
                    binding.segment.clone(),
                )
            })
        });
        let Some((spec, context, segment)) = call else {
            return futures::future::ready(()).boxed_local();
        };
        let selected = option.clone();
        self.update(|state| {
            state.submitting = true;
            state.confirming = None;
            state.acknowledged = false;
            state.error = None;
        });
        let controller = self.clone();
        async move {
            if let Err(error) = spec.on_select(selected, context).await {
                if controller.binding_id() == Some(binding_id) {
                    controller.update(move |state| {
                        state.submitting = false;
                        state.error = Some(error);
                    });
                }
                return;
            }
            if controller.binding_id() != Some(binding_id) {
                return;
            }
            controller.deps.consume(&segment);
            controller.binding.borrow_mut().take();
            controller.set(PopupState::default());
            controller.deps.focus_composer();
        }
        .boxed_local()
    }

    fn binding_id(&self) -> Option<u64> {
        self.binding.borrow().as_ref().map(|binding| binding.id)
    }

    fn set(&self, state: PopupState) {
        let listeners = {
            let mut store = self.state.borrow_mut();
            store.snapshot = Rc::new(state);
            store.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }

    fn update(&self, mutate: impl FnOnce(&mut PopupState)) {
        let mut state = (*self.snapshot()).clone();
        mutate(&mut state);
        self.set(state);
    }

    fn unsubscribe(&self, id: u64) {
        self.state.borrow_mut().listeners.remove(&id);
    }
}

/// Idempotent popup Store subscription.
pub struct PopupSubscription {
    controller: Weak<PopupSelectController>,
    id: Option<u64>,
}

impl Drop for PopupSubscription {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        if let Some(controller) = self.controller.upgrade() {
            controller.unsubscribe(id);
        }
    }
}
