//! Client runtime ownership above the portable Slot ledger.

use std::{
    any::Any,
    cell::RefCell,
    collections::HashMap,
    fmt,
    rc::{Rc, Weak},
};

use seekdeep_client_ui_slots::{
    SlotCore, SlotEntry, SlotMicrotaskScheduler, SlotName, SlotRegistrationOptions, SlotScope,
    SlotStoreDeclaration, SlotStoreFactory, SlotStoreInstance, SlotSubscription, StoreHandleId,
};

type DisposalCallback = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;
type InjectionSetup = Rc<dyn Fn(&mut SlotEffectBatch) -> Result<(), ClientSlotError>>;

/// Idempotent synchronous lifecycle effect.
#[derive(Clone)]
pub struct RuntimeDisposer {
    callback: DisposalCallback,
}

impl fmt::Debug for RuntimeDisposer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeDisposer")
            .field("active", &self.callback.borrow().is_some())
            .finish()
    }
}

impl RuntimeDisposer {
    /// Wraps one exact-once cleanup.
    #[must_use]
    pub fn new(callback: impl FnOnce() + 'static) -> Self {
        Self {
            callback: Rc::new(RefCell::new(Some(Box::new(callback)))),
        }
    }

    /// Runs cleanup at most once.
    pub fn dispose(&self) {
        if let Some(callback) = self.callback.borrow_mut().take() {
            callback();
        }
    }

    /// Whether cleanup remains active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.callback.borrow().is_some()
    }
}

/// Transactional declaration-injection setup ledger.
#[derive(Default)]
pub struct SlotEffectBatch {
    effects: Vec<RuntimeDisposer>,
}

impl SlotEffectBatch {
    /// Adds one already-installed reversible effect.
    pub fn push(&mut self, effect: RuntimeDisposer) {
        self.effects.push(effect);
    }

    fn dispose(self) {
        for effect in self.effects.into_iter().rev() {
            effect.dispose();
        }
    }
}

/// Runtime Slot service failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ClientSlotError(String);

impl ClientSlotError {
    /// Creates one stable diagnostic.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Delayed injection failures are contained, wrapped, and reported on the next microtask.
pub trait SlotInjectionFailureReporter {
    /// Publishes one delayed setup failure without interrupting sibling declaration listeners.
    fn report_later(&self, error: ClientSlotError);
}

/// Independent Session or Workspace standard-kit face.
pub trait RuntimeStandardFace: Any {
    /// Type-erased downcast support for browser Host adapters.
    fn as_any(&self) -> &dyn Any;
    /// Current JSON-compatible diagnostic snapshot.
    fn snapshot(&self) -> serde_json::Value;
}

/// Locale face installed independently from the cached renderer Host object.
pub trait RuntimeLocaleFace: Any {
    /// Type-erased downcast support for browser Host adapters.
    fn as_any(&self) -> &dyn Any;
    /// Current locale revision.
    fn revision(&self) -> u64;
}

/// Shared Store handle or per-registration factory.
#[derive(Clone)]
pub enum RuntimeStoreDeclaration {
    /// Reuses one handle across entries of the same scope.
    Shared(Rc<dyn SlotStoreFactory>),
    /// Mints an exclusive handle for each registration.
    Factory(Rc<dyn Fn() -> Rc<dyn SlotStoreFactory>>),
}

/// Host-specific Slot entry payload retained beside the portable ledger.
pub struct RuntimeSlotPayload<C> {
    /// Component or renderer-owned entry value.
    pub component: C,
    /// Resolvable Store handle after exclusive factory minting.
    pub store: Option<Rc<dyn SlotStoreFactory>>,
}

impl<C: fmt::Debug> fmt::Debug for RuntimeSlotPayload<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeSlotPayload")
            .field("component", &self.component)
            .field("store", &self.store.as_ref().map(|_| "handle"))
            .finish()
    }
}

type RuntimeCore<C, I, X> = SlotCore<RuntimeSlotPayload<C>, I, X>;
type RuntimeEntry<C, I> = SlotEntry<RuntimeSlotPayload<C>, I>;

/// Root renderer installed by the browser React binding layer.
pub trait ClientRootRenderer<C, I, X, Owner, Node> {
    /// Renders the root tree through the live runtime Host face.
    fn render_root(&self, host: &ClientSlotRegistry<C, I, X, Owner, Node>, owner: Owner) -> Node;
}

struct StoreAxis {
    id: StoreHandleId,
    handle: Rc<dyn SlotStoreFactory>,
    scope: SlotScope,
    refs: usize,
    instances: HashMap<String, Rc<dyn SlotStoreInstance>>,
}

struct RegistryState<C, I, X, Owner, Node> {
    stores: Vec<StoreAxis>,
    next_store: u64,
    renderer: Option<Rc<dyn ClientRootRenderer<C, I, X, Owner, Node>>>,
    locale: Option<Rc<dyn RuntimeLocaleFace>>,
    sessions: Option<Rc<dyn RuntimeStandardFace>>,
    workspaces: Option<Rc<dyn RuntimeStandardFace>>,
}

/// React-free runtime Service above one portable Slot core.
pub struct ClientSlotRegistry<C, I, X, Owner, Node> {
    core: Rc<RuntimeCore<C, I, X>>,
    state: RefCell<RegistryState<C, I, X, Owner, Node>>,
    mutation_subscription: RefCell<Option<SlotSubscription<RuntimeSlotPayload<C>, I, X>>>,
}

impl<C, I, X, Owner, Node> fmt::Debug for ClientSlotRegistry<C, I, X, Owner, Node> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.borrow();
        formatter
            .debug_struct("ClientSlotRegistry")
            .field("core", &self.core)
            .field("stores", &state.stores.len())
            .field("renderer", &state.renderer.is_some())
            .field("locale", &state.locale.is_some())
            .finish_non_exhaustive()
    }
}

impl<C, I, X, Owner, Node> ClientSlotRegistry<C, I, X, Owner, Node>
where
    C: 'static,
    I: Clone + 'static,
    X: 'static,
    Owner: 'static,
    Node: 'static,
{
    /// Creates the runtime Service and bridges every core mutation synchronously.
    #[must_use]
    pub fn new(
        scheduler: Rc<dyn SlotMicrotaskScheduler>,
        on_changed: Rc<dyn Fn(&SlotName)>,
    ) -> Rc<Self> {
        let core = SlotCore::new(scheduler);
        let registry = Rc::new(Self {
            core: core.clone(),
            state: RefCell::new(RegistryState {
                stores: Vec::new(),
                next_store: 0,
                renderer: None,
                locale: None,
                sessions: None,
                workspaces: None,
            }),
            mutation_subscription: RefCell::new(None),
        });
        let subscription = core.on_mutate(on_changed);
        *registry.mutation_subscription.borrow_mut() = Some(subscription);
        registry
    }

    /// Underlying portable ledger.
    #[must_use]
    pub fn core(&self) -> &Rc<RuntimeCore<C, I, X>> {
        &self.core
    }

    /// Registers one caller-owned entry and binds its Store instance axis.
    ///
    /// # Errors
    ///
    /// Returns portable core validation failures before the Service commits Store state.
    pub fn register(
        self: &Rc<Self>,
        mut options: SlotRegistrationOptions<I>,
        component: C,
        store: Option<RuntimeStoreDeclaration>,
    ) -> Result<RuntimeDisposer, ClientSlotError> {
        let handle = store.map(|store| match store {
            RuntimeStoreDeclaration::Shared(handle) => handle,
            RuntimeStoreDeclaration::Factory(factory) => factory(),
        });
        let store_id = handle.as_ref().map(|handle| self.store_id(handle));
        if let Some(id) = store_id {
            options.store = Some(SlotStoreDeclaration::Shared(id));
        }
        let target = options.name.clone();
        let registration = self
            .core
            .register(
                options,
                RuntimeSlotPayload {
                    component,
                    store: handle.clone(),
                },
            )
            .map_err(|error| ClientSlotError::new(error.to_string()))?;
        if let (Some(handle), Some(id)) = (&handle, store_id) {
            let Some(spec) = self.core.spec(&target) else {
                registration.dispose();
                return Err(ClientSlotError::new(format!(
                    "slot \"{target}\" became undeclared while registering its Store"
                )));
            };
            let scope = spec.scope;
            self.acquire_store(handle.clone(), id, scope);
        }
        let weak = Rc::downgrade(self);
        Ok(RuntimeDisposer::new(move || {
            registration.dispose();
            if let (Some(registry), Some(handle)) = (weak.upgrade(), handle) {
                registry.release_store(&handle);
            }
        }))
    }

    /// Installs an effect for each declaration lifetime of `key`.
    ///
    /// # Errors
    ///
    /// Propagates setup failure synchronously when the declaration already exists.
    pub fn inject(
        self: &Rc<Self>,
        key: SlotName,
        setup: InjectionSetup,
        reporter: Rc<dyn SlotInjectionFailureReporter>,
    ) -> Result<SlotInjection<C, I, X, Owner, Node>, ClientSlotError> {
        let controller = Rc::new(InjectionController {
            registry: Rc::downgrade(self),
            key: key.clone(),
            setup,
            reporter,
            state: RefCell::new(InjectionState {
                stopped: false,
                active_epoch: None,
                active: None,
                subscription: None,
            }),
        });
        let weak = Rc::downgrade(&controller);
        let subscription = self.core.subscribe_declaration(
            key,
            Rc::new(move || {
                let Some(controller) = weak.upgrade() else {
                    return;
                };
                if let Err(error) = controller.reconcile() {
                    controller.stop();
                    controller.reporter.report_later(error);
                }
            }),
        );
        controller.state.borrow_mut().subscription = Some(subscription);
        if let Err(error) = controller.reconcile() {
            controller.stop();
            return Err(error);
        }
        Ok(SlotInjection { controller })
    }

    /// Installs the root renderer once.
    ///
    /// # Errors
    ///
    /// Rejects a second live renderer.
    pub fn install_renderer(
        self: &Rc<Self>,
        renderer: Rc<dyn ClientRootRenderer<C, I, X, Owner, Node>>,
    ) -> Result<RuntimeDisposer, ClientSlotError> {
        if self.state.borrow().renderer.is_some() {
            return Err(ClientSlotError::new(
                "slot renderer already installed (install() is boot-once)",
            ));
        }
        self.state.borrow_mut().renderer = Some(renderer.clone());
        let weak = Rc::downgrade(self);
        Ok(RuntimeDisposer::new(move || {
            if let Some(registry) = weak.upgrade() {
                let mut state = registry.state.borrow_mut();
                if state
                    .renderer
                    .as_ref()
                    .is_some_and(|current| Rc::ptr_eq(current, &renderer))
                {
                    state.renderer = None;
                }
            }
        }))
    }

    /// Installs the locale face once while keeping renderer Host lookup live.
    ///
    /// # Errors
    ///
    /// Rejects a second live locale face.
    pub fn install_locale(
        self: &Rc<Self>,
        locale: Rc<dyn RuntimeLocaleFace>,
    ) -> Result<RuntimeDisposer, ClientSlotError> {
        if self.state.borrow().locale.is_some() {
            return Err(ClientSlotError::new(
                "locale face already installed (installLocale() is boot-once)",
            ));
        }
        self.state.borrow_mut().locale = Some(locale.clone());
        let weak = Rc::downgrade(self);
        Ok(RuntimeDisposer::new(move || {
            if let Some(registry) = weak.upgrade() {
                let mut state = registry.state.borrow_mut();
                if state
                    .locale
                    .as_ref()
                    .is_some_and(|current| Rc::ptr_eq(current, &locale))
                {
                    state.locale = None;
                }
            }
        }))
    }

    /// Installs the Session object-layer face.
    pub fn install_sessions(self: &Rc<Self>, face: Rc<dyn RuntimeStandardFace>) -> RuntimeDisposer {
        self.install_standard(face, true)
    }

    /// Installs the Workspace object-layer face.
    pub fn install_workspaces(
        self: &Rc<Self>,
        face: Rc<dyn RuntimeStandardFace>,
    ) -> RuntimeDisposer {
        self.install_standard(face, false)
    }

    fn install_standard(
        self: &Rc<Self>,
        face: Rc<dyn RuntimeStandardFace>,
        sessions: bool,
    ) -> RuntimeDisposer {
        if sessions {
            self.state.borrow_mut().sessions = Some(face.clone());
        } else {
            self.state.borrow_mut().workspaces = Some(face.clone());
        }
        let weak = Rc::downgrade(self);
        RuntimeDisposer::new(move || {
            if let Some(registry) = weak.upgrade() {
                let mut state = registry.state.borrow_mut();
                let target = if sessions {
                    &mut state.sessions
                } else {
                    &mut state.workspaces
                };
                if target
                    .as_ref()
                    .is_some_and(|current| Rc::ptr_eq(current, &face))
                {
                    *target = None;
                }
            }
        })
    }

    /// Renders through the installed root renderer after every boot-order guard.
    ///
    /// # Errors
    ///
    /// Rejects non-root dispatch, missing renderer, empty root, or absent object layers.
    pub fn render_slot(&self, key: &SlotName, owner: Owner) -> Result<Node, ClientSlotError> {
        if key.as_str() != "root" {
            return Err(ClientSlotError::new(format!(
                "ctx-level renderSlot only renders 'root' (got \"{key}\"); child slots render through the component props face"
            )));
        }
        let state = self.state.borrow();
        let renderer = state.renderer.clone().ok_or_else(|| {
            ClientSlotError::new(
                "slot renderer not installed — boot must call ctx.slots.install(createSlotRenderer()) before rendering 'root'",
            )
        })?;
        if self.core.entries(key).is_empty() {
            return Err(ClientSlotError::new(
                "'root' has no registration — a layout entry must register into 'root' before the shell renders it",
            ));
        }
        if state.sessions.is_none() {
            return Err(ClientSlotError::new(
                "renderSlot('root') before the sessions service mounted — boot order puts runtime apply first",
            ));
        }
        if state.workspaces.is_none() {
            return Err(ClientSlotError::new(
                "renderSlot('root') before the workspaces service mounted — boot order puts runtime apply first",
            ));
        }
        drop(state);
        Ok(renderer.render_root(self, owner))
    }

    /// Live Session face exposed to the renderer Host.
    #[must_use]
    pub fn sessions(&self) -> Option<Rc<dyn RuntimeStandardFace>> {
        self.state.borrow().sessions.clone()
    }

    /// Live Workspace face exposed to the renderer Host.
    #[must_use]
    pub fn workspaces(&self) -> Option<Rc<dyn RuntimeStandardFace>> {
        self.state.borrow().workspaces.clone()
    }

    /// Live locale face exposed through a getter rather than captured at Host creation.
    #[must_use]
    pub fn locale(&self) -> Option<Rc<dyn RuntimeLocaleFace>> {
        self.state.borrow().locale.clone()
    }

    /// Resolves or creates one entry Store instance under its scope key.
    ///
    /// # Errors
    ///
    /// Rejects unloaded handles and missing Session ids for session scopes.
    pub fn store_of(
        &self,
        entry: &RuntimeEntry<C, I>,
        session_id: Option<&str>,
    ) -> Result<Option<Rc<dyn SlotStoreInstance>>, ClientSlotError> {
        let Some(handle) = &entry.payload.store else {
            return Ok(None);
        };
        let mut state = self.state.borrow_mut();
        let axis = state
            .stores
            .iter_mut()
            .find(|axis| Rc::ptr_eq(&axis.handle, handle))
            .ok_or_else(|| {
                ClientSlotError::new(
                    "store handle is not registered (entry unloaded, or the handle never went through register)",
                )
            })?;
        let key = match axis.scope {
            SlotScope::Root => "root",
            SlotScope::Session | SlotScope::SessionMaybe => session_id.ok_or_else(|| {
                ClientSlotError::new(format!(
                    "{} store resolution requires a session id",
                    scope_name(axis.scope)
                ))
            })?,
        };
        if let Some(instance) = axis.instances.get(key) {
            return Ok(Some(instance.clone()));
        }
        let instance = axis
            .handle
            .create((axis.scope != SlotScope::Root).then_some(key));
        axis.instances.insert(key.to_owned(), instance.clone());
        Ok(Some(instance))
    }

    /// Clears and drops persisted state for one dead Session across all session Store axes.
    pub fn prune_store_scope(&self, session_id: &str) {
        for axis in &mut self.state.borrow_mut().stores {
            if axis.scope != SlotScope::Session {
                continue;
            }
            let instance = axis
                .instances
                .remove(session_id)
                .unwrap_or_else(|| axis.handle.create(Some(session_id)));
            instance.clear_persisted();
        }
    }

    fn store_id(&self, handle: &Rc<dyn SlotStoreFactory>) -> StoreHandleId {
        let mut state = self.state.borrow_mut();
        if let Some(axis) = state
            .stores
            .iter()
            .find(|axis| Rc::ptr_eq(&axis.handle, handle))
        {
            return axis.id;
        }
        state.next_store = state.next_store.wrapping_add(1);
        StoreHandleId::new(state.next_store)
    }

    fn acquire_store(&self, handle: Rc<dyn SlotStoreFactory>, id: StoreHandleId, scope: SlotScope) {
        let mut state = self.state.borrow_mut();
        if let Some(axis) = state
            .stores
            .iter_mut()
            .find(|axis| Rc::ptr_eq(&axis.handle, &handle))
        {
            axis.refs += 1;
            return;
        }
        state.stores.push(StoreAxis {
            id,
            handle,
            scope,
            refs: 1,
            instances: HashMap::new(),
        });
    }

    fn release_store(&self, handle: &Rc<dyn SlotStoreFactory>) {
        let mut state = self.state.borrow_mut();
        let Some(index) = state
            .stores
            .iter()
            .position(|axis| Rc::ptr_eq(&axis.handle, handle))
        else {
            return;
        };
        state.stores[index].refs -= 1;
        if state.stores[index].refs == 0 {
            state.stores.remove(index);
        }
    }
}

struct InjectionState<P, I, X> {
    stopped: bool,
    active_epoch: Option<u64>,
    active: Option<SlotEffectBatch>,
    subscription: Option<SlotSubscription<RuntimeSlotPayload<P>, I, X>>,
}

struct InjectionController<C, I, X, Owner, Node> {
    registry: Weak<ClientSlotRegistry<C, I, X, Owner, Node>>,
    key: SlotName,
    setup: InjectionSetup,
    reporter: Rc<dyn SlotInjectionFailureReporter>,
    state: RefCell<InjectionState<C, I, X>>,
}

impl<C, I, X, Owner, Node> InjectionController<C, I, X, Owner, Node>
where
    C: 'static,
    I: Clone + 'static,
    X: 'static,
    Owner: 'static,
    Node: 'static,
{
    fn reconcile(&self) -> Result<(), ClientSlotError> {
        let Some(registry) = self.registry.upgrade() else {
            return Ok(());
        };
        let (spec, epoch, prior) = {
            let mut state = self.state.borrow_mut();
            if state.stopped {
                return Ok(());
            }
            let spec = registry.core.spec(&self.key);
            let epoch = registry.core.declaration_epoch(&self.key);
            if state.active.is_some() && state.active_epoch == Some(epoch) {
                return Ok(());
            }
            let prior = state.active.take();
            state.active_epoch = None;
            (spec, epoch, prior)
        };
        if let Some(prior) = prior {
            prior.dispose();
        }
        if spec.is_none() {
            return Ok(());
        }
        let mut active = SlotEffectBatch::default();
        if let Err(error) = (self.setup)(&mut active) {
            active.dispose();
            return Err(error);
        }
        let mut state = self.state.borrow_mut();
        if state.stopped {
            drop(state);
            active.dispose();
            return Ok(());
        }
        state.active = Some(active);
        state.active_epoch = Some(epoch);
        Ok(())
    }

    fn stop(&self) {
        let (subscription, active) = {
            let mut state = self.state.borrow_mut();
            if state.stopped {
                return;
            }
            state.stopped = true;
            state.active_epoch = None;
            (state.subscription.take(), state.active.take())
        };
        if let Some(subscription) = subscription {
            subscription.dispose();
        }
        if let Some(active) = active {
            active.dispose();
        }
    }
}

/// Controller for one declaration-dependent contribution.
pub struct SlotInjection<C, I, X, Owner, Node> {
    controller: Rc<InjectionController<C, I, X, Owner, Node>>,
}

impl<C, I, X, Owner, Node> SlotInjection<C, I, X, Owner, Node>
where
    C: 'static,
    I: Clone + 'static,
    X: 'static,
    Owner: 'static,
    Node: 'static,
{
    /// Stops waiting and removes the active declaration-lifetime effect.
    pub fn dispose(&self) {
        self.controller.stop();
    }
}

fn scope_name(scope: SlotScope) -> &'static str {
    match scope {
        SlotScope::Root => "root",
        SlotScope::SessionMaybe => "session-maybe",
        SlotScope::Session => "session",
    }
}
