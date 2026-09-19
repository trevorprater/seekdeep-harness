//! Per-Session standard-props provider roster and atomic current projection.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

use indexmap::IndexMap;
use seekdeep_identity::SessionId;

/// Stable assembly handle passed to every provider resolver.
#[derive(Clone)]
pub struct SessionBinding<Hook, Projections, Payload = ()> {
    /// Owning Session identity.
    pub session_id: SessionId,
    /// Outward Session observable and behavior face.
    pub session: Hook,
    /// Open-key projection-face collection.
    pub projections: Projections,
    /// Target-specific complete binding retained for compatibility resolvers.
    pub payload: Payload,
}

/// One provider's resolved hook and plain-prop members.
pub struct SessionProvideContribution<Hook, Prop> {
    /// Bare observable sources by declared hook base name.
    pub hooks: IndexMap<String, Option<Hook>>,
    /// Plain members by declared prop name.
    pub props: IndexMap<String, Option<Prop>>,
}

impl<Hook, Prop> Default for SessionProvideContribution<Hook, Prop> {
    fn default() -> Self {
        Self {
            hooks: IndexMap::new(),
            props: IndexMap::new(),
        }
    }
}

type ProvideResolver<Hook, Prop, Projections, Payload> = Rc<
    dyn Fn(
        &SessionBinding<Hook, Projections, Payload>,
    ) -> Result<SessionProvideContribution<Hook, Prop>, SessionProvideError>,
>;

/// Static member roster plus one per-Session resolver.
pub struct SessionProvideDescriptor<Hook, Prop, Projections, Payload = ()> {
    /// Declared hook base names.
    pub hooks: Vec<String>,
    /// Declared plain-prop names.
    pub props: Vec<String>,
    /// Resolves every declared member for one definite Session.
    pub resolve: ProvideResolver<Hook, Prop, Projections, Payload>,
}

/// Current or no-Session standard-props bundle.
pub struct SessionProvideInfo<Hook, Prop, Projections> {
    /// Current Session identity, absent in the static no-Session projection.
    pub session_id: Option<SessionId>,
    /// Static hook roster; values are absent without a Session.
    pub hooks: IndexMap<String, Option<Hook>>,
    /// Static plain-prop roster; values are absent without a Session.
    pub props: IndexMap<String, Option<Prop>>,
    /// Open-key projection faces, absent without a Session.
    pub projections: Option<Projections>,
}

/// Owner-side live bundle storage and current-selection resolution.
pub trait SessionProvideChannelHost<Hook, Prop, Projections, Payload> {
    /// Re-materializes every already-live Session bundle.
    ///
    /// # Errors
    ///
    /// Returns the first provider resolution or owner storage failure.
    fn rebuild_bundles(
        &self,
        channel: &SessionProvideChannel<Hook, Prop, Projections, Payload>,
    ) -> Result<(), SessionProvideError>;

    /// Resolves the current selection's definite or absent bundle.
    ///
    /// # Errors
    ///
    /// Returns an owner-side selection resolution failure.
    fn resolve_current(
        &self,
    ) -> Result<Rc<SessionProvideInfo<Hook, Prop, Projections>>, SessionProvideError>;

    /// Reports one contained current-projection subscriber failure.
    fn report_subscriber_failure(&self, message: &str);
}

/// Fail-loud provider declaration or materialization error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SessionProvideError(String);

impl SessionProvideError {
    /// Wraps a target-specific resolver or host failure.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

struct Provider<Hook, Prop, Projections, Payload> {
    id: u64,
    descriptor: SessionProvideDescriptor<Hook, Prop, Projections, Payload>,
}

/// Fallible current-projection listener; failures are contained and reported.
pub type SessionProvideListener = Rc<dyn Fn() -> Result<(), String>>;

struct ChannelState<Hook, Prop, Projections, Payload> {
    providers: Vec<Provider<Hook, Prop, Projections, Payload>>,
    next_provider_id: u64,
    maybe_info: Rc<SessionProvideInfo<Hook, Prop, Projections>>,
    current_snapshot: Rc<SessionProvideInfo<Hook, Prop, Projections>>,
    listeners: BTreeMap<u64, SessionProvideListener>,
    next_listener_id: u64,
}

/// Provider roster, bundle materializer, and atomic current-Session source.
pub struct SessionProvideChannel<Hook, Prop, Projections, Payload = ()> {
    host: Rc<dyn SessionProvideChannelHost<Hook, Prop, Projections, Payload>>,
    state: RefCell<ChannelState<Hook, Prop, Projections, Payload>>,
}

impl<Hook, Prop, Projections, Payload> SessionProvideChannel<Hook, Prop, Projections, Payload>
where
    Hook: Clone + 'static,
    Prop: Clone + 'static,
    Projections: Clone + 'static,
    Payload: 'static,
{
    /// Creates a channel with the runtime-owned `session` hook first.
    #[must_use]
    pub fn new(
        host: Rc<dyn SessionProvideChannelHost<Hook, Prop, Projections, Payload>>,
    ) -> Rc<Self> {
        let built_in = Provider {
            id: 0,
            descriptor: SessionProvideDescriptor {
                hooks: vec!["session".to_owned()],
                props: Vec::new(),
                resolve: Rc::new(|binding: &SessionBinding<Hook, Projections, Payload>| {
                    Ok(SessionProvideContribution {
                        hooks: IndexMap::from([(
                            "session".to_owned(),
                            Some(binding.session.clone()),
                        )]),
                        props: IndexMap::new(),
                    })
                }),
            },
        };
        let maybe_info = Rc::new(SessionProvideInfo {
            session_id: None,
            hooks: IndexMap::from([("session".to_owned(), None)]),
            props: IndexMap::new(),
            projections: None,
        });
        Rc::new(Self {
            host,
            state: RefCell::new(ChannelState {
                providers: vec![built_in],
                next_provider_id: 0,
                maybe_info: maybe_info.clone(),
                current_snapshot: maybe_info,
                listeners: BTreeMap::new(),
                next_listener_id: 0,
            }),
        })
    }

    /// Returns the static no-Session projection under the current roster.
    #[must_use]
    pub fn maybe_info(&self) -> Rc<SessionProvideInfo<Hook, Prop, Projections>> {
        self.state.borrow().maybe_info.clone()
    }

    /// Returns the latest atomically published current bundle.
    #[must_use]
    pub fn current_snapshot(&self) -> Rc<SessionProvideInfo<Hook, Prop, Projections>> {
        self.state.borrow().current_snapshot.clone()
    }

    /// Subscribes to synchronous current-bundle identity changes.
    #[must_use]
    pub fn subscribe_current(
        self: &Rc<Self>,
        listener: SessionProvideListener,
    ) -> SessionProvideSubscription {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener_id = state.next_listener_id.wrapping_add(1);
            let id = state.next_listener_id;
            state.listeners.insert(id, listener);
            id
        };
        let channel = self.clone();
        SessionProvideSubscription {
            dispose: Rc::new(move || {
                channel.state.borrow_mut().listeners.remove(&id);
            }),
        }
    }

    /// Registers one provider and rolls back a roster that cannot materialize.
    ///
    /// # Errors
    ///
    /// Returns duplicate declarations or any live-bundle resolver failure.
    pub fn provide(
        self: &Rc<Self>,
        descriptor: SessionProvideDescriptor<Hook, Prop, Projections, Payload>,
    ) -> Result<SessionProvideRegistration, SessionProvideError> {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_provider_id = state.next_provider_id.wrapping_add(1);
            let id = state.next_provider_id;
            state.providers.push(Provider { id, descriptor });
            id
        };
        if let Err(error) = self.apply_roster_change() {
            self.remove_provider(id);
            self.apply_roster_change()?;
            return Err(error);
        }
        let channel = self.clone();
        Ok(SessionProvideRegistration {
            dispose: Rc::new(move || {
                channel.remove_provider(id);
                channel.apply_roster_change()
            }),
        })
    }

    /// Re-resolves and synchronously publishes the current bundle when identity changed.
    ///
    /// # Errors
    ///
    /// Returns a current-selection resolution failure.
    pub fn publish_current(&self) -> Result<(), SessionProvideError> {
        let next = self.host.resolve_current()?;
        let listeners = {
            let mut state = self.state.borrow_mut();
            if Rc::ptr_eq(&next, &state.current_snapshot) {
                return Ok(());
            }
            state.current_snapshot = next;
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            let outcome = catch_unwind(AssertUnwindSafe(|| listener()));
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.host.report_subscriber_failure(&error),
                Err(_) => self
                    .host
                    .report_subscriber_failure("currentProvideInfo subscriber panicked"),
            }
        }
        Ok(())
    }

    /// Materializes one definite Session bundle against the current roster.
    ///
    /// # Errors
    ///
    /// Returns undeclared, missing, duplicate, or resolver failures.
    pub fn materialize_info(
        &self,
        binding: &SessionBinding<Hook, Projections, Payload>,
    ) -> Result<Rc<SessionProvideInfo<Hook, Prop, Projections>>, SessionProvideError> {
        let state = self.state.borrow();
        let mut hooks = IndexMap::new();
        let mut props = IndexMap::new();
        for provider in &state.providers {
            let descriptor = &provider.descriptor;
            let contribution = (descriptor.resolve)(binding)?;
            for name in contribution.hooks.keys() {
                if !descriptor.hooks.contains(name) {
                    return Err(error(format!("undeclared hook \"{name}\"")));
                }
            }
            for name in contribution.props.keys() {
                if !descriptor.props.contains(name) {
                    return Err(error(format!("undeclared prop \"{name}\"")));
                }
            }
            for name in &descriptor.hooks {
                let Some(source) = contribution.hooks.get(name).and_then(Option::as_ref) else {
                    return Err(error(format!("missing hook \"{name}\"")));
                };
                if hooks.contains_key(name) {
                    return Err(error(format!("duplicate hook \"{name}\"")));
                }
                hooks.insert(name.clone(), Some(source.clone()));
            }
            for name in &descriptor.props {
                let Some(value) = contribution.props.get(name).and_then(Option::as_ref) else {
                    return Err(error(format!("missing prop \"{name}\"")));
                };
                if props.contains_key(name) {
                    return Err(error(format!("duplicate prop \"{name}\"")));
                }
                props.insert(name.clone(), Some(value.clone()));
            }
        }
        Ok(Rc::new(SessionProvideInfo {
            session_id: Some(binding.session_id.clone()),
            hooks,
            props,
            projections: Some(binding.projections.clone()),
        }))
    }

    fn apply_roster_change(&self) -> Result<(), SessionProvideError> {
        let maybe_info = self.materialize_maybe_info()?;
        self.state.borrow_mut().maybe_info = maybe_info;
        self.host.rebuild_bundles(self)?;
        self.publish_current()
    }

    fn materialize_maybe_info(
        &self,
    ) -> Result<Rc<SessionProvideInfo<Hook, Prop, Projections>>, SessionProvideError> {
        let state = self.state.borrow();
        let mut hooks = IndexMap::new();
        let mut props = IndexMap::new();
        for provider in &state.providers {
            for name in &provider.descriptor.hooks {
                if hooks.insert(name.clone(), None).is_some() {
                    return Err(error(format!("duplicate hook \"{name}\"")));
                }
            }
            for name in &provider.descriptor.props {
                if props.insert(name.clone(), None).is_some() {
                    return Err(error(format!("duplicate prop \"{name}\"")));
                }
            }
        }
        Ok(Rc::new(SessionProvideInfo {
            session_id: None,
            hooks,
            props,
            projections: None,
        }))
    }

    fn remove_provider(&self, id: u64) {
        let mut state = self.state.borrow_mut();
        if let Some(index) = state
            .providers
            .iter()
            .position(|provider| provider.id == id)
        {
            state.providers.remove(index);
        }
    }
}

fn error(detail: impl AsRef<str>) -> SessionProvideError {
    SessionProvideError(format!("sessions.provide: {}", detail.as_ref()))
}

/// Reusable unsubscribe handle; every call is harmless after the first removal.
pub struct SessionProvideSubscription {
    dispose: Rc<dyn Fn()>,
}

impl SessionProvideSubscription {
    /// Removes the current-projection subscriber.
    pub fn dispose(&self) {
        (self.dispose)();
    }
}

/// Provider disposer matching the source's repeatable roster rebuild behavior.
pub struct SessionProvideRegistration {
    dispose: Rc<dyn Fn() -> Result<(), SessionProvideError>>,
}

impl SessionProvideRegistration {
    /// Removes the provider when present, then reapplies the surviving roster.
    ///
    /// # Errors
    ///
    /// Returns a surviving provider or owner rebuild failure.
    pub fn dispose(&self) -> Result<(), SessionProvideError> {
        (self.dispose)()
    }
}
