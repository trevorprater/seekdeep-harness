//! Target-neutral Client test doubles shared by Rust feature tests.

use std::{
    any::Any,
    collections::HashMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, ServiceKey};

mod fixtures;
mod settings_scope;
mod snapshot;
mod translate;
#[cfg(target_arch = "wasm32")]
mod wasm_browser_languages;
#[cfg(target_arch = "wasm32")]
mod wasm_remote;
#[cfg(target_arch = "wasm32")]
mod wasm_sessions;
#[cfg(target_arch = "wasm32")]
mod wasm_workspaces;
#[cfg(not(target_arch = "wasm32"))]
mod workspaces;

pub use fixtures::*;
pub use settings_scope::*;
pub use snapshot::*;
pub use translate::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_browser_languages::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_remote::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_sessions::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_workspaces::*;
#[cfg(not(target_arch = "wasm32"))]
pub use workspaces::*;

/// Typed test-only seat corresponding to `ctx.remote`.
pub const TEST_REMOTE: ServiceKey<TestRemote> = ServiceKey::new("remote");

/// One opaque forwarded event argument.
pub type TestRemoteArgument = Arc<dyn Any + Send + Sync>;

/// Subscriber called synchronously with the exact forwarded argument slice.
pub type TestRemoteListener =
    Arc<dyn Fn(&[TestRemoteArgument]) -> anyhow::Result<()> + Send + Sync>;

/// Event-only `remote` test double used when a feature does not need generated namespaces.
pub struct TestRemote {
    subscriptions: Mutex<HashMap<String, Vec<TestRemoteListener>>>,
}

impl std::fmt::Debug for TestRemote {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestRemote")
            .field("events", &self.subscriptions.lock().keys())
            .finish()
    }
}

impl TestRemote {
    /// Constructs and publishes the double as `ctx.remote`.
    ///
    /// # Errors
    ///
    /// Returns the ordinary duplicate-service or inactive-Context failure.
    pub fn install(context: &Context) -> anyhow::Result<Arc<Self>> {
        let remote = Arc::new(Self {
            subscriptions: Mutex::new(HashMap::new()),
        });
        context.provide(TEST_REMOTE, remote.clone())?;
        Ok(remote)
    }

    /// Delivers one forwarded Host event to a registration-order snapshot.
    ///
    /// The first listener error propagates to the test. This deliberate difference from
    /// production prevents a feature test from treating this double as evidence for Remote
    /// listener containment.
    ///
    /// # Errors
    ///
    /// Returns the first subscriber failure without calling later subscribers.
    pub fn dispatch(&self, event: &str, arguments: &[TestRemoteArgument]) -> anyhow::Result<()> {
        let listeners = self
            .subscriptions
            .lock()
            .get(event)
            .cloned()
            .unwrap_or_default();
        for listener in listeners {
            listener(arguments)?;
        }
        Ok(())
    }

    /// Subscribes one listener and returns its idempotent disposer.
    #[must_use]
    pub fn subscribe(
        self: &Arc<Self>,
        event: impl Into<String>,
        listener: TestRemoteListener,
    ) -> TestRemoteSubscription {
        let event = event.into();
        let mut subscriptions = self.subscriptions.lock();
        let listeners = subscriptions.entry(event.clone()).or_default();
        if !listeners
            .iter()
            .any(|registered| Arc::ptr_eq(registered, &listener))
        {
            listeners.push(listener.clone());
        }
        drop(subscriptions);
        TestRemoteSubscription {
            remote: Arc::downgrade(self),
            event,
            listener,
            disposed: AtomicBool::new(false),
        }
    }

    /// Refuses generated namespace mounting; use the real Client Remote service for that path.
    ///
    /// # Errors
    ///
    /// Always returns the source diagnostic.
    pub fn mount(&self) -> anyhow::Result<()> {
        anyhow::bail!("TestRemote: $mount needs the real Client Remote service")
    }

    fn unsubscribe(&self, event: &str, listener: &TestRemoteListener) {
        let mut subscriptions = self.subscriptions.lock();
        let Some(listeners) = subscriptions.get_mut(event) else {
            return;
        };
        listeners.retain(|registered| !Arc::ptr_eq(registered, listener));
        if listeners.is_empty() {
            subscriptions.remove(event);
        }
    }
}

/// Explicit subscription lifetime returned by [`TestRemote::subscribe`].
pub struct TestRemoteSubscription {
    remote: Weak<TestRemote>,
    event: String,
    listener: TestRemoteListener,
    disposed: AtomicBool,
}

impl std::fmt::Debug for TestRemoteSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestRemoteSubscription")
            .field("event", &self.event)
            .field("disposed", &self.disposed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl TestRemoteSubscription {
    /// Removes this exact listener; repeated calls are inert.
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(remote) = self.remote.upgrade() {
            remote.unsubscribe(&self.event, &self.listener);
        }
    }
}
