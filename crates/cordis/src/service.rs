//! Type-safe service keys and the scoped service store.

use std::{
    any::Any,
    collections::HashMap,
    marker::PhantomData,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::RwLock;
use uuid::Uuid;

use crate::{Fiber, FiberState};

/// A thread-safe dynamically registered service.
pub trait Service: Any + Send + Sync {}

impl<T: Any + Send + Sync> Service for T {}

/// A stable typed name for a service slot.
#[derive(Debug)]
pub struct ServiceKey<T: Service> {
    name: &'static str,
    marker: PhantomData<fn() -> T>,
}

impl<T: Service> Copy for ServiceKey<T> {}

impl<T: Service> Clone for ServiceKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Service> ServiceKey<T> {
    /// Declares a typed service key.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            marker: PhantomData,
        }
    }

    /// Stable configuration and lookup name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ServiceSlot {
    pub(crate) name: String,
    pub(crate) isolation: Option<Uuid>,
}

struct Provider {
    id: Uuid,
    owner: Weak<Fiber>,
    value: Arc<dyn Any + Send + Sync>,
}

/// Root-owned stack of service providers.
pub(crate) struct ServiceStore {
    providers: RwLock<HashMap<ServiceSlot, Vec<Provider>>>,
    revision: AtomicU64,
}

impl Default for ServiceStore {
    fn default() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
            revision: AtomicU64::new(0),
        }
    }
}

impl ServiceStore {
    pub(crate) fn insert<T: Service>(
        &self,
        slot: ServiceSlot,
        owner: &Arc<Fiber>,
        value: Arc<T>,
    ) -> Option<Uuid> {
        let id = Uuid::now_v7();
        let mut providers = self.providers.write();
        let entries = providers.entry(slot).or_default();
        if !entries.is_empty() {
            return None;
        }
        entries.push(Provider {
            id,
            owner: Arc::downgrade(owner),
            value,
        });
        self.revision.fetch_add(1, Ordering::AcqRel);
        Some(id)
    }

    pub(crate) fn remove(&self, slot: &ServiceSlot, id: Uuid) -> bool {
        let mut providers = self.providers.write();
        let Some(entries) = providers.get_mut(slot) else {
            return false;
        };
        let Some(index) = entries.iter().position(|provider| provider.id == id) else {
            return false;
        };
        entries.remove(index);
        if entries.is_empty() {
            providers.remove(slot);
        }
        self.revision.fetch_add(1, Ordering::AcqRel);
        true
    }

    pub(crate) fn get<T: Service>(&self, slot: &ServiceSlot, strict: bool) -> Option<Arc<T>> {
        let providers = self.providers.read();
        let provider = providers.get(slot)?.iter().rev().find(|provider| {
            !strict
                || provider
                    .owner
                    .upgrade()
                    .is_some_and(|fiber| fiber.state() == FiberState::Active)
        })?;
        let value = provider.value.clone();
        Arc::downcast::<T>(value).ok()
    }

    pub(crate) fn provider_id(&self, slot: &ServiceSlot, strict: bool) -> Option<Uuid> {
        self.providers
            .read()
            .get(slot)?
            .iter()
            .rev()
            .find(|provider| {
                !strict
                    || provider
                        .owner
                        .upgrade()
                        .is_some_and(|fiber| fiber.state() == FiberState::Active)
            })
            .map(|provider| provider.id)
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
}
