//! Browser WASM shell facade, components, assembly plugin, and boot transaction.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{APP_ROOT_STYLES, APP_SHELL_ID, MODULES_ID, PLATFORM_MODULES};

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    react_dom_client: JsValue,
    cordis: JsValue,
    loader: JsValue,
    client_modules: JsValue,
    modules_client: JsValue,
    web_react: JsValue,
    static_modules: JsValue,
    app_root: JsValue,
    document_title: JsValue,
}

struct BrowserSignal {
    value: RefCell<JsValue>,
    listeners: RefCell<Vec<Function>>,
}

impl BrowserSignal {
    fn new(value: JsValue) -> Rc<Self> {
        Rc::new(Self {
            value: RefCell::new(value),
            listeners: RefCell::new(Vec::new()),
        })
    }

    fn set(&self, value: JsValue) {
        *self.value.borrow_mut() = value;
        let listeners = self.listeners.borrow().clone();
        for listener in listeners {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }
    }
}

fn signal_face(signal: Rc<BrowserSignal>) -> Result<JsValue, JsValue> {
    let getter = signal.clone();
    let get_snapshot = Closure::wrap(
        Box::new(move || getter.value.borrow().clone()) as Box<dyn FnMut() -> JsValue>
    );
    let subscriber = signal.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        subscriber.listeners.borrow_mut().push(listener.clone());
        let cleanup = subscriber.clone();
        Closure::wrap(Box::new(move || {
            cleanup
                .listeners
                .borrow_mut()
                .retain(|candidate| !Object::is(candidate, &listener));
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    let setter = signal;
    let set =
        Closure::wrap(Box::new(move |value: JsValue| setter.set(value)) as Box<dyn FnMut(JsValue)>);
    object(&[
        ("getSnapshot", get_snapshot.into_js_value()),
        ("subscribe", subscribe.into_js_value()),
        ("set", set.into_js_value()),
    ])
    .map(Into::into)
}

/// Configures shell-owned JavaScript modules and injects compiled shell styles.
///
/// # Errors
///
/// Returns malformed module or DOM failures.
#[wasm_bindgen(js_name = configureClientWeb)]
#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::too_many_arguments)]
pub fn configure_client_web(
    react: JsValue,
    react_dom_client: JsValue,
    cordis: JsValue,
    loader: JsValue,
    client_modules: JsValue,
    modules_client: JsValue,
    web_react: JsValue,
    static_modules: JsValue,
) -> Result<(), JsValue> {
    let app_root = component(render_app_root);
    let document_title = component(render_document_title);
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            react_dom_client,
            cordis,
            loader,
            client_modules,
            modules_client,
            web_react,
            static_modules,
            app_root,
            document_title,
        });
    });
    inject_styles()
}

/// Frozen platform module words in shell seed order.
#[wasm_bindgen(js_name = platformModules)]
pub fn platform_modules() -> Array {
    PLATFORM_MODULES
        .iter()
        .map(|name| JsValue::from_str(name))
        .collect()
}

/// Returns the static singleton table configured by the shell bundle.
///
/// # Errors
///
/// Returns a configure-before-use failure.
#[wasm_bindgen(js_name = getStaticModules)]
pub fn get_static_modules() -> Result<JsValue, JsValue> {
    Ok(configured_modules()?.static_modules)
}

/// Creates a writable kernel signal JS face.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
#[wasm_bindgen(js_name = createSignal)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_signal(initial: JsValue) -> Result<JsValue, JsValue> {
    signal_face(BrowserSignal::new(initial))
}

/// Creates the copy-on-write boot status Store.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
#[wasm_bindgen(js_name = createLoaderStatusStore)]
pub fn create_loader_status_store() -> Result<JsValue, JsValue> {
    let signal = BrowserSignal::new(Object::new().into());
    let face = Object::from(signal_face(signal.clone())?);
    let setter = signal;
    let set = Closure::wrap(
        Box::new(move |id: String, state: String| -> Result<(), JsValue> {
            let next = Object::assign(&Object::new(), &Object::from(setter.value.borrow().clone()));
            Reflect::set(&next, &JsValue::from_str(&id), &JsValue::from_str(&state))?;
            setter.set(next.into());
            Ok(())
        }) as Box<dyn FnMut(String, String) -> Result<(), JsValue>>,
    );
    set_property(&face, "set", &set.into_js_value())?;
    Ok(face.into())
}

/// Stable shell `AppRoot` component.
///
/// # Errors
///
/// Returns a configure-before-use failure.
#[wasm_bindgen(js_name = appRootComponent)]
pub fn app_root_component() -> Result<JsValue, JsValue> {
    Ok(configured_modules()?.app_root)
}

/// Stable shell `DocumentTitle` component.
///
/// # Errors
///
/// Returns a configure-before-use failure.
#[wasm_bindgen(js_name = documentTitleComponent)]
pub fn document_title_component() -> Result<JsValue, JsValue> {
    Ok(configured_modules()?.document_title)
}

#[allow(clippy::needless_pass_by_value)]
fn render_app_root(props: JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let settled_face = required_property(&props, "settled", "AppRoot props")?;
    let status_face = required_property(&props, "status", "AppRoot props")?;
    let error_face = required_property(&props, "error", "AppRoot props")?;
    let settled = use_signal(&modules.react, &settled_face)?
        .as_bool()
        .unwrap_or(false);
    let status = use_signal(&modules.react, &status_face)?;
    let error = use_signal(&modules.react, &error_face)?;
    if settled {
        let real = function(&props, "renderApp")?.call0(&JsValue::UNDEFINED)?;
        return fragment(&modules.react, &[real]);
    }
    let failed = Object::entries(&Object::from(status))
        .iter()
        .filter(|entry| Array::from(entry).get(1).as_string().as_deref() == Some("failed"))
        .collect::<Vec<_>>();
    let loud = !error.is_undefined() || !failed.is_empty();
    let wordmark = tag(
        &modules.react,
        "div",
        "seekdeep-web-wordmark",
        &[JsValue::from_str("HARNESS")],
    )?;
    let body = if loud {
        let mut children = vec![tag(
            &modules.react,
            "div",
            "seekdeep-web-failed-title",
            &[JsValue::from_str("Failed to load plugins")],
        )?];
        for entry in failed {
            children.push(tag(
                &modules.react,
                "div",
                "seekdeep-web-failed-item",
                &[Array::from(&entry).get(0)],
            )?);
        }
        if !error.is_undefined() {
            children.push(tag(
                &modules.react,
                "div",
                "seekdeep-web-failed-item",
                &[error],
            )?);
        }
        tag(&modules.react, "div", "seekdeep-web-failed", &children)?
    } else {
        fragment(
            &modules.react,
            &[
                tag(&modules.react, "div", "seekdeep-web-spinner", &[])?,
                tag(
                    &modules.react,
                    "div",
                    "seekdeep-web-hint",
                    &[JsValue::from_str("Loading plugins…")],
                )?,
            ],
        )?
    };
    let card = tag(
        &modules.react,
        "div",
        "seekdeep-web-card",
        &[wordmark, body],
    )?;
    tag(&modules.react, "div", "seekdeep-web-boot", &[card])
}

#[allow(clippy::needless_pass_by_value)]
fn render_document_title(props: JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let document = required_property(&js_sys::global(), "document", "global")?;
    let current = required_property(&document, "title", "document")?;
    let original = function(&modules.react, "useRef")?.call1(&modules.react, &current)?;
    let title = Reflect::get(&props, &JsValue::from_str("title"))?;
    let effect_title = title.clone();
    let effect_document = document.clone();
    let effect_original = original;
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let original = Reflect::get(&effect_original, &JsValue::from_str("current"))?;
        let next = effect_title.as_string().map_or_else(
            || original.clone(),
            |title| {
                JsValue::from_str(&format!(
                    "{title} — {}",
                    original.as_string().unwrap_or_default()
                ))
            },
        );
        Reflect::set(&effect_document, &JsValue::from_str("title"), &next)?;
        let cleanup_document = effect_document.clone();
        let cleanup_original = original;
        let cleanup = Closure::wrap(Box::new(move || {
            let _ = Reflect::set(
                &cleanup_document,
                &JsValue::from_str("title"),
                &cleanup_original,
            );
        }) as Box<dyn FnMut()>);
        Ok(cleanup.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dependencies = Array::of1(&title);
    function(&modules.react, "useEffect")?.call2(
        &modules.react,
        &effect.into_js_value(),
        &dependencies,
    )?;
    Ok(JsValue::NULL)
}

/// Builds the settled real-UI renderer for one active app-shell context.
///
/// # Errors
///
/// Fails loud when Sessions or Slots are unavailable.
#[wasm_bindgen(js_name = buildRenderApp)]
#[allow(clippy::needless_pass_by_value)]
pub fn build_render_app(ctx: JsValue) -> Result<Function, JsValue> {
    let modules = configured_modules()?;
    let sessions = call_method(&ctx, "get", &[JsValue::from_str("sessions")])?;
    if sessions.is_undefined() {
        return Err(js_sys::Error::new("shell assembly: sessions service unavailable").into());
    }
    let use_sessions = call_method(
        &modules.web_react,
        "bindSnapshotSelector",
        &[required_property(&sessions, "list", "Sessions service")?],
    )?
    .dyn_into::<Function>()?;
    let title_component = modules.document_title;
    let react = modules.react.clone();
    let session_title = Closure::wrap(
        Box::new(move |_props: JsValue| -> Result<JsValue, JsValue> {
            let select = Function::new_with_args(
                "state",
                "const id = state.current; return id === undefined ? undefined : state.byId[id]?.title",
            );
            let title = use_sessions.call2(&JsValue::UNDEFINED, &select, &JsValue::UNDEFINED)?;
            let props = Object::new();
            if !title.is_undefined() {
                set_property(&props, "title", &title)?;
            }
            create_element(&react, &title_component, Some(&props), &[])
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    let session_title = session_title.into_js_value();
    let render_ctx = ctx;
    Ok(Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let modules = configured_modules()?;
        let title = create_element(&modules.react, &session_title, None, &[])?;
        let slots = call_method(&render_ctx, "get", &[JsValue::from_str("slots")])?;
        if slots.is_undefined() {
            return Err(js_sys::Error::new("shell assembly: slots service unavailable").into());
        }
        let root = call_method(
            &slots,
            "renderSlot",
            &[JsValue::from_str("root"), Object::new().into()],
        )?;
        fragment(&modules.react, &[title, root])
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
    .into_js_value()
    .unchecked_into())
}

/// App-shell Cordis plugin body.
///
/// # Errors
///
/// Returns missing-service, renderer-install, or service-provision failures.
#[wasm_bindgen(js_name = applyAppShell)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_app_shell(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required_service(&ctx, "slots")?;
    required_service(&ctx, "sessions")?;
    required_service(&ctx, "layout")?;
    let renderer = call_method(&modules.web_react, "createSlotRenderer", &[])?;
    call_method(&slots, "install", &[renderer])?;
    let built = Rc::new(RefCell::new(None::<Function>));
    let render_ctx = ctx.clone();
    let render_built = built;
    let render = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if render_built.borrow().is_none() {
            *render_built.borrow_mut() = Some(build_render_app(render_ctx.clone())?);
        }
        let render = render_built
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("app-shell render factory was not initialized"))?;
        render.call0(&JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let service = object(&[("renderApp", render.into_js_value())])?;
    let reflect = required_property(&ctx, "reflect", "Client Context")?;
    call_method(
        &reflect,
        "provide",
        &[JsValue::from_str("appShell"), service.into()],
    )?;
    Ok(())
}

/// App-shell exact inject list.
#[wasm_bindgen(js_name = appShellInject)]
pub fn app_shell_inject() -> Array {
    ["slots", "sessions", "layout"]
        .into_iter()
        .map(JsValue::from_str)
        .collect()
}

/// Shell pseudo entry identity.
#[wasm_bindgen(js_name = appShellId)]
pub fn app_shell_id() -> String {
    APP_SHELL_ID.to_owned()
}

/// App-shell Cordis plugin name.
#[wasm_bindgen(js_name = appShellName)]
pub fn app_shell_name() -> String {
    "app-shell".to_owned()
}

#[wasm_bindgen(js_name = AppWebEntry)]
/// JavaScript-facing owner of one mounted browser boot transaction.
pub struct WasmAppWebEntry {
    element: JsValue,
    seams: JsValue,
    status: JsValue,
    settled: JsValue,
    error: JsValue,
    root: Rc<RefCell<Option<JsValue>>>,
    context: Rc<RefCell<Option<JsValue>>>,
}

#[wasm_bindgen(js_class = AppWebEntry)]
impl WasmAppWebEntry {
    /// Holds the shell mount and optional module transport seams.
    ///
    /// # Errors
    ///
    /// Returns signal-construction failures.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(element: JsValue, seams: JsValue) -> Result<Self, JsValue> {
        Ok(Self {
            element,
            seams,
            status: create_loader_status_store()?,
            settled: create_signal(JsValue::FALSE)?,
            error: create_signal(JsValue::UNDEFINED)?,
            root: Rc::new(RefCell::new(None)),
            context: Rc::new(RefCell::new(None)),
        })
    }

    /// Runs the two-stage module/plugin boot. Manifest errors reject; boot failures stay rendered.
    pub fn run(&self) -> Promise {
        let element = self.element.clone();
        let seams = self.seams.clone();
        let status = self.status.clone();
        let settled = self.settled.clone();
        let error = self.error.clone();
        let root_slot = self.root.clone();
        let context_slot = self.context.clone();
        future_to_promise(async move {
            let modules = configured_modules()?;
            let raw = Reflect::get(&js_sys::global(), &JsValue::from_str("__SEEKDEEP_BOOT__"))?;
            let manifest = call_method(&modules.client_modules, "parseBootManifest", &[raw])?;
            let system = construct_module_system(&modules, &manifest, &seams)?;
            let app_shell = app_shell_module()?;
            call_method(
                &system,
                "registerStatic",
                &[JsValue::from_str(APP_SHELL_ID), app_shell],
            )?;
            call_method(
                &system,
                "registerStatic",
                &[
                    JsValue::from_str(MODULES_ID),
                    modules.modules_client.clone(),
                ],
            )?;
            Reflect::set(
                &js_sys::global(),
                &JsValue::from_str("__SEEKDEEP_MODULES__"),
                &system,
            )?;
            let root = call_method(&modules.react_dom_client, "createRoot", &[element])?;
            *root_slot.borrow_mut() = Some(root.clone());
            let render_shell =
                render_shell_gate(&modules, &status, &settled, &error, context_slot.clone())?;
            call_method(&root, "render", &[render_shell])?;

            let prefetch = prefetch_immediate(&system, &manifest)?;
            let ctx = construct(
                &required_property(&modules.cordis, "Context", "Cordis")?,
                &[],
            )?;
            *context_slot.borrow_mut() = Some(ctx.clone());
            let boot = run_plugin_boot(&modules, &ctx, &system, &manifest, &status, prefetch).await;
            match boot {
                Ok(()) => {
                    call_method(&settled, "set", &[JsValue::TRUE])?;
                }
                Err(reason) => {
                    console_error(&reason);
                    let message = error_message(&reason);
                    call_method(&error, "set", &[JsValue::from_str(&message)])?;
                }
            }
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Unmounts loading or settled UI.
    pub fn dispose(&self) {
        if let Some(root) = self.root.borrow().as_ref() {
            let _ = call_method(root, "unmount", &[]);
        }
    }
}

fn construct_module_system(
    modules: &BrowserModules,
    manifest: &JsValue,
    seams: &JsValue,
) -> Result<JsValue, JsValue> {
    let options = Object::new();
    set_property(
        &options,
        "modules",
        &required_property(manifest, "modules", "Boot manifest")?,
    )?;
    set_property(&options, "staticModules", &modules.static_modules)?;
    if seams.is_object() && !seams.is_null() {
        Object::assign(&options, &Object::from(seams.clone()));
    }
    let constructor = required_property(
        &modules.client_modules,
        "ClientModuleSystem",
        "Client Modules",
    )?;
    construct(&constructor, &[options.into()])
}

fn app_shell_module() -> Result<JsValue, JsValue> {
    let apply = Closure::wrap(Box::new(move |ctx: JsValue| apply_app_shell(ctx))
        as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    object(&[
        ("name", JsValue::from_str("app-shell")),
        ("inject", app_shell_inject().into()),
        ("apply", apply.into_js_value()),
    ])
    .map(Into::into)
}

fn render_shell_gate(
    modules: &BrowserModules,
    status: &JsValue,
    settled: &JsValue,
    error: &JsValue,
    context: Rc<RefCell<Option<JsValue>>>,
) -> Result<JsValue, JsValue> {
    let render = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let ctx = context
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("web boot: Client Context is unavailable"))?;
        let shell = call_method(&ctx, "get", &[JsValue::from_str("appShell")])?;
        if shell.is_undefined() {
            return Err(
                js_sys::Error::new("web boot: appShell service missing after settled").into(),
            );
        }
        call_method(&shell, "renderApp", &[])
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    create_element(
        &modules.react,
        &modules.app_root,
        Some(&object(&[
            ("settled", settled.clone()),
            ("status", status.clone()),
            ("error", error.clone()),
            ("renderApp", render.into_js_value()),
        ])?),
        &[],
    )
}

fn prefetch_immediate(system: &JsValue, manifest: &JsValue) -> Result<Promise, JsValue> {
    let promises = Array::new();
    let plugins = Array::from(&required_property(manifest, "plugins", "Boot manifest")?);
    for row in plugins.iter() {
        if Reflect::get(&row, &JsValue::from_str("immediately"))?.as_bool() != Some(true) {
            continue;
        }
        let id = required_property(&row, "id", "Boot plugin row")?;
        let promise = call_method(system, "prefetch", &[id])?.dyn_into::<Promise>()?;
        let ignore = Function::new_no_args("return undefined");
        promises.push(&call_method(promise.as_ref(), "catch", &[ignore.into()])?);
    }
    Ok(Promise::all(&promises))
}

async fn run_plugin_boot(
    modules: &BrowserModules,
    ctx: &JsValue,
    system: &JsValue,
    manifest: &JsValue,
    status: &JsValue,
    prefetch: Promise,
) -> Result<(), JsValue> {
    JsFuture::from(
        call_method(ctx, "plugin", std::slice::from_ref(&modules.loader))?.dyn_into::<Promise>()?,
    )
    .await?;
    let loader = required_property(ctx, "loader", "Client Context")?;
    Reflect::set(&loader, &JsValue::from_str("internal"), system)?;
    let status_face = status.clone();
    let status_listener = Closure::wrap(Box::new(move |fiber: JsValue| -> Result<(), JsValue> {
        let entry = Reflect::get(&fiber, &JsValue::from_str("entry"))?;
        if entry.is_undefined() {
            return Ok(());
        }
        let root = Reflect::get(&entry, &JsValue::from_str("fiber"))?;
        if root.is_undefined() {
            return Ok(());
        }
        let options = required_property(&entry, "options", "Loader entry")?;
        let name = required_property(&options, "name", "Loader entry options")?;
        let state = required_property(&root, "state", "Loader entry fiber")?;
        call_method(
            &status_face,
            "set",
            &[name, JsValue::from_str(&fiber_label(&state)?)],
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    call_method(
        ctx,
        "on",
        &[
            JsValue::from_str("internal/status"),
            status_listener.into_js_value(),
        ],
    )?;
    JsFuture::from(prefetch).await?;

    let plugins = Array::from(&required_property(manifest, "plugins", "Boot manifest")?);
    let rows = Array::new();
    rows.push(&JsValue::from_str(MODULES_ID));
    for row in plugins.iter() {
        let id = required_property(&row, "id", "Boot plugin row")?;
        if id.as_string().as_deref() != Some(MODULES_ID) {
            rows.push(&id);
        }
    }
    rows.push(&JsValue::from_str(APP_SHELL_ID));
    let creates = Array::new();
    for name in rows.iter() {
        call_method(status, "set", &[name.clone(), JsValue::from_str("loading")])?;
        let options = object(&[("name", name)])?;
        creates.push(&call_method(&loader, "create", &[options.into()])?);
    }
    let entry_ids = Array::from(&JsFuture::from(Promise::all(&creates)).await?);
    for index in 0..entry_ids.length() {
        let id = entry_ids.get(index);
        let entry = call_method(&loader, "resolve", std::slice::from_ref(&id))?;
        if Reflect::get(&entry, &JsValue::from_str("fiber"))?.is_undefined() {
            call_method(
                status,
                "set",
                &[rows.get(index), JsValue::from_str("failed")],
            )?;
        }
    }
    JsFuture::from(call_method(&loader, "await", &[])?.dyn_into::<Promise>()?).await?;
    assert_loader_entries_active(ctx, &loader)?;
    Ok(())
}

fn assert_loader_entries_active(ctx: &JsValue, loader: &JsValue) -> Result<(), JsValue> {
    let entries = Array::from(&call_method(loader, "entries", &[])?);
    let mut failures = Vec::new();
    for entry in entries.iter() {
        let options = required_property(&entry, "options", "Loader entry")?;
        let name = required_property(&options, "name", "Loader entry options")?
            .as_string()
            .unwrap_or_default();
        let fiber = Reflect::get(&entry, &JsValue::from_str("fiber"))?;
        if fiber.is_undefined() {
            failures.push(format!(
                "{name}: import failed (see console for the import error)"
            ));
            continue;
        }
        let state = fiber_label(&required_property(&fiber, "state", "Loader fiber")?)?;
        if state == "active" {
            continue;
        }
        if state == "pending" {
            let inject = Object::keys(&Object::from(required_property(
                &fiber,
                "inject",
                "Loader fiber",
            )?));
            let mut missing = Vec::new();
            for service in inject.iter().filter_map(|service| service.as_string()) {
                if call_method(ctx, "get", &[JsValue::from_str(&service)])?.is_undefined() {
                    missing.push(service);
                }
            }
            let noun = if missing.len() == 1 {
                "service"
            } else {
                "services"
            };
            failures.push(format!(
                "{name}: pending (waiting for {noun}: {})",
                if missing.is_empty() {
                    "unknown".to_owned()
                } else {
                    missing.join(", ")
                }
            ));
        } else {
            failures.push(format!("{name}: {state}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        let noun = if failures.len() == 1 {
            "entry"
        } else {
            "entries"
        };
        Err(js_sys::Error::new(&format!(
            "web boot: {} {noun} did not activate\n{}",
            failures.len(),
            failures.join("\n")
        ))
        .into())
    }
}

fn fiber_label(value: &JsValue) -> Result<String, JsValue> {
    match value.as_f64() {
        Some(0.0) => Ok("pending".to_owned()),
        Some(1.0) => Ok("loading".to_owned()),
        Some(2.0) => Ok("active".to_owned()),
        Some(3.0) => Ok("failed".to_owned()),
        Some(4.0) => Ok("disposed".to_owned()),
        Some(5.0) => Ok("unloading".to_owned()),
        _ => Err(js_sys::Error::new("web boot: unknown fiber state").into()),
    }
}

fn use_signal(react: &JsValue, signal: &JsValue) -> Result<JsValue, JsValue> {
    function(react, "useSyncExternalStore")?.call2(
        react,
        &required_property(signal, "subscribe", "Kernel signal")?,
        &required_property(signal, "getSnapshot", "Kernel signal")?,
    )
}

fn inject_styles() -> Result<(), JsValue> {
    let document = required_property(&js_sys::global(), "document", "global")?;
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin"),
            JsValue::from_str("@seekdeep-ai/seekdeep-client-web"),
        ],
    )?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-shell-style"),
            JsValue::from_str("app-root"),
        ],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(APP_ROOT_STYLES),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn component(render: fn(JsValue) -> Result<JsValue, JsValue>) -> JsValue {
    Closure::wrap(Box::new(move |props: JsValue| render(props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

fn tag(
    react: &JsValue,
    name: &str,
    class_name: &str,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    create_element(
        react,
        &JsValue::from_str(name),
        Some(&object(&[("className", JsValue::from_str(class_name))])?),
        children,
    )
}

fn fragment(react: &JsValue, children: &[JsValue]) -> Result<JsValue, JsValue> {
    let fragment = required_property(react, "Fragment", "React")?;
    create_element(react, &fragment, None, children)
}

fn create_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let args = Array::new();
    args.push(kind);
    args.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        args.push(child);
    }
    function(react, "createElement")?.apply(react, &args)
}

fn construct(constructor: &JsValue, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let constructor = constructor.clone().dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    Reflect::construct(&constructor, &args)
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-web module factory did not configure shell modules").into()
        })
    })
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() {
        Err(js_sys::Error::new(&format!("client app-shell requires service {name:?}")).into())
    } else {
        Ok(service)
    }
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_sys::Error::new(&format!(
            "client-web: {owner} omitted required property {key:?}"
        ))
        .into())
    } else {
        Ok(property)
    }
}

fn function(value: &JsValue, name: &str) -> Result<Function, JsValue> {
    required_property(value, name, "JavaScript object")?.dyn_into::<Function>()
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        set_property(&object, key, value)?;
    }
    Ok(object)
}

fn set_property(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value).map(|_| ())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = function(value, name)?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

#[allow(clippy::redundant_closure_for_method_calls)]
fn console_error(error: &JsValue) {
    if let Ok(console) = Reflect::get(&js_sys::global(), &JsValue::from_str("console"))
        && let Ok(log) = Reflect::get(&console, &JsValue::from_str("error"))
            .and_then(|value| value.dyn_into::<Function>())
    {
        let _ = log.call1(&console, error);
    }
}

fn error_message(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
        .unwrap_or_else(|| {
            js_sys::JsString::from(error.clone())
                .as_string()
                .unwrap_or_default()
        })
}
