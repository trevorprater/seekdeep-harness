//! Rust typestate facade for kind-correct Slot declarations and registrations.

use std::marker::PhantomData;

use crate::{
    SlotKind, SlotName, SlotRegistrationOptions, SlotScope, SlotSpec, SlotStoreDeclaration,
};

/// Single-occupant Slot marker.
#[derive(Clone, Copy, Debug)]
pub struct SingleSlot;

/// Ordered-list Slot marker.
#[derive(Clone, Copy, Debug)]
pub struct ListSlot;

/// Key-dispatched Slot marker.
#[derive(Clone, Copy, Debug)]
pub struct KeyedSlot;

/// Selector-routed chain Slot marker.
#[derive(Clone, Copy, Debug)]
pub struct ChainSlot;

/// One statically kinded Slot declaration.
#[derive(Clone, Debug)]
pub struct TypedSlot<S> {
    name: SlotName,
    scope: SlotScope,
    marker: PhantomData<S>,
}

impl<S> TypedSlot<S> {
    /// Exact Slot identity.
    #[must_use]
    pub fn name(&self) -> &SlotName {
        &self.name
    }
}

macro_rules! typed_slot {
    ($marker:ty, $kind:expr) => {
        impl TypedSlot<$marker> {
            /// Declares one Slot of this fixed kind and runtime scope.
            #[must_use]
            pub fn new(name: impl Into<String>, scope: SlotScope) -> Self {
                Self {
                    name: SlotName::new(name),
                    scope,
                    marker: PhantomData,
                }
            }

            /// Runtime declaration with an optional parent-supplied inject face.
            #[must_use]
            pub fn spec<I>(&self, inject: Option<I>) -> SlotSpec<I> {
                SlotSpec {
                    kind: $kind,
                    scope: self.scope,
                    inject,
                }
            }
        }
    };
}

typed_slot!(SingleSlot, SlotKind::Single);
typed_slot!(ListSlot, SlotKind::List);
typed_slot!(KeyedSlot, SlotKind::Keyed);
typed_slot!(ChainSlot, SlotKind::Chain);

impl TypedSlot<SingleSlot> {
    /// Begins a kind-correct single-cell registration.
    #[must_use]
    pub fn registration<I>(&self) -> TypedRegistration<SingleSlot, I> {
        TypedRegistration::new(&self.name)
    }
}

impl TypedSlot<ListSlot> {
    /// Begins a list registration with its mandatory cell id.
    #[must_use]
    pub fn registration<I>(&self, id: impl Into<String>) -> TypedRegistration<ListSlot, I> {
        let mut registration = TypedRegistration::new(&self.name);
        registration.options.id = Some(id.into());
        registration
    }
}

impl TypedSlot<KeyedSlot> {
    /// Begins a keyed registration with its mandatory dispatch key.
    ///
    /// ```compile_fail
    /// use seekdeep_client_ui_slots::{KeyedSlot, SlotScope, TypedSlot};
    /// let slot = TypedSlot::<KeyedSlot>::new("tool.view", SlotScope::Session);
    /// let _missing_key = slot.registration::<()>();
    /// ```
    #[must_use]
    pub fn registration<I>(&self, key: impl Into<String>) -> TypedRegistration<KeyedSlot, I> {
        let mut registration = TypedRegistration::new(&self.name);
        registration.options.key = Some(key.into());
        registration
    }
}

impl TypedSlot<ChainSlot> {
    /// Begins a chain registration with its mandatory pure selector.
    ///
    /// ```compile_fail
    /// use seekdeep_client_ui_slots::{ChainSlot, SlotScope, TypedSlot};
    /// let slot = TypedSlot::<ChainSlot>::new("conversation.takeover", SlotScope::Session);
    /// let _missing_selector = slot.registration::<(), _>();
    /// ```
    #[must_use]
    pub fn registration<I, Selector>(
        &self,
        selector: Selector,
    ) -> TypedChainRegistration<I, Selector> {
        let mut registration = TypedRegistration::new(&self.name);
        registration.options.has_selector = true;
        TypedChainRegistration {
            registration,
            selector,
        }
    }
}

/// Chain builder retaining the mandatory selector beside type-erased core options.
#[derive(Clone, Debug)]
pub struct TypedChainRegistration<I, Selector> {
    registration: TypedRegistration<ChainSlot, I>,
    selector: Selector,
}

impl<I, Selector> TypedChainRegistration<I, Selector> {
    /// Sets chain election priority.
    #[must_use]
    pub fn priority(mut self, priority: f64) -> Self {
        self.registration.options.priority = Some(priority);
        self
    }

    /// Produces core options and the selector host payload together.
    #[must_use]
    pub fn into_parts(self) -> (SlotRegistrationOptions<I>, Selector) {
        (self.registration.options, self.selector)
    }
}

/// Common kind-safe registration builder.
#[derive(Clone, Debug)]
pub struct TypedRegistration<S, I> {
    options: SlotRegistrationOptions<I>,
    marker: PhantomData<S>,
}

impl<S, I> TypedRegistration<S, I> {
    fn new(name: &SlotName) -> Self {
        Self {
            options: SlotRegistrationOptions::new(name.as_str()),
            marker: PhantomData,
        }
    }

    /// Declares one typed child and thereby authorizes its renderer.
    #[must_use]
    pub fn child<Child>(mut self, child: &TypedSlot<Child>, spec: SlotSpec<I>) -> Self {
        self.options.children.insert(child.name.clone(), spec);
        self
    }

    /// Sets shadowing or chain priority.
    #[must_use]
    pub fn priority(mut self, priority: f64) -> Self {
        self.options.priority = Some(priority);
        self
    }

    /// Declares a Store seat.
    #[must_use]
    pub fn store(mut self, store: SlotStoreDeclaration) -> Self {
        self.options.store = Some(store);
        self
    }

    /// Stamps the diagnostic registrant.
    #[must_use]
    pub fn registrant(mut self, registrant: impl Into<String>) -> Self {
        self.options.registrant = Some(registrant.into());
        self
    }

    /// Declares a dictionary namespace.
    #[must_use]
    pub fn locale(mut self, locale: impl Into<String>) -> Self {
        self.options.locale = Some(locale.into());
        self
    }

    /// Produces the type-erased core options after kind-specific fields were supplied.
    #[must_use]
    pub fn into_options(self) -> SlotRegistrationOptions<I> {
        self.options
    }
}

impl<I> TypedRegistration<ListSlot, I> {
    /// Sets list display order. Callers use this only for `TypedSlot<ListSlot>` builders.
    #[must_use]
    pub fn list_order(mut self, order: f64) -> Self {
        self.options.order = Some(order);
        self
    }
}
