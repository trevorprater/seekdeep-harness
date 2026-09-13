//! Browser Slot test-runtime assembly over the production Rust registries.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_runtime::{
    WasmClientSlotRegistry, WasmConversationEventRegistry, WasmConversationViewRegistry,
};
use seekdeep_client_web_react::{
    configure_client_web_react, create_selector_shim, create_slot_renderer,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{install_test_sessions, install_test_workspaces};

thread_local! {
    static MODULES: RefCell<Option<RuntimeModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct RuntimeModules {
    create_context: Function,
    stabilize: Function,
    act: Function,
    produce: Function,
    react: JsValue,
    render: Function,
    within: Function,
    register_snapshot_serializer: Function,
    clear_storage: Function,
    is_html_element: Function,
    invoke_plugin: Function,
    resolve_inject: Function,
}

struct OwnerPropsState {
    values: RefCell<Vec<(String, JsValue)>>,
    listeners: RefCell<Vec<Function>>,
    version: Cell<u32>,
}

struct FeatureState {
    fiber: JsValue,
    stabilize: Function,
    disposed: Cell<bool>,
}

struct StandaloneRootState {
    slots: JsValue,
    stabilize: Function,
    disposer: RefCell<Option<Function>>,
}

struct RuntimeState {
    modules: RuntimeModules,
    ctx: JsValue,
    slots: JsValue,
    sessions: JsValue,
    workspaces: JsValue,
    registry: WasmClientSlotRegistry,
    conversation_events: WasmConversationEventRegistry,
    conversation_views: WasmConversationViewRegistry,
    root: RefCell<JsValue>,
    root_disposer: RefCell<Option<Function>>,
    renderer_host: RefCell<Option<JsValue>>,
    views: RefCell<Vec<JsValue>>,
    handles: RefCell<Vec<Rc<FeatureState>>>,
    disposed: Cell<bool>,
    owner_props: Rc<OwnerPropsState>,
    auto_declared: RefCell<Vec<String>>,
    auto_root_view: RefCell<Option<JsValue>>,
}

/// Configures framework-only adapters used around the Rust-owned test runtime.
///
/// # Errors
///
/// Returns when any required adapter member is absent or not callable.
#[wasm_bindgen(js_name = configureClientTestRuntime)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_test_runtime(config: JsValue) -> Result<(), JsValue> {
    let react = required(&config, "react", "Client test runtime config")?;
    let selector = create_selector_shim(react.clone())?;
    configure_client_web_react(react.clone(), selector.into())?;
    let modules = RuntimeModules {
        create_context: required_function(&config, "createContext", "Client test runtime config")?,
        stabilize: required_function(&config, "stabilize", "Client test runtime config")?,
        act: required_function(&config, "act", "Client test runtime config")?,
        produce: required_function(&config, "produce", "Client test runtime config")?,
        react,
        render: required_function(&config, "render", "Client test runtime config")?,
        within: required_function(&config, "within", "Client test runtime config")?,
        register_snapshot_serializer: required_function(
            &config,
            "registerSnapshotSerializer",
            "Client test runtime config",
        )?,
        clear_storage: required_function(&config, "clearStorage", "Client test runtime config")?,
        is_html_element: required_function(&config, "isHtmlElement", "Client test runtime config")?,
        invoke_plugin: required_function(&config, "invokePlugin", "Client test runtime config")?,
        resolve_inject: required_function(&config, "resolveInject", "Client test runtime config")?,
    };
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules));
    Ok(())
}

/// Creates the public test-root declaration controller over a Slot service.
///
/// # Errors
///
/// Returns a malformed stabilizer or object-construction failure.
#[wasm_bindgen(js_name = createTestRoot)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_test_root(slots: JsValue, stabilizer: JsValue) -> Result<JsValue, JsValue> {
    let stabilizer = stabilizer
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("TestRoot stabilize must be a function"))?;
    let state = Rc::new(StandaloneRootState {
        slots,
        stabilize: stabilizer,
        disposer: RefCell::new(None),
    });
    let face = Object::new();
    let declaration_state = state.clone();
    let declare = Closure::wrap(
        Box::new(move |children: JsValue, frame: JsValue| -> Promise {
            let state = declaration_state.clone();
            let mutation = Closure::once_into_js(move || -> Result<(), JsValue> {
                let options =
                    object(&[("name", JsValue::from_str("root")), ("children", children)])?;
                let disposer = call_method(&state.slots, "register", &[options.into(), frame])?
                    .dyn_into::<Function>()?;
                *state.disposer.borrow_mut() = Some(disposer);
                Ok(())
            });
            match declaration_state
                .stabilize
                .call1(&JsValue::UNDEFINED, &mutation)
            {
                Ok(result) => Promise::resolve(&result),
                Err(error) => Promise::reject(&error),
            }
        }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
    );
    set(&face, "declare", &declare.into_js_value())?;
    let release_state = state;
    let release = Closure::wrap(Box::new(move || {
        if let Some(disposer) = release_state.disposer.borrow_mut().take() {
            let _ = disposer.call0(&JsValue::UNDEFINED);
        }
    }) as Box<dyn FnMut()>);
    set(&face, "release", &release.into_js_value())?;
    Ok(face.into())
}

/// The assembled browser test runtime.
#[wasm_bindgen(js_name = SlotTestRuntime)]
pub struct WasmSlotTestRuntime {
    state: Rc<RuntimeState>,
}

#[wasm_bindgen(js_class = SlotTestRuntime)]
impl WasmSlotTestRuntime {
    /// Assembles a real Cordis Context, production registries, renderer, and controlled doubles.
    #[wasm_bindgen(js_name = create)]
    pub fn create() -> Promise {
        future_to_promise(async { assemble_runtime().map(JsValue::from) })
    }

    /// Root Cordis Context.
    #[wasm_bindgen(getter)]
    pub fn ctx(&self) -> JsValue {
        self.state.ctx.clone()
    }

    /// Production Slot service face.
    #[wasm_bindgen(getter)]
    pub fn slots(&self) -> JsValue {
        self.state.slots.clone()
    }

    /// Test-owned root declaration controller.
    #[wasm_bindgen(getter)]
    pub fn root(&self) -> JsValue {
        self.state.root.borrow().clone()
    }

    /// Fixture-backed Sessions service.
    #[wasm_bindgen(getter)]
    pub fn sessions(&self) -> JsValue {
        self.state.sessions.clone()
    }

    /// Fixture-backed Workspaces service.
    #[wasm_bindgen(getter)]
    pub fn workspaces(&self) -> JsValue {
        self.state.workspaces.clone()
    }

    /// Publishes an additional feature dependency on the root Context.
    ///
    /// # Errors
    ///
    /// Returns ordinary Cordis publication failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn provide(&self, name: String, value: JsValue) -> Result<(), JsValue> {
        call_method(
            &self.state.ctx,
            "provide",
            &[JsValue::from_str(&name), value],
        )?;
        Ok(())
    }

    /// Mounts one feature Fiber after fail-loud required-service preflight.
    #[allow(clippy::needless_pass_by_value)]
    pub fn mount(&self, plugin: JsValue) -> Promise {
        mount_feature(&self.state, plugin)
    }

    /// Renders the complete root tree through the production renderer.
    ///
    /// # Errors
    ///
    /// Returns boot-order, renderer, React, or test-adapter failures.
    #[wasm_bindgen(js_name = renderRoot)]
    pub fn render_root(&self) -> Result<JsValue, JsValue> {
        render_root(&self.state)
    }

    /// Declares child Slots under the automatic root frame.
    #[allow(clippy::needless_pass_by_value)]
    pub fn declare(&self, children: JsValue) -> Promise {
        declare_auto_root(&self.state, children)
    }

    /// Renders one declared child Slot and returns its local view.
    ///
    /// # Errors
    ///
    /// Returns undeclared-key, boot-order, missing-wrapper, React, or query-adapter failures.
    #[wasm_bindgen(js_name = renderSlot)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn render_slot(&self, key: String, owner: JsValue) -> Result<JsValue, JsValue> {
        render_local_slot(&self.state, &key, owner)
    }

    /// Resolves the renderer-owned Store instance for a Slot and optional Session scope.
    ///
    /// # Errors
    ///
    /// Returns before-render, missing-registration, storeless-entry, or Host failures.
    #[wasm_bindgen(js_name = storeOf)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn store_of(&self, key: String, scope_key: Option<String>) -> Result<JsValue, JsValue> {
        store_of(&self.state, &key, scope_key.as_deref())
    }

    /// Flushes pending registry and renderer notifications through the configured stabilizer.
    pub fn flush(&self) -> Promise {
        let callback = Closure::once_into_js(|| {});
        stabilize(&self.state, &callback)
    }

    /// Idempotently tears down views, features, root declarations, scopes, and persisted test data.
    pub fn dispose(&self) -> Promise {
        dispose_runtime(&self.state)
    }
}

fn assemble_runtime() -> Result<WasmSlotTestRuntime, JsValue> {
    let modules = configured_modules()?;
    modules
        .register_snapshot_serializer
        .call0(&JsValue::UNDEFINED)?;
    let ctx = modules.create_context.call0(&JsValue::UNDEFINED)?;
    if !ctx.is_object() || ctx.is_null() {
        return Err(js_sys::TypeError::new(
            "Client test runtime createContext must return a Context object",
        )
        .into());
    }

    let changed_ctx = ctx.clone();
    let on_changed = Closure::wrap(Box::new(move |key: String| -> Result<(), JsValue> {
        call_method(
            &changed_ctx,
            "emit",
            &[JsValue::from_str("slots/changed"), JsValue::from_str(&key)],
        )?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let registry = WasmClientSlotRegistry::new(Some(
        on_changed.into_js_value().unchecked_into::<Function>(),
    ));
    let slots = registry.face_for(ctx.clone())?;
    provide_context(&ctx, "slots", &slots)?;

    let conversation_events = WasmConversationEventRegistry::new();
    let conversation_views = WasmConversationViewRegistry::new();
    let event_face = conversation_events.face_for(ctx.clone())?;
    let view_face = conversation_views.face_for(ctx.clone())?;
    provide_context(&ctx, "conversationEvents", &event_face)?;
    provide_context(&ctx, "conversationViews", &view_face)?;

    let sessions = install_test_sessions(
        ctx.clone(),
        modules.stabilize.clone().into(),
        modules.produce.clone().into(),
    )?;
    let workspaces = install_test_workspaces(
        ctx.clone(),
        modules.stabilize.clone().into(),
        modules.produce.clone().into(),
    )?;
    registry.install_sessions(
        object(&[
            ("list", required(&sessions, "list", "TestSessions")?),
            (
                "provideInfo",
                required(&sessions, "currentProvideInfo", "TestSessions")?,
            ),
        ])?
        .into(),
    );
    registry.install_workspaces(
        object(&[("list", required(&workspaces, "list", "TestWorkspaces")?)])?.into(),
    );

    let state = Rc::new(RuntimeState {
        modules,
        ctx,
        slots,
        sessions,
        workspaces,
        registry,
        conversation_events,
        conversation_views,
        root: RefCell::new(JsValue::UNDEFINED),
        root_disposer: RefCell::new(None),
        renderer_host: RefCell::new(None),
        views: RefCell::new(Vec::new()),
        handles: RefCell::new(Vec::new()),
        disposed: Cell::new(false),
        owner_props: Rc::new(OwnerPropsState {
            values: RefCell::new(Vec::new()),
            listeners: RefCell::new(Vec::new()),
            version: Cell::new(0),
        }),
        auto_declared: RefCell::new(Vec::new()),
        auto_root_view: RefCell::new(None),
    });
    *state.root.borrow_mut() = root_face(&state)?.into();
    install_renderer(&state)?;
    Ok(WasmSlotTestRuntime { state })
}

fn install_renderer(state: &Rc<RuntimeState>) -> Result<(), JsValue> {
    let renderer = create_slot_renderer()?;
    let render = required_function(&renderer, "renderRoot", "Slot renderer")?;
    let installed = Object::new();
    let render_state = Rc::downgrade(state);
    let render_this = renderer;
    let render_root = Closure::wrap(Box::new(
        move |host: JsValue, owner: JsValue| -> Result<JsValue, JsValue> {
            let state = render_state
                .upgrade()
                .ok_or_else(|| js_sys::Error::new("SlotTestRuntime was dropped"))?;
            *state.renderer_host.borrow_mut() = Some(host.clone());
            render.call2(&render_this, &host, &owner)
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    set(&installed, "renderRoot", &render_root.into_js_value())?;
    call_method(&state.slots, "install", &[installed.into()])?;
    Ok(())
}

fn root_face(state: &Rc<RuntimeState>) -> Result<Object, JsValue> {
    let face = Object::new();
    let declare_state = Rc::downgrade(state);
    let declare = Closure::wrap(
        Box::new(move |children: JsValue, frame: JsValue| -> Promise {
            declare_state.upgrade().map_or_else(
                || Promise::reject(&js_sys::Error::new("SlotTestRuntime was dropped")),
                |state| declare_root(&state, children, frame),
            )
        }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>,
    );
    set(&face, "declare", &declare.into_js_value())?;
    let release_state = Rc::downgrade(state);
    let release = Closure::wrap(Box::new(move || {
        if let Some(state) = release_state.upgrade() {
            let _ = release_root(&state);
        }
    }) as Box<dyn FnMut()>);
    set(&face, "release", &release.into_js_value())?;
    Ok(face)
}

fn declare_root(state: &Rc<RuntimeState>, children: JsValue, frame: JsValue) -> Promise {
    let declaration_state = state.clone();
    let mutation = Closure::once_into_js(move || -> Result<(), JsValue> {
        let options = object(&[("name", JsValue::from_str("root")), ("children", children)])?;
        let disposer = call_method(
            &declaration_state.slots,
            "register",
            &[options.into(), frame],
        )?
        .dyn_into::<Function>()?;
        *declaration_state.root_disposer.borrow_mut() = Some(disposer);
        Ok(())
    });
    stabilize(state, &mutation)
}

fn release_root(state: &RuntimeState) -> Result<Promise, JsValue> {
    let Some(disposer) = state.root_disposer.borrow_mut().take() else {
        return Ok(Promise::resolve(&JsValue::UNDEFINED));
    };
    let result = disposer.call0(&JsValue::UNDEFINED)?;
    Ok(Promise::resolve(&result))
}

fn declare_auto_root(state: &Rc<RuntimeState>, children: JsValue) -> Promise {
    if !children.is_object() || children.is_null() {
        return Promise::reject(&js_sys::TypeError::new(
            "SlotTestRuntime.declare children must be an object",
        ));
    }
    {
        let mut declared = state.auto_declared.borrow_mut();
        for key in Object::keys(&Object::from(children.clone()))
            .iter()
            .filter_map(|key| key.as_string())
        {
            if !declared.contains(&key) {
                declared.push(key);
            }
        }
    }
    declare_root(state, children, auto_frame(state))
}

fn auto_frame(state: &Rc<RuntimeState>) -> JsValue {
    let owner_state = state.owner_props.clone();
    let subscribe_state = owner_state.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let mut listeners = subscribe_state.listeners.borrow_mut();
        if !listeners
            .iter()
            .any(|registered| Object::is(registered, &listener))
        {
            listeners.push(listener.clone());
        }
        drop(listeners);
        let cleanup_state = subscribe_state.clone();
        Closure::wrap(Box::new(move || {
            cleanup_state
                .listeners
                .borrow_mut()
                .retain(|registered| !Object::is(registered, &listener));
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    let subscribe: Function = subscribe.into_js_value().unchecked_into();
    let version_state = owner_state.clone();
    let version =
        Closure::wrap(Box::new(move || version_state.version.get()) as Box<dyn FnMut() -> u32>);
    let version: Function = version.into_js_value().unchecked_into();
    let react = state.modules.react.clone();
    let frame = Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        required_function(&react, "useSyncExternalStore", "React")?
            .call2(&react, &subscribe, &version)?;
        let render_slot = required_function(&props, "renderSlot", "Automatic root frame props")?;
        let mut rendered = Vec::with_capacity(owner_state.values.borrow().len());
        for (key, owner) in owner_state.values.borrow().iter() {
            let child = render_slot.call2(&JsValue::UNDEFINED, &JsValue::from_str(key), owner)?;
            rendered.push(react_element(
                &react,
                &required(&react, "Fragment", "React")?,
                Some(&object(&[("key", JsValue::from_str(key))])?.into()),
                &[child],
            )?);
        }
        react_element(
            &react,
            &required(&react, "Fragment", "React")?,
            None,
            &rendered,
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    frame.into_js_value()
}

fn render_root(state: &Rc<RuntimeState>) -> Result<JsValue, JsValue> {
    let root = call_method(
        &state.slots,
        "renderSlot",
        &[JsValue::from_str("root"), Object::new().into()],
    )?;
    let node = react_element(
        &state.modules.react,
        &required(&state.modules.react, "Fragment", "React")?,
        None,
        &[root],
    )?;
    let view = state.modules.render.call1(&JsValue::UNDEFINED, &node)?;
    state.views.borrow_mut().push(view.clone());
    Ok(view)
}

fn render_local_slot(
    state: &Rc<RuntimeState>,
    key: &str,
    owner: JsValue,
) -> Result<JsValue, JsValue> {
    if !state.auto_declared.borrow().iter().any(|item| item == key) {
        return Err(js_sys::Error::new(&format!(
            "renderSlot('{key}') without declare() — declare the key first (or use root.declare for a custom frame)"
        ))
        .into());
    }
    install_owner(state, key, owner)?;
    let view = if let Some(view) = state.auto_root_view.borrow().clone() {
        view
    } else {
        let view = render_root(state)?;
        *state.auto_root_view.borrow_mut() = Some(view.clone());
        view
    };
    let root = required(&view, "container", "Testing Library RenderResult")?;
    let selector = format!("[data-slot=\"{key}\"]");
    let container = call_method(&root, "querySelector", &[JsValue::from_str(&selector)])?;
    let is_html = state
        .modules
        .is_html_element
        .call1(&JsValue::UNDEFINED, &container)?
        .as_bool()
        .unwrap_or(false);
    if !is_html {
        return Err(js_sys::Error::new(&format!(
            "renderSlot('{key}'): the auto frame rendered no wrapper — was the runtime already disposed?"
        ))
        .into());
    }
    let view_queries = state
        .modules
        .within
        .call1(&JsValue::UNDEFINED, &container)?;
    let update_state = state.clone();
    let update_key = key.to_owned();
    let update = Closure::wrap(Box::new(move |next: JsValue| -> Result<(), JsValue> {
        install_owner(&update_state, &update_key, next)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    object(&[
        ("container", container),
        ("view", view_queries),
        ("update", update.into_js_value()),
    ])
    .map(Into::into)
}

fn install_owner(state: &Rc<RuntimeState>, key: &str, owner: JsValue) -> Result<(), JsValue> {
    let owner_state = state.owner_props.clone();
    let key = key.to_owned();
    let mutation = Closure::once_into_js(move || -> Result<(), JsValue> {
        let mut values = owner_state.values.borrow_mut();
        if let Some((_, current)) = values.iter_mut().find(|(candidate, _)| candidate == &key) {
            *current = owner;
        } else {
            values.push((key, owner));
        }
        owner_state
            .version
            .set(owner_state.version.get().wrapping_add(1));
        drop(values);
        let listeners = owner_state.listeners.borrow().clone();
        for listener in listeners {
            listener.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    });
    state.modules.act.call1(&JsValue::UNDEFINED, &mutation)?;
    Ok(())
}

fn store_of(state: &RuntimeState, key: &str, scope_key: Option<&str>) -> Result<JsValue, JsValue> {
    let Some(host) = state.renderer_host.borrow().clone() else {
        return Err(js_sys::Error::new(
            "storeOf before renderRoot() — the host face exists only inside the installed renderer",
        )
        .into());
    };
    let entries = Array::from(&call_method(&host, "entriesOf", &[JsValue::from_str(key)])?);
    let entry = entries.get(0);
    if entry.is_undefined() {
        return Err(js_sys::Error::new(&format!(
            "storeOf('{key}'): no registration on the ledger"
        ))
        .into());
    }
    let scope = scope_key.map_or(JsValue::UNDEFINED, JsValue::from_str);
    let instance = call_method(&host, "storeOf", &[entry, scope])?;
    if instance.is_undefined() {
        return Err(
            js_sys::Error::new(&format!("storeOf('{key}'): the entry declares no store")).into(),
        );
    }
    Ok(instance)
}

fn mount_feature(state: &Rc<RuntimeState>, plugin: JsValue) -> Promise {
    let operation = (|| -> Result<(JsValue, Promise), JsValue> {
        let inject = state.modules.resolve_inject.call1(
            &JsValue::UNDEFINED,
            &Reflect::get(&plugin, &JsValue::from_str("inject"))?,
        )?;
        let missing = inject_names(&inject)?
            .into_iter()
            .filter_map(|name| {
                call_method(&state.ctx, "get", &[JsValue::from_str(&name)])
                    .map(|value| value.is_undefined().then_some(name))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !missing.is_empty() {
            return Err(js_sys::Error::new(&format!(
                "mount would suspend: missing service(s) {} — provide() them first",
                missing.join(", ")
            ))
            .into());
        }
        let descriptor = bound_plugin_descriptor(state, plugin, &inject)?;
        let fiber = call_method(&state.ctx, "plugin", &[descriptor])?;
        let wait_fiber = fiber.clone();
        let wait = Closure::once_into_js(move || -> Result<JsValue, JsValue> {
            call_method(&wait_fiber, "await", &[])
        });
        Ok((fiber, stabilize(state, &wait)))
    })();
    let (fiber, settled) = match operation {
        Ok(operation) => operation,
        Err(error) => return Promise::reject(&error),
    };
    let runtime = state.clone();
    future_to_promise(async move {
        JsFuture::from(settled).await?;
        let handle = Rc::new(FeatureState {
            fiber,
            stabilize: runtime.modules.stabilize.clone(),
            disposed: Cell::new(false),
        });
        let face = feature_face(&handle)?;
        runtime.handles.borrow_mut().push(handle);
        Ok(face.into())
    })
}

fn bound_plugin_descriptor(
    state: &Rc<RuntimeState>,
    plugin: JsValue,
    inject: &JsValue,
) -> Result<JsValue, JsValue> {
    let descriptor = Object::new();
    let name = Reflect::get(&plugin, &JsValue::from_str("name"))?;
    if !name.is_undefined() {
        set(&descriptor, "name", &name)?;
    }
    set(&descriptor, "inject", inject)?;
    let apply_state = state.clone();
    let original = plugin;
    let apply = Closure::wrap(Box::new(
        move |context: JsValue, config: JsValue| -> Result<JsValue, JsValue> {
            let context = bind_feature_context(&apply_state, context)?;
            apply_state.modules.invoke_plugin.call3(
                &JsValue::UNDEFINED,
                &original,
                &context,
                &config,
            )
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    set(&descriptor, "apply", &apply.into_js_value())?;
    Ok(descriptor.into())
}

fn bind_feature_context(state: &RuntimeState, context: JsValue) -> Result<JsValue, JsValue> {
    let facade = Object::create(&Object::from(context.clone()));
    let slots = state.registry.face_for(context.clone())?;
    let events = state.conversation_events.face_for(context.clone())?;
    let views = state.conversation_views.face_for(context.clone())?;
    define_own(&facade, "slots", &slots)?;
    define_own(&facade, "conversationEvents", &events)?;
    define_own(&facade, "conversationViews", &views)?;
    let get_context = context;
    let get_slots = slots;
    let get_events = events;
    let get_views = views;
    let get = Closure::wrap(Box::new(move |name: String| -> Result<JsValue, JsValue> {
        match name.as_str() {
            "slots" => Ok(get_slots.clone()),
            "conversationEvents" => Ok(get_events.clone()),
            "conversationViews" => Ok(get_views.clone()),
            _ => call_method(&get_context, "get", &[JsValue::from_str(&name)]),
        }
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    define_own(&facade, "get", &get.into_js_value())?;
    Ok(facade.into())
}

fn feature_face(state: &Rc<FeatureState>) -> Result<Object, JsValue> {
    let face = Object::new();
    set(&face, "fiber", &state.fiber)?;
    let dispose_state = state.clone();
    let dispose = Closure::wrap(
        Box::new(move || dispose_feature(&dispose_state)) as Box<dyn FnMut() -> Promise>
    );
    set(&face, "dispose", &dispose.into_js_value())?;
    Ok(face)
}

fn dispose_feature(state: &Rc<FeatureState>) -> Promise {
    if state.disposed.replace(true) {
        return Promise::resolve(&JsValue::UNDEFINED);
    }
    let fiber = state.fiber.clone();
    let mutation = Closure::once_into_js(move || -> Result<JsValue, JsValue> {
        call_method(&fiber, "dispose", &[])
    });
    match state.stabilize.call1(&JsValue::UNDEFINED, &mutation) {
        Ok(result) => Promise::resolve(&result),
        Err(error) => Promise::reject(&error),
    }
}

fn dispose_runtime(state: &Rc<RuntimeState>) -> Promise {
    if state.disposed.replace(true) {
        return Promise::resolve(&JsValue::UNDEFINED);
    }
    *state.auto_root_view.borrow_mut() = None;
    let views = std::mem::take(&mut *state.views.borrow_mut());
    for view in views {
        if let Err(error) = call_method(&view, "unmount", &[]) {
            return Promise::reject(&error);
        }
    }
    let handles = std::mem::take(&mut *state.handles.borrow_mut());
    let runtime = state.clone();
    future_to_promise(async move {
        for handle in handles {
            JsFuture::from(dispose_feature(&handle)).await?;
        }
        JsFuture::from(release_root(&runtime)?).await?;
        let scope_disposal = call_method(&runtime.sessions, "disposeScopes", &[])?;
        JsFuture::from(Promise::resolve(&scope_disposal)).await?;
        runtime.modules.clear_storage.call0(&JsValue::UNDEFINED)?;
        Ok(JsValue::UNDEFINED)
    })
}

fn stabilize(state: &RuntimeState, callback: &JsValue) -> Promise {
    match state.modules.stabilize.call1(&JsValue::UNDEFINED, callback) {
        Ok(result) => Promise::resolve(&result),
        Err(error) => Promise::reject(&error),
    }
}

fn configured_modules() -> Result<RuntimeModules, JsValue> {
    MODULES.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new(
                "client-test-runtime is not configured — load its package entry before creating a runtime",
            )
            .into()
        })
    })
}

fn inject_names(value: &JsValue) -> Result<Vec<String>, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(Vec::new());
    }
    if Array::is_array(value) {
        return Array::from(value)
            .iter()
            .map(|value| {
                value.as_string().ok_or_else(|| {
                    js_sys::TypeError::new("feature inject entries must be strings").into()
                })
            })
            .collect();
    }
    if value.is_object() {
        return Ok(Object::keys(&Object::from(value.clone()))
            .iter()
            .filter_map(|value| value.as_string())
            .collect());
    }
    Err(js_sys::TypeError::new("feature inject must be an array or object").into())
}

fn react_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&JsValue>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(kind);
    arguments.push(props.unwrap_or(&JsValue::NULL));
    for child in children {
        arguments.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}

fn provide_context(context: &JsValue, name: &str, service: &JsValue) -> Result<(), JsValue> {
    call_method(
        context,
        "provide",
        &[JsValue::from_str(name), service.clone()],
    )?;
    Ok(())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required_function(value, name, "Client test runtime face")?;
    let arguments: Array = arguments.iter().cloned().collect();
    function.apply(value, &arguments)
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    if member.is_undefined() || member.is_null() {
        Err(js_sys::TypeError::new(&format!("{owner} requires {key:?}")).into())
    } else {
        Ok(member)
    }
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{owner} requires function {key:?}")).into())
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

fn set(target: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(target, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set Client test runtime member {key:?}")).into())
    }
}

fn define_own(target: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    let descriptor = Object::new();
    set(&descriptor, "value", value)?;
    set(&descriptor, "writable", &JsValue::TRUE)?;
    set(&descriptor, "enumerable", &JsValue::TRUE)?;
    set(&descriptor, "configurable", &JsValue::TRUE)?;
    Object::define_property(target, &JsValue::from_str(key), &descriptor);
    Ok(())
}
