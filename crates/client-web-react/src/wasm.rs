//! Browser hook facade for React and the external-store selector shim.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use js_sys::{Array, Function, Object, Reflect, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::InvokeCounter;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    use_selector: Function,
    host_context: JsValue,
    binding_context: JsValue,
    hook_cache: WeakMap,
    projection_cache: WeakMap,
    locale_subscription_cache: WeakMap,
    locale_seat_cache: WeakMap,
    standard_props_cache: WeakMap,
    absent_source: JsValue,
    absent_hook: Option<Function>,
    error_boundary: JsValue,
    session_provider_component: JsValue,
    session_maybe_provider_component: JsValue,
    root_outlet_component: JsValue,
    slot_outlet_component: JsValue,
    entry_wrapper_component: JsValue,
    entry_body_component: JsValue,
    render_slot_cache: WeakMap,
    render_chain_cache: WeakMap,
    root_inject_cache: WeakMap,
    session_inject_cache: WeakMap,
    session_maybe_inject_cache: WeakMap,
    entry_key_cache: WeakMap,
    next_entry_key: Rc<Cell<u64>>,
    slot_assembly_error: Function,
    stale_authorization_error: Function,
    slot_ownership_error: Function,
}

struct InvokeCellState {
    counter: InvokeCounter,
    action: Function,
    listeners: Vec<Function>,
}

/// Configures React and `useSyncExternalStoreWithSelector`.
///
/// # Errors
///
/// Returns a malformed selector-shim failure.
#[wasm_bindgen(js_name = configureClientWebReact)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_web_react(react: JsValue, use_selector: JsValue) -> Result<(), JsValue> {
    let use_selector = use_selector.dyn_into::<Function>()?;
    let create_context = function(&react, "createContext")?;
    let host_context = create_context.call1(&react, &JsValue::NULL)?;
    let binding_context = create_context.call1(&react, &JsValue::NULL)?;
    let absent_source = absent_source()?;
    let error_boundary = error_boundary_class(&react)?;
    let slot_assembly_error = error_class("SlotAssemblyError")?;
    let stale_authorization_error = error_class("StaleAuthorizationError")?;
    let slot_ownership_error = error_class("SlotOwnershipError")?;
    let session_provider_component = component(session_provider);
    let session_maybe_provider_component = component(session_maybe_provider);
    let root_outlet_component = component(render_root_outlet);
    let slot_outlet_component = component(render_slot_outlet);
    let entry_wrapper_component = component(render_entry_wrapper);
    let entry_body_component = component(render_entry_body);
    MODULES.with(|slot| {
        *slot.borrow_mut() = Some(BrowserModules {
            react,
            use_selector,
            host_context,
            binding_context,
            hook_cache: WeakMap::new(),
            projection_cache: WeakMap::new(),
            locale_subscription_cache: WeakMap::new(),
            locale_seat_cache: WeakMap::new(),
            standard_props_cache: WeakMap::new(),
            absent_source,
            absent_hook: None,
            error_boundary,
            session_provider_component,
            session_maybe_provider_component,
            root_outlet_component,
            slot_outlet_component,
            entry_wrapper_component,
            entry_body_component,
            render_slot_cache: WeakMap::new(),
            render_chain_cache: WeakMap::new(),
            root_inject_cache: WeakMap::new(),
            session_inject_cache: WeakMap::new(),
            session_maybe_inject_cache: WeakMap::new(),
            entry_key_cache: WeakMap::new(),
            next_entry_key: Rc::new(Cell::new(0)),
            slot_assembly_error,
            stale_authorization_error,
            slot_ownership_error,
        });
    });
    Ok(())
}

/// Builds the package-local `useSyncExternalStoreWithSelector` compatibility hook.
///
/// # Errors
///
/// Returns malformed React-hook failures.
#[wasm_bindgen(js_name = createSelectorShim)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_selector_shim(react: JsValue) -> Result<Function, JsValue> {
    Ok(Closure::wrap(Box::new(
        move |subscribe: Function,
              get_snapshot: Function,
              _server_snapshot: JsValue,
              selector: Function,
              equal: JsValue|
              -> Result<JsValue, JsValue> {
            let reference = function(&react, "useRef")?.call1(&react, &JsValue::NULL)?;
            let mut cell = Reflect::get(&reference, &JsValue::from_str("current"))?;
            if cell.is_null() {
                cell = object(&[
                    ("has", JsValue::FALSE),
                    ("snapshot", JsValue::UNDEFINED),
                    ("selector", JsValue::UNDEFINED),
                    ("selection", JsValue::UNDEFINED),
                ])?
                .into();
                Reflect::set(&reference, &JsValue::from_str("current"), &cell)?;
            }
            let selected_cell = cell;
            let selected_snapshot = get_snapshot;
            let selected_selector = selector;
            let selected_equal = equal;
            let get_selected = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
                let snapshot = selected_snapshot.call0(&JsValue::UNDEFINED)?;
                let has = Reflect::get(&selected_cell, &JsValue::from_str("has"))?
                    .as_bool()
                    .unwrap_or(false);
                let previous_snapshot =
                    Reflect::get(&selected_cell, &JsValue::from_str("snapshot"))?;
                let previous_selector =
                    Reflect::get(&selected_cell, &JsValue::from_str("selector"))?;
                if has
                    && Object::is(&snapshot, &previous_snapshot)
                    && Object::is(selected_selector.as_ref(), &previous_selector)
                {
                    return Reflect::get(&selected_cell, &JsValue::from_str("selection"));
                }
                let next = selected_selector.call1(&JsValue::UNDEFINED, &snapshot)?;
                let previous = Reflect::get(&selected_cell, &JsValue::from_str("selection"))?;
                let chosen = if has && selected_equal.is_function() {
                    let equal = selected_equal.clone().dyn_into::<Function>()?;
                    if equal
                        .call2(&JsValue::UNDEFINED, &previous, &next)?
                        .as_bool()
                        == Some(true)
                    {
                        previous
                    } else {
                        next
                    }
                } else {
                    next
                };
                Reflect::set(&selected_cell, &JsValue::from_str("has"), &JsValue::TRUE)?;
                Reflect::set(&selected_cell, &JsValue::from_str("snapshot"), &snapshot)?;
                Reflect::set(
                    &selected_cell,
                    &JsValue::from_str("selector"),
                    selected_selector.as_ref(),
                )?;
                Reflect::set(&selected_cell, &JsValue::from_str("selection"), &chosen)?;
                Ok(chosen)
            })
                as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
            function(&react, "useSyncExternalStore")?.call2(
                &react,
                subscribe.as_ref(),
                &get_selected.into_js_value(),
            )
        },
    )
        as Box<
            dyn FnMut(Function, Function, JsValue, Function, JsValue) -> Result<JsValue, JsValue>,
        >)
    .into_js_value()
    .unchecked_into())
}

/// Binds one bare observable source to a stable selector hook.
///
/// # Errors
///
/// Returns malformed source or configured-module failures.
#[wasm_bindgen(js_name = bindSnapshotSelector)]
#[allow(clippy::needless_pass_by_value)]
pub fn bind_snapshot_selector(source: JsValue) -> Result<Function, JsValue> {
    let modules = configured_modules()?;
    let subscribe_source = source.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        call_method(&subscribe_source, "subscribe", &[listener.into()])?.dyn_into::<Function>()
    })
        as Box<dyn FnMut(Function) -> Result<Function, JsValue>>);
    let snapshot_source = source;
    let get_snapshot =
        Closure::wrap(
            Box::new(move || call_method(&snapshot_source, "getSnapshot", &[]))
                as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
        );
    let subscribe = subscribe.into_js_value();
    let get_snapshot = get_snapshot.into_js_value();
    let selector_hook = modules.use_selector;
    Ok(
        Closure::wrap(Box::new(move |selector: Function, equal: JsValue| {
            let args = Array::new();
            args.push(&subscribe);
            args.push(&get_snapshot);
            args.push(&JsValue::UNDEFINED);
            args.push(&selector);
            args.push(&equal);
            selector_hook.apply(&JsValue::UNDEFINED, &args)
        })
            as Box<dyn FnMut(Function, JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value()
        .unchecked_into(),
    )
}

/// Wraps the latest asynchronous action as a stable trigger and pending selector.
///
/// # Errors
///
/// Returns malformed React, action, or Promise failures.
#[wasm_bindgen(js_name = useInvoke)]
#[allow(clippy::needless_pass_by_value)]
pub fn use_invoke(action: JsValue) -> Result<Array, JsValue> {
    let modules = configured_modules()?;
    let action = action.dyn_into::<Function>()?;
    let reference = function(&modules.react, "useRef")?.call1(&modules.react, &JsValue::NULL)?;
    let mut cell = Reflect::get(&reference, &JsValue::from_str("current"))?;
    if cell.is_null() {
        cell = create_invoke_cell(action.clone())?;
        Reflect::set(&reference, &JsValue::from_str("current"), &cell)?;
    } else {
        call_method(&cell, "setFn", &[action.into()])?;
    }
    let pending = function(&modules.react, "useSyncExternalStore")?.call2(
        &modules.react,
        &required_property(&cell, "subscribe", "invoke cell")?,
        &required_property(&cell, "getPending", "invoke cell")?,
    )?;
    Ok(Array::of2(
        &required_property(&cell, "invoke", "invoke cell")?,
        &pending,
    ))
}

/// Returns the installed renderer host or fails outside the root tree.
///
/// # Errors
///
/// Returns the source assembly diagnostic when no provider is installed.
#[wasm_bindgen(js_name = useHost)]
pub fn use_host() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let host =
        function(&modules.react, "useContext")?.call1(&modules.react, &modules.host_context)?;
    if host.is_null() || host.is_undefined() {
        Err(assembly_error(
            "slot machinery rendered outside the installed renderer tree",
        ))
    } else {
        Ok(host)
    }
}

/// Returns the current-session-optional provide bundle.
///
/// # Errors
///
/// Returns the source assembly diagnostic outside the binding provider.
#[wasm_bindgen(js_name = useSessionMaybeProvideInfo)]
pub fn use_session_maybe_provide_info() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let info =
        function(&modules.react, "useContext")?.call1(&modules.react, &modules.binding_context)?;
    if info.is_null() || info.is_undefined() {
        Err(assembly_error(
            "session-aware slot rendered outside the root binding provider",
        ))
    } else {
        Ok(info)
    }
}

/// Returns a strict session bundle.
///
/// # Errors
///
/// Returns the source diagnostic outside a provider or while no session exists.
#[wasm_bindgen(js_name = useSessionProvideInfo)]
pub fn use_session_provide_info() -> Result<JsValue, JsValue> {
    let info = use_session_maybe_provide_info()?;
    if Reflect::get(&info, &JsValue::from_str("sessionId"))?.is_undefined() {
        Err(assembly_error(
            "strict session slot rendered without a session",
        ))
    } else {
        Ok(info)
    }
}

/// Identity-stable selector hook per Host observable.
///
/// # Errors
///
/// Returns malformed source or configuration failures.
#[wasm_bindgen(js_name = observableHook)]
#[allow(clippy::needless_pass_by_value)]
pub fn observable_hook(source: JsValue) -> Result<Function, JsValue> {
    let modules = configured_modules()?;
    if !source.is_object() || source.is_null() {
        return Err(js_sys::TypeError::new("observable source must be an object").into());
    }
    let source_object = Object::from(source.clone());
    let cached = modules.hook_cache.get(&source_object);
    if !cached.is_undefined() {
        return cached.dyn_into::<Function>();
    }
    let hook = bind_snapshot_selector(source.clone())?;
    modules.hook_cache.set(&source_object, &hook);
    Ok(hook)
}

/// Binds an optional source while preserving hook order.
///
/// # Errors
///
/// Returns malformed source or configuration failures.
#[wasm_bindgen(js_name = maybeObservableHook)]
#[allow(clippy::needless_pass_by_value)]
pub fn maybe_observable_hook(source: JsValue) -> Result<Function, JsValue> {
    if !source.is_undefined() && !source.is_null() {
        return observable_hook(source);
    }
    if let Some(hook) = MODULES.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|modules| modules.absent_hook.clone())
    }) {
        return Ok(hook);
    }
    let absent = configured_modules()?.absent_source;
    let hook = observable_hook(absent)?;
    let absent_selector = Function::new_with_args("_snapshot", "return undefined");
    let output = Closure::wrap(Box::new(move |_selector: JsValue, _equal: JsValue| {
        hook.call2(&JsValue::UNDEFINED, &absent_selector, &JsValue::UNDEFINED)?;
        Ok(JsValue::UNDEFINED)
    })
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
    .unchecked_into::<Function>();
    MODULES.with(|slot| {
        if let Some(modules) = slot.borrow_mut().as_mut() {
            modules.absent_hook = Some(output.clone());
        }
    });
    Ok(output)
}

/// Returns one stable key-addressed projection selector hook per provide bundle.
///
/// # Errors
///
/// Returns malformed projection faces or selector failures.
#[wasm_bindgen(js_name = projectionHook)]
#[allow(clippy::needless_pass_by_value)]
pub fn projection_hook(info: JsValue) -> Result<Function, JsValue> {
    let modules = configured_modules()?;
    let info_object = Object::from(info.clone());
    let cached = modules.projection_cache.get(&info_object);
    if !cached.is_undefined() {
        return cached.dyn_into::<Function>();
    }
    let hook_info = info.clone();
    let identity = Function::new_with_args("value", "return value");
    let absent = modules.absent_source;
    let hook = Closure::wrap(Box::new(
        move |key: String, selector: JsValue, equal: JsValue| -> Result<JsValue, JsValue> {
            let projections = Reflect::get(&hook_info, &JsValue::from_str("projections"))?;
            let source = if projections.is_undefined() || projections.is_null() {
                absent.clone()
            } else {
                let source = call_method(&projections, "faceOf", &[JsValue::from_str(&key)])?;
                if source.is_undefined() || source.is_null() {
                    absent.clone()
                } else {
                    source
                }
            };
            let use_value = observable_hook(source)?;
            let selector = if selector.is_function() {
                selector
            } else {
                identity.clone().into()
            };
            use_value.call2(&JsValue::UNDEFINED, &selector, &equal)
        },
    )
        as Box<dyn FnMut(String, JsValue, JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
    .unchecked_into::<Function>();
    modules.projection_cache.set(&info_object, &hook);
    Ok(hook)
}

/// Root-level current-session binding provider.
///
/// # Errors
///
/// Returns missing-host or malformed React failures.
#[wasm_bindgen(js_name = SessionMaybeProvider)]
#[allow(clippy::needless_pass_by_value)]
pub fn session_maybe_provider(props: JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let info = current_provide_info(&modules.react)?;
    provider_element(
        &modules,
        &info,
        &required_property(&props, "children", "SessionMaybeProvider props")?,
        None,
    )
}

/// Render-prop session provider with keyed remount semantics.
///
/// # Errors
///
/// Returns missing-host, malformed render-prop, or React failures.
#[wasm_bindgen(js_name = SessionProvider)]
#[allow(clippy::needless_pass_by_value)]
pub fn session_provider(props: JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let info = current_provide_info(&modules.react)?;
    let id = Reflect::get(&info, &JsValue::from_str("sessionId"))?;
    if id.is_undefined() {
        let empty = Reflect::get(&props, &JsValue::from_str("empty"))?;
        let child = if empty.is_function() {
            empty.dyn_into::<Function>()?.call0(&JsValue::UNDEFINED)?
        } else {
            JsValue::NULL
        };
        return fragment(&modules.react, &child);
    }
    let children =
        required_property(&props, "children", "SessionProvider props")?.dyn_into::<Function>()?;
    let child = children.call1(&JsValue::UNDEFINED, &id)?;
    provider_element(&modules, &info, &child, Some(id))
}

/// Builds the renderer installed into the Rust Client Slot service.
///
/// # Errors
///
/// Returns configure-before-use or React object-construction failures.
#[wasm_bindgen(js_name = createSlotRenderer)]
pub fn create_slot_renderer() -> Result<JsValue, JsValue> {
    configured_modules()?;
    let render = Closure::wrap(Box::new(
        move |host: JsValue, owner_props: JsValue| -> Result<JsValue, JsValue> {
            let modules = configured_modules()?;
            let root = create_element(
                &modules.react,
                &modules.root_outlet_component,
                Some(&object(&[("ownerProps", owner_props)])?),
                &[],
            )?;
            let session = create_element(
                &modules.react,
                &modules.session_maybe_provider_component,
                None,
                &[root],
            )?;
            let provider = required_property(&modules.host_context, "Provider", "Host Context")?;
            create_element(
                &modules.react,
                &provider,
                Some(&object(&[("value", host)])?),
                &[session],
            )
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    object(&[("renderRoot", render.into_js_value())]).map(Into::into)
}

/// Compatibility constructors for the public error exports.
///
/// # Errors
///
/// Returns a configure-before-use failure.
#[wasm_bindgen(js_name = webReactErrorClasses)]
pub fn web_react_error_classes() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    object(&[
        ("SlotAssemblyError", modules.slot_assembly_error.into()),
        (
            "StaleAuthorizationError",
            modules.stale_authorization_error.into(),
        ),
        ("SlotOwnershipError", modules.slot_ownership_error.into()),
    ])
    .map(Into::into)
}

/// Stable `SessionProvider` component export.
///
/// # Errors
///
/// Returns a configure-before-use failure.
#[wasm_bindgen(js_name = sessionProviderComponent)]
pub fn session_provider_component() -> Result<JsValue, JsValue> {
    Ok(configured_modules()?.session_provider_component)
}

fn component(render: fn(JsValue) -> Result<JsValue, JsValue>) -> JsValue {
    Closure::wrap(Box::new(move |props: JsValue| render(props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

fn error_boundary_class(react: &JsValue) -> Result<JsValue, JsValue> {
    let factory = Function::new_with_args(
        "React",
        "return class SlotErrorBoundary extends React.Component {\n  constructor(props) { super(props); this.state = { failed: false }; }\n  static getDerivedStateFromError(error) { if (error && error.name === 'SlotAssemblyError') throw error; return { failed: true }; }\n  componentDidCatch(error) { console.error(`slot entry crashed in '${this.props.slotKey}':`, error); this.props.onEntryError(error); }\n  render() { return this.state.failed ? React.createElement('div', { 'data-slot-error': this.props.slotKey }) : this.props.children; }\n}",
    );
    factory.call1(&JsValue::UNDEFINED, react)
}

fn error_class(name: &str) -> Result<Function, JsValue> {
    Function::new_with_args(
        "name",
        "return class extends Error { constructor(message) { super(message); this.name = name; } }",
    )
    .call1(&JsValue::UNDEFINED, &JsValue::from_str(name))?
    .dyn_into::<Function>()
}

#[allow(clippy::needless_pass_by_value)]
fn render_root_outlet(props: JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let host = use_host()?;
    use_slot_version(&modules, &host, "root")?;
    use_locale_revision(&modules, &host)?;
    let winners = Array::from(&call_method(
        &host,
        "entriesOfSlot",
        &[JsValue::from_str("root")],
    )?);
    let entry = winners.get(0);
    if entry.is_undefined() {
        let raw = Array::from(&call_method(
            &host,
            "entriesOf",
            &[JsValue::from_str("root")],
        )?);
        if raw.length() > 0 {
            return error_face(&modules, "root");
        }
        return Err(assembly_error(
            "renderSlot('root') before any 'root' registration (boot order)",
        ));
    }
    let owner = required_property(&props, "ownerProps", "RootOutlet props")?;
    let spec = object(&[
        ("kind", JsValue::from_str("single")),
        ("scope", JsValue::from_str("root")),
    ])?;
    let guarded = guarded_entry(
        &modules,
        &host,
        "root",
        &entry,
        &owner,
        spec.as_ref(),
        &JsValue::UNDEFINED,
        &JsValue::UNDEFINED,
    )?;
    let style = object(&[("display", JsValue::from_str("contents"))])?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("data-slot", JsValue::from_str("root")),
            ("style", style.into()),
        ])?),
        &[guarded],
    )
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::needless_pass_by_value)]
fn render_slot_outlet(props: JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let host = use_host()?;
    let slot_key = required_string(&props, "slotKey", "SlotOutlet props")?;
    let owner = required_property(&props, "ownerProps", "SlotOutlet props")?;
    let opts = Reflect::get(&props, &JsValue::from_str("opts"))?;
    use_slot_version(&modules, &host, &slot_key)?;
    use_locale_revision(&modules, &host)?;
    let info = use_session_maybe_provide_info()?;
    let spec = call_method(&host, "specOf", &[JsValue::from_str(&slot_key)])?;
    let content = if spec.is_undefined() || spec.is_null() {
        JsValue::NULL
    } else {
        let kind = required_string(&spec, "kind", "Slot spec")?;
        let scope = required_string(&spec, "scope", "Slot spec")?;
        let id = Reflect::get(&info, &JsValue::from_str("sessionId"))?;
        let strict_absent = scope == "session" && id.is_undefined();
        let overlay = option_bool(&opts, "overlay");
        if strict_absent && (kind != "chain" || !overlay) {
            option_value(&opts, "fallback")
        } else {
            let entries = if strict_absent {
                Array::new()
            } else {
                Array::from(&call_method(
                    &host,
                    "entriesOf",
                    &[JsValue::from_str(&slot_key)],
                )?)
            };
            let winners = if strict_absent {
                Array::new()
            } else {
                Array::from(&call_method(
                    &host,
                    "entriesOfSlot",
                    &[JsValue::from_str(&slot_key)],
                )?)
            };
            let slot_injected = Reflect::get(&spec, &JsValue::from_str("inject"))?;
            match kind.as_str() {
                "single" => {
                    let entry = winners.get(0);
                    if entry.is_undefined() {
                        if entries.length() > 0 {
                            error_face(&modules, &slot_key)?
                        } else {
                            option_value(&opts, "fallback")
                        }
                    } else {
                        guarded_entry(
                            &modules,
                            &host,
                            &slot_key,
                            &entry,
                            &owner,
                            &spec,
                            &slot_injected,
                            &opts,
                        )?
                    }
                }
                "keyed" => keyed_dispatch(
                    &modules,
                    &host,
                    &slot_key,
                    &owner,
                    &opts,
                    &spec,
                    &slot_injected,
                    &entries,
                    &winners,
                )?,
                "chain" => chain_dispatch(
                    &modules,
                    &host,
                    &slot_key,
                    &owner,
                    &opts,
                    &spec,
                    &slot_injected,
                    &entries,
                )?,
                "list" => list_dispatch(
                    &modules,
                    &host,
                    &slot_key,
                    &owner,
                    &opts,
                    &spec,
                    &slot_injected,
                    &entries,
                    &winners,
                )?,
                _ => {
                    return Err(assembly_error(&format!(
                        "slot '{slot_key}' has unknown kind '{kind}'"
                    )));
                }
            }
        }
    };
    slot_anchor(&modules.react, &slot_key, &content)
}

#[allow(clippy::too_many_arguments)]
fn keyed_dispatch(
    modules: &BrowserModules,
    host: &JsValue,
    slot_key: &str,
    owner: &JsValue,
    opts: &JsValue,
    spec: &JsValue,
    slot_injected: &JsValue,
    entries: &Array,
    winners: &Array,
) -> Result<JsValue, JsValue> {
    let requested = option_value(opts, "entryKey");
    let matches = |entry: &JsValue| -> bool {
        entry_option(entry, "key").is_ok_and(|key| Object::is(&key, &requested))
    };
    if let Some(entry) = winners.iter().find(matches) {
        return guarded_entry(
            modules,
            host,
            slot_key,
            &entry,
            owner,
            spec,
            slot_injected,
            opts,
        );
    }
    if entries.iter().any(|entry| matches(&entry)) {
        error_face(modules, slot_key)
    } else {
        Ok(option_value(opts, "fallback"))
    }
}

#[allow(clippy::too_many_arguments)]
fn chain_dispatch(
    modules: &BrowserModules,
    host: &JsValue,
    slot_key: &str,
    owner: &JsValue,
    opts: &JsValue,
    spec: &JsValue,
    slot_injected: &JsValue,
    entries: &Array,
) -> Result<JsValue, JsValue> {
    let mut elected = JsValue::NULL;
    for entry in entries.iter() {
        let select = required_property(&entry, "select", "chain entry")?.dyn_into::<Function>()?;
        let matched = match select.call1(&JsValue::UNDEFINED, owner) {
            Ok(matched) => matched,
            Err(error) => {
                log_chain_error(slot_key, &entry, &error);
                continue;
            }
        };
        if matched.is_null() {
            continue;
        }
        let selected_owner = Object::assign(&Object::new(), &Object::from(owner.clone()));
        Reflect::set(&selected_owner, &JsValue::from_str("matched"), &matched)?;
        elected = guarded_entry(
            modules,
            host,
            slot_key,
            &entry,
            selected_owner.as_ref(),
            spec,
            slot_injected,
            opts,
        )?;
        break;
    }
    let fallback = option_value(opts, "fallback");
    if !option_bool(opts, "overlay") {
        return Ok(if elected.is_null() { fallback } else { elected });
    }
    let fallback_wrapper = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("data-chain-overlay-fallback", JsValue::from_str(slot_key)),
            (
                "style",
                object(&[(
                    "display",
                    JsValue::from_str(if elected.is_null() {
                        "contents"
                    } else {
                        "none"
                    }),
                )])?
                .into(),
            ),
        ])?),
        &[fallback],
    )?;
    fragment_children(&modules.react, &[fallback_wrapper, elected])
}

struct ListRow {
    entry: Option<JsValue>,
    id: JsValue,
    order: f64,
}

#[allow(clippy::too_many_arguments)]
fn list_dispatch(
    modules: &BrowserModules,
    host: &JsValue,
    slot_key: &str,
    owner: &JsValue,
    opts: &JsValue,
    spec: &JsValue,
    slot_injected: &JsValue,
    entries: &Array,
    winners: &Array,
) -> Result<JsValue, JsValue> {
    let mut rows = winners
        .iter()
        .map(|entry| {
            let id = entry_option(&entry, "id")?;
            let order = entry_number(&entry, "order").unwrap_or(0.0);
            Ok(ListRow {
                entry: Some(entry),
                id,
                order,
            })
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    for entry in entries.iter() {
        let id = entry_option(&entry, "id")?;
        if rows.iter().any(|row| Object::is(&row.id, &id)) {
            continue;
        }
        rows.push(ListRow {
            entry: None,
            id,
            order: entry_number(&entry, "order").unwrap_or(0.0),
        });
    }
    rows.sort_by(|left, right| left.order.total_cmp(&right.order));
    let only = option_value(opts, "only");
    if !only.is_undefined() {
        rows.retain(|row| Object::is(&row.id, &only));
    }
    if rows.is_empty() {
        return Ok(option_value(opts, "fallback"));
    }
    let mut children = Vec::with_capacity(rows.len());
    for row in rows {
        children.push(if let Some(entry) = row.entry {
            guarded_entry(
                modules,
                host,
                slot_key,
                &entry,
                owner,
                spec,
                slot_injected,
                opts,
            )?
        } else {
            error_face(modules, slot_key)?
        });
    }
    fragment_children(&modules.react, &children)
}

fn slot_anchor(react: &JsValue, slot_key: &str, child: &JsValue) -> Result<JsValue, JsValue> {
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("data-slot", JsValue::from_str(slot_key)),
            (
                "style",
                object(&[("display", JsValue::from_str("contents"))])?.into(),
            ),
        ])?),
        std::slice::from_ref(child),
    )
}

fn fragment_children(react: &JsValue, children: &[JsValue]) -> Result<JsValue, JsValue> {
    let fragment = required_property(react, "Fragment", "React")?;
    create_element(react, &fragment, None, children)
}

fn option_value(value: &JsValue, key: &str) -> JsValue {
    if value.is_undefined() || value.is_null() {
        JsValue::UNDEFINED
    } else {
        Reflect::get(value, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
    }
}

fn option_bool(value: &JsValue, key: &str) -> bool {
    option_value(value, key).as_bool().unwrap_or(false)
}

fn entry_option(entry: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let options = required_property(entry, "options", "stored entry")?;
    Reflect::get(&options, &JsValue::from_str(key))
}

fn entry_number(entry: &JsValue, key: &str) -> Option<f64> {
    entry_option(entry, key)
        .ok()
        .and_then(|value| value.as_f64())
}

#[allow(clippy::redundant_closure_for_method_calls)]
fn log_chain_error(slot_key: &str, entry: &JsValue, error: &JsValue) {
    let registrant = Reflect::get(entry, &JsValue::from_str("registrant"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| "unknown registrant".to_owned());
    let global = js_sys::global();
    if let Ok(console) = Reflect::get(&global, &JsValue::from_str("console"))
        && let Ok(log) = Reflect::get(&console, &JsValue::from_str("error"))
            .and_then(|value| value.dyn_into::<Function>())
    {
        let _ = log.call2(
            &console,
            &JsValue::from_str(&format!(
                "chain selector crashed in '{slot_key}' ({registrant}), treating as declined:"
            )),
            error,
        );
    }
}

fn render_entry_wrapper(props: JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let scope = required_property(&props, "scope", "Entry wrapper")?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("entry scope must be a string"))?;
    if scope == "root" {
        return create_element(
            &modules.react,
            &modules.entry_body_component,
            Some(&Object::from(props)),
            &[],
        );
    }
    let info = use_session_maybe_provide_info()?;
    let id = Reflect::get(&info, &JsValue::from_str("sessionId"))?;
    if scope == "session" {
        if id.is_undefined() {
            return Ok(JsValue::NULL);
        }
        let body = Object::assign(&Object::new(), &Object::from(props));
        Reflect::set(&body, &JsValue::from_str("info"), &info)?;
        Reflect::set(&body, &JsValue::from_str("key"), &id)?;
        return create_element(
            &modules.react,
            &modules.entry_body_component,
            Some(&body),
            &[],
        );
    }

    let initial = object(&[
        ("adopted", JsValue::UNDEFINED),
        ("epoch", JsValue::from_f64(0.0)),
    ])?;
    let state = Array::from(
        &function(&modules.react, "useState")?.call1(&modules.react, initial.as_ref())?,
    );
    let current = Object::from(state.get(0));
    let setter = state.get(1).dyn_into::<Function>()?;
    let adopted = Reflect::get(&current, &JsValue::from_str("adopted"))?;
    let mut epoch = required_number(current.as_ref(), "epoch")?;
    let mut next_adopted = adopted.clone();
    let changed = if !id.is_undefined() && adopted.is_undefined() {
        next_adopted = id.clone();
        true
    } else if !id.is_undefined() && !adopted.is_undefined() && !Object::is(&id, &adopted) {
        next_adopted = id.clone();
        epoch += 1.0;
        true
    } else if id.is_undefined() && !adopted.is_undefined() {
        next_adopted = JsValue::UNDEFINED;
        epoch += 1.0;
        true
    } else {
        false
    };
    if changed {
        setter.call1(
            &JsValue::UNDEFINED,
            object(&[
                ("adopted", next_adopted),
                ("epoch", JsValue::from_f64(epoch)),
            ])?
            .as_ref(),
        )?;
    }
    let body = Object::assign(&Object::new(), &Object::from(props));
    Reflect::set(&body, &JsValue::from_str("info"), &info)?;
    Reflect::set(&body, &JsValue::from_str("key"), &JsValue::from_f64(epoch))?;
    create_element(
        &modules.react,
        &modules.entry_body_component,
        Some(&body),
        &[],
    )
}

#[allow(clippy::needless_pass_by_value)]
fn render_entry_body(props: JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let host = use_host()?;
    let entry = required_property(&props, "entry", "Entry body")?;
    let owner = required_property(&props, "ownerProps", "Entry body")?;
    let scope = required_property(&props, "scope", "Entry body")?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("entry scope must be a string"))?;
    let info = Reflect::get(&props, &JsValue::from_str("info"))?;
    let info = (!info.is_undefined()).then_some(info);
    let (standard, mut kit) = standard_kit(&modules, &host, &entry, &scope, info.as_ref())?;
    let actions = Reflect::get(&kit, &JsValue::from_str("actions"))?;
    let injected = cached_entry_inject(
        &modules,
        &entry,
        &scope,
        info.as_ref(),
        (!actions.is_undefined()).then_some(&actions),
    )?;
    assign(&mut kit, &injected)?;
    let slot_injected = Reflect::get(&props, &JsValue::from_str("slotInjected"))?;
    let has_hook_context = Reflect::get(&props, &JsValue::from_str("hasHookContext"))?
        .as_bool()
        .unwrap_or(false);
    let hook_context = Reflect::get(&props, &JsValue::from_str("hookContext"))?;
    let slot_props = bind_slot_inject(&slot_injected, &standard, &hook_context, has_hook_context)?;
    assign(&mut kit, &slot_props)?;
    assign(&mut kit, &owner)?;
    let component = required_property(&entry, "component", "Stored entry")?;
    create_element(&modules.react, &component, Some(&kit), &[])
}

fn use_slot_version(modules: &BrowserModules, host: &JsValue, key: &str) -> Result<(), JsValue> {
    let subscribe_host = host.clone();
    let subscribe_key = key.to_owned();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        call_method(
            &subscribe_host,
            "subscribe",
            &[JsValue::from_str(&subscribe_key), listener.into()],
        )
    })
        as Box<dyn FnMut(Function) -> Result<JsValue, JsValue>>);
    let snapshot_host = host.clone();
    let snapshot_key = key.to_owned();
    let snapshot = Closure::wrap(Box::new(move || {
        call_method(
            &snapshot_host,
            "getVersion",
            &[JsValue::from_str(&snapshot_key)],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    function(&modules.react, "useSyncExternalStore")?.call2(
        &modules.react,
        &subscribe.into_js_value(),
        &snapshot.into_js_value(),
    )?;
    Ok(())
}

fn use_locale_revision(modules: &BrowserModules, host: &JsValue) -> Result<(), JsValue> {
    let locale = Reflect::get(host, &JsValue::from_str("locale"))?;
    let (subscribe, snapshot) = if locale.is_undefined() || locale.is_null() {
        let subscribe = Closure::wrap(Box::new(move |_listener: Function| -> Function {
            Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>)
                .into_js_value()
                .unchecked_into()
        }) as Box<dyn FnMut(Function) -> Function>);
        let snapshot = Closure::wrap(Box::new(move || 0.0) as Box<dyn FnMut() -> f64>);
        (subscribe.into_js_value(), snapshot.into_js_value())
    } else {
        let face = Object::from(locale.clone());
        let cached = modules.locale_subscription_cache.get(&face);
        let cached = if cached.is_undefined() {
            let subscribe_face = locale.clone();
            let subscribe = Closure::wrap(Box::new(move |listener: Function| {
                call_method(&subscribe_face, "subscribe", &[listener.into()])
            })
                as Box<dyn FnMut(Function) -> Result<JsValue, JsValue>>);
            let snapshot_face = locale;
            let snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
                let snapshot = call_method(&snapshot_face, "getSnapshot", &[])?;
                Reflect::get(&snapshot, &JsValue::from_str("revision"))
            })
                as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
            let cached = object(&[
                ("subscribe", subscribe.into_js_value()),
                ("snapshot", snapshot.into_js_value()),
            ])?;
            modules
                .locale_subscription_cache
                .set(&face, cached.as_ref());
            cached.into()
        } else {
            cached
        };
        (
            required_property(&cached, "subscribe", "locale subscription")?,
            required_property(&cached, "snapshot", "locale subscription")?,
        )
    };
    function(&modules.react, "useSyncExternalStore")?.call2(
        &modules.react,
        &subscribe,
        &snapshot,
    )?;
    Ok(())
}

fn locale_seat(
    modules: &BrowserModules,
    locale: &JsValue,
    namespace: &str,
) -> Result<JsValue, JsValue> {
    let face = Object::from(locale.clone());
    let cached = modules.locale_seat_cache.get(&face);
    let per_namespace = if cached.is_undefined() {
        let value = Object::new();
        modules.locale_seat_cache.set(&face, value.as_ref());
        value
    } else {
        Object::from(cached)
    };
    let revision = required_number(&call_method(locale, "getSnapshot", &[])?, "revision")?;
    let current = Reflect::get(&per_namespace, &JsValue::from_str(namespace))?;
    if !current.is_undefined()
        && required_number(&current, "revision")?
            .total_cmp(&revision)
            .is_eq()
    {
        return required_property(&current, "translate", "locale seat");
    }
    let bound =
        call_method(locale, "bind", &[JsValue::from_str(namespace)])?.dyn_into::<Function>()?;
    let translate = Closure::wrap(Box::new(move |key: JsValue, params: JsValue| {
        bound.call2(&JsValue::UNDEFINED, &key, &params)
    })
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    let translate = translate.into_js_value();
    Reflect::set(
        &per_namespace,
        &JsValue::from_str(namespace),
        object(&[
            ("revision", JsValue::from_f64(revision)),
            ("translate", translate.clone()),
        ])?
        .as_ref(),
    )?;
    Ok(translate)
}

#[allow(clippy::too_many_arguments)]
fn guarded_entry(
    modules: &BrowserModules,
    host: &JsValue,
    slot_key: &str,
    entry: &JsValue,
    owner: &JsValue,
    spec: &JsValue,
    slot_injected: &JsValue,
    opts: &JsValue,
) -> Result<JsValue, JsValue> {
    let scope = required_property(spec, "scope", "Slot spec")?;
    let has_hook_context = opts.is_object()
        && !opts.is_null()
        && Reflect::has(opts, &JsValue::from_str("hookContext"))?;
    let hook_context = if has_hook_context {
        Reflect::get(opts, &JsValue::from_str("hookContext"))?
    } else {
        JsValue::UNDEFINED
    };
    let wrapper_props = object(&[
        ("entry", entry.clone()),
        ("ownerProps", owner.clone()),
        ("scope", scope),
        ("slotKey", JsValue::from_str(slot_key)),
        ("slotInjected", slot_injected.clone()),
        ("hookContext", hook_context),
        ("hasHookContext", JsValue::from_bool(has_hook_context)),
    ])?;
    let wrapper = create_element(
        &modules.react,
        &modules.entry_wrapper_component,
        Some(&wrapper_props),
        &[],
    )?;
    let report_host = host.clone();
    let report_entry = entry.clone();
    let report_key = slot_key.to_owned();
    let chain = required_property(spec, "kind", "Slot spec")?
        .as_string()
        .as_deref()
        == Some("chain");
    let on_error = Closure::wrap(Box::new(move |error: JsValue| -> Result<(), JsValue> {
        call_method(
            &report_host,
            "reportEntryError",
            &[
                JsValue::from_str(&report_key),
                report_entry.clone(),
                error,
                object(&[("abdicate", JsValue::from_bool(!chain))])?.into(),
            ],
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let mut boundary_key = entry_key(modules, entry)?;
    if required_string(spec, "scope", "Slot spec")? == "session" {
        let info = use_session_maybe_provide_info()?;
        let session_id = Reflect::get(&info, &JsValue::from_str("sessionId"))?;
        if !session_id.is_undefined() {
            boundary_key = JsValue::from_str(&format!(
                "{}:{}",
                boundary_key.as_f64().unwrap_or_default(),
                session_id.as_string().unwrap_or_default()
            ));
        }
    }
    let boundary_props = object(&[
        ("slotKey", JsValue::from_str(slot_key)),
        ("key", boundary_key),
        ("onEntryError", on_error.into_js_value()),
    ])?;
    create_element(
        &modules.react,
        &modules.error_boundary,
        Some(&boundary_props),
        &[wrapper],
    )
}

#[allow(clippy::unnecessary_wraps)]
fn entry_key(modules: &BrowserModules, entry: &JsValue) -> Result<JsValue, JsValue> {
    let entry = Object::from(entry.clone());
    let cached = modules.entry_key_cache.get(&entry);
    if !cached.is_undefined() {
        return Ok(cached);
    }
    let key = modules.next_entry_key.get();
    modules.next_entry_key.set(key.wrapping_add(1));
    let key = JsValue::from_f64(
        key.to_string()
            .parse()
            .expect("u64 decimal text is a finite JavaScript number"),
    );
    modules.entry_key_cache.set(&entry, &key);
    Ok(key)
}

fn error_face(modules: &BrowserModules, key: &str) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[("data-slot-error", JsValue::from_str(key))])?),
        &[],
    )
}

fn standard_props(
    modules: &BrowserModules,
    host: &JsValue,
    scope: &str,
    info: Option<&JsValue>,
) -> Result<Object, JsValue> {
    let host_object = Object::from(host.clone());
    let cached = modules.standard_props_cache.get(&host_object);
    let cached = if cached.is_undefined() {
        let sessions = required_property(host, "sessions", "renderer Host")?;
        let workspaces = required_property(host, "workspaces", "renderer Host")?;
        let root = object(&[
            (
                "useSessions",
                observable_hook(required_property(&sessions, "list", "Session Host")?)?.into(),
            ),
            (
                "useWorkspaces",
                observable_hook(required_property(&workspaces, "list", "Workspace Host")?)?.into(),
            ),
        ])?;
        let value = object(&[
            ("root", root.into()),
            ("session", WeakMap::new().into()),
            ("maybe", WeakMap::new().into()),
        ])?;
        modules
            .standard_props_cache
            .set(&host_object, value.as_ref());
        value.into()
    } else {
        cached
    };
    if scope == "root" {
        return required_property(&cached, "root", "standard props cache").map(Object::from);
    }
    let info = Object::from(
        info.cloned()
            .ok_or_else(|| assembly_error("session scope rendered without provide info"))?,
    );
    let key = if scope == "session" {
        "session"
    } else {
        "maybe"
    };
    let by_info = required_property(&cached, key, "standard props cache")?.dyn_into::<WeakMap>()?;
    let existing = by_info.get(&info);
    if !existing.is_undefined() {
        return Ok(Object::from(existing));
    }
    let root = required_property(&cached, "root", "standard props cache")?;
    let standard = Object::assign(&Object::new(), &Object::from(root));
    let hooks = required_property(info.as_ref(), "hooks", "Session provide info")?;
    for hook_key in Object::keys(&Object::from(hooks.clone()))
        .iter()
        .filter_map(|key| key.as_string())
    {
        let source = Reflect::get(&hooks, &JsValue::from_str(&hook_key))?;
        let hook = if scope == "session-maybe" {
            maybe_observable_hook(source)?
        } else {
            if source.is_undefined() {
                return Err(assembly_error(&format!(
                    "strict session hook {hook_key:?} has no source"
                )));
            }
            observable_hook(source)?
        };
        Reflect::set(
            &standard,
            &JsValue::from_str(&hook_name(&hook_key)),
            hook.as_ref(),
        )?;
    }
    let provided = required_property(info.as_ref(), "props", "Session provide info")?;
    Object::assign(&standard, &Object::from(provided));
    let session_id = Reflect::get(info.as_ref(), &JsValue::from_str("sessionId"))?;
    Reflect::set(&standard, &JsValue::from_str("sessionId"), &session_id)?;
    Reflect::set(
        &standard,
        &JsValue::from_str("useProjection"),
        projection_hook(info.clone().into())?.as_ref(),
    )?;
    by_info.set(&info, standard.as_ref());
    Ok(standard)
}

#[allow(clippy::too_many_lines)]
fn standard_kit(
    modules: &BrowserModules,
    host: &JsValue,
    entry: &JsValue,
    scope: &str,
    info: Option<&JsValue>,
) -> Result<(Object, Object), JsValue> {
    let standard = standard_props(modules, host, scope, info)?;
    let kit = Object::assign(&Object::new(), &standard);
    let namespace = Reflect::get(entry, &JsValue::from_str("locale"))?;
    if !namespace.is_undefined() {
        let locale = Reflect::get(host, &JsValue::from_str("locale"))?;
        if locale.is_undefined() || locale.is_null() {
            return Err(assembly_error(&format!(
                "entry declares locale namespace {:?} but no locale face is installed (locale plugin missing from the composition?)",
                namespace.as_string().unwrap_or_default()
            )));
        }
        let translator = locale_seat(modules, &locale, &namespace.as_string().unwrap_or_default())?;
        Reflect::set(&kit, &JsValue::from_str("t"), &translator)?;
    }
    let scope_key = info
        .map(|info| Reflect::get(info, &JsValue::from_str("sessionId")))
        .transpose()?
        .unwrap_or(JsValue::UNDEFINED);
    let store = if scope == "session-maybe" && scope_key.is_undefined() {
        JsValue::UNDEFINED
    } else {
        call_method(host, "storeOf", &[entry.clone(), scope_key])?
    };
    if !store.is_undefined() {
        Reflect::set(
            &kit,
            &JsValue::from_str("useStore"),
            observable_hook(store.clone())?.as_ref(),
        )?;
        Reflect::set(
            &kit,
            &JsValue::from_str("actions"),
            &required_property(&store, "actions", "Store instance")?,
        )?;
    }
    let children = Reflect::get(entry, &JsValue::from_str("children"))?;
    if !children.is_undefined() {
        Reflect::set(
            &kit,
            &JsValue::from_str("renderSlot"),
            bound_render_slot(modules, host, entry, false)?.as_ref(),
        )?;
        if children_has_kind(&children, "chain")? {
            Reflect::set(
                &kit,
                &JsValue::from_str("renderSlotChain"),
                bound_render_slot(modules, host, entry, true)?.as_ref(),
            )?;
        }
        if children_has_scope(&children, "session")? {
            Reflect::set(
                &kit,
                &JsValue::from_str("SessionProvider"),
                &modules.session_provider_component,
            )?;
        }
    }
    Ok((standard, kit))
}

fn cached_entry_inject(
    modules: &BrowserModules,
    entry: &JsValue,
    scope: &str,
    info: Option<&JsValue>,
    actions: Option<&JsValue>,
) -> Result<JsValue, JsValue> {
    let entry_object = Object::from(entry.clone());
    let cache = match scope {
        "root" => &modules.root_inject_cache,
        "session" => &modules.session_inject_cache,
        _ => &modules.session_maybe_inject_cache,
    };
    if scope == "root" {
        let cached = cache.get(&entry_object);
        if !cached.is_undefined() {
            return Ok(cached);
        }
        let value = run_entry_inject(entry, info, actions)?;
        cache.set(&entry_object, &value);
        return Ok(value);
    }
    let info = Object::from(
        info.cloned()
            .ok_or_else(|| assembly_error("session entry rendered without provide info"))?,
    );
    let nested = cache.get(&entry_object);
    let nested = if nested.is_undefined() {
        let nested = WeakMap::new();
        cache.set(&entry_object, nested.as_ref());
        nested
    } else {
        nested.dyn_into::<WeakMap>()?
    };
    let cached = nested.get(&info);
    if !cached.is_undefined() {
        return Ok(cached);
    }
    let value = run_entry_inject(entry, Some(info.as_ref()), actions)?;
    nested.set(&info, &value);
    Ok(value)
}

fn run_entry_inject(
    entry: &JsValue,
    info: Option<&JsValue>,
    actions: Option<&JsValue>,
) -> Result<JsValue, JsValue> {
    let inject = Reflect::get(entry, &JsValue::from_str("inject"))?;
    if inject.is_undefined() {
        return Ok(Object::new().into());
    }
    let inject = inject.dyn_into::<Function>()?;
    let args = Array::new();
    if let Some(info) = info {
        args.push(&Reflect::get(info, &JsValue::from_str("sessionId"))?);
    }
    if let Some(actions) = actions {
        args.push(actions);
    }
    bind_entry_hooks(inject.apply(&JsValue::UNDEFINED, &args)?)
}

#[allow(clippy::needless_pass_by_value)]
fn bind_entry_hooks(face: JsValue) -> Result<JsValue, JsValue> {
    let output = Object::assign(&Object::new(), &Object::from(face.clone()));
    let hooks = Reflect::get(&face, &JsValue::from_str("hooks"))?;
    if hooks.is_undefined() {
        return Ok(output.into());
    }
    Reflect::delete_property(&output, &JsValue::from_str("hooks"))?;
    for key in Object::keys(&Object::from(hooks.clone()))
        .iter()
        .filter_map(|key| key.as_string())
    {
        Reflect::set(
            &output,
            &JsValue::from_str(&hook_name(&key)),
            observable_hook(Reflect::get(&hooks, &JsValue::from_str(&key))?)?.as_ref(),
        )?;
    }
    Ok(output.into())
}

fn bind_slot_inject(
    face: &JsValue,
    standard: &Object,
    hook_context: &JsValue,
    has_hook_context: bool,
) -> Result<JsValue, JsValue> {
    if face.is_undefined() || face.is_null() {
        return Ok(Object::new().into());
    }
    let output = Object::assign(&Object::new(), &Object::from(face.clone()));
    let hooks = Reflect::get(face, &JsValue::from_str("hooks"))?;
    if hooks.is_undefined() {
        return Ok(output.into());
    }
    Reflect::delete_property(&output, &JsValue::from_str("hooks"))?;
    let factories = Object::new();
    for key in Object::keys(&Object::from(hooks.clone()))
        .iter()
        .filter_map(|key| key.as_string())
    {
        let definition = Reflect::get(&hooks, &JsValue::from_str(&key))?;
        if definition.is_function() {
            Reflect::set(&factories, &JsValue::from_str(&key), &definition)?;
        } else {
            Reflect::set(
                &output,
                &JsValue::from_str(&hook_name(&key)),
                observable_hook(definition)?.as_ref(),
            )?;
        }
    }
    if Object::keys(&factories).length() > 0 {
        if !has_hook_context {
            return Err(assembly_error(
                "slot has contextual injected Hooks but no hookContext",
            ));
        }
        let compute_factories = factories.clone();
        let compute_standard = standard.clone();
        let compute_context = hook_context.clone();
        let compute = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let hooks = Object::new();
            for key in Object::keys(&compute_factories)
                .iter()
                .filter_map(|key| key.as_string())
            {
                let factory = Reflect::get(&compute_factories, &JsValue::from_str(&key))?
                    .dyn_into::<Function>()?;
                let hook = factory.call2(
                    &JsValue::UNDEFINED,
                    compute_standard.as_ref(),
                    &compute_context,
                )?;
                Reflect::set(&hooks, &JsValue::from_str(&hook_name(&key)), &hook)?;
            }
            Ok(hooks.into())
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        let dependencies = Array::of3(factories.as_ref(), standard.as_ref(), hook_context);
        let contextual = function(&configured_modules()?.react, "useMemo")?.call2(
            &JsValue::UNDEFINED,
            &compute.into_js_value(),
            &dependencies,
        )?;
        Object::assign(&output, &Object::from(contextual));
    }
    Ok(output.into())
}

fn bound_render_slot(
    modules: &BrowserModules,
    host: &JsValue,
    entry: &JsValue,
    chain: bool,
) -> Result<Function, JsValue> {
    let entry_object = Object::from(entry.clone());
    let cache = if chain {
        &modules.render_chain_cache
    } else {
        &modules.render_slot_cache
    };
    let cached = cache.get(&entry_object);
    if !cached.is_undefined() {
        return cached.dyn_into::<Function>();
    }
    let render_host = host.clone();
    let render_entry = entry.clone();
    let component = modules.slot_outlet_component.clone();
    let react = modules.react.clone();
    let render = Closure::wrap(Box::new(
        move |key: String, owner: JsValue, opts: JsValue| -> Result<JsValue, JsValue> {
            if call_method(&render_host, "isLive", std::slice::from_ref(&render_entry))?.as_bool()
                != Some(true)
            {
                return Err(authorization_error(
                    "StaleAuthorizationError",
                    &format!(
                        "{}('{key}') from a disposed registration",
                        if chain {
                            "renderSlotChain"
                        } else {
                            "renderSlot"
                        }
                    ),
                ));
            }
            let children = required_property(&render_entry, "children", "Stored entry")?;
            let spec = Reflect::get(&children, &JsValue::from_str(&key))?;
            if spec.is_undefined() {
                return Err(authorization_error(
                    "SlotOwnershipError",
                    &format!("slot '{key}' is not declared by this entry's children"),
                ));
            }
            let kind = required_property(&spec, "kind", "child Slot spec")?
                .as_string()
                .unwrap_or_default();
            if chain != (kind == "chain") {
                let message = if chain {
                    format!("slot '{key}' is declared '{kind}', not 'chain' — use renderSlot")
                } else {
                    format!("slot '{key}' is declared 'chain' — use renderSlotChain")
                };
                return Err(authorization_error("SlotOwnershipError", &message));
            }
            create_element(
                &react,
                &component,
                Some(&object(&[
                    ("slotKey", JsValue::from_str(&key)),
                    ("ownerProps", owner),
                    ("opts", opts),
                ])?),
                &[],
            )
        },
    )
        as Box<dyn FnMut(String, JsValue, JsValue) -> Result<JsValue, JsValue>>);
    let render: Function = render.into_js_value().unchecked_into();
    cache.set(&entry_object, render.as_ref());
    Ok(render)
}

fn children_has_kind(children: &JsValue, expected: &str) -> Result<bool, JsValue> {
    for key in Object::keys(&Object::from(children.clone())).iter() {
        if required_property(&Reflect::get(children, &key)?, "kind", "child Slot spec")?
            .as_string()
            .as_deref()
            == Some(expected)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn children_has_scope(children: &JsValue, expected: &str) -> Result<bool, JsValue> {
    for key in Object::keys(&Object::from(children.clone())).iter() {
        if required_property(&Reflect::get(children, &key)?, "scope", "child Slot spec")?
            .as_string()
            .as_deref()
            == Some(expected)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn hook_name(name: &str) -> String {
    let mut characters = name.chars();
    characters.next().map_or_else(
        || "use".to_owned(),
        |first| format!("use{}{}", first.to_ascii_uppercase(), characters.as_str()),
    )
}

fn authorization_error(name: &str, message: &str) -> JsValue {
    configured_modules()
        .and_then(|modules| {
            let constructor = if name == "StaleAuthorizationError" {
                modules.stale_authorization_error
            } else {
                modules.slot_ownership_error
            };
            Reflect::construct(&constructor, &Array::of1(&JsValue::from_str(message)))
        })
        .unwrap_or_else(|_| js_sys::Error::new(message).into())
}

#[allow(clippy::unnecessary_wraps)]
fn assign(target: &mut Object, source: &JsValue) -> Result<(), JsValue> {
    if source.is_undefined() || source.is_null() {
        return Ok(());
    }
    Object::assign(target, &Object::from(source.clone()));
    Ok(())
}

fn create_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(kind);
    arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        arguments.push(child);
    }
    function(react, "createElement")?.apply(react, &arguments)
}

fn required_number(value: &JsValue, key: &str) -> Result<f64, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a number")).into())
}

fn current_provide_info(_react: &JsValue) -> Result<JsValue, JsValue> {
    let host = use_host()?;
    let sessions = required_property(&host, "sessions", "renderer host")?;
    let source = required_property(&sessions, "provideInfo", "renderer sessions")?;
    let hook = observable_hook(source)?;
    let identity = Function::new_with_args("value", "return value");
    hook.call2(&JsValue::UNDEFINED, &identity, &JsValue::UNDEFINED)
}

fn provider_element(
    modules: &BrowserModules,
    info: &JsValue,
    child: &JsValue,
    key: Option<JsValue>,
) -> Result<JsValue, JsValue> {
    let provider = required_property(&modules.binding_context, "Provider", "binding context")?;
    let props = Object::new();
    Reflect::set(&props, &JsValue::from_str("value"), info)?;
    if let Some(key) = key {
        Reflect::set(&props, &JsValue::from_str("key"), &key)?;
    }
    react_element(&modules.react, &provider, &props.into(), child)
}

fn fragment(react: &JsValue, child: &JsValue) -> Result<JsValue, JsValue> {
    let fragment = required_property(react, "Fragment", "React")?;
    react_element(react, &fragment, &JsValue::NULL, child)
}

fn react_element(
    react: &JsValue,
    kind: &JsValue,
    props: &JsValue,
    child: &JsValue,
) -> Result<JsValue, JsValue> {
    let args = Array::of3(kind, props, child);
    function(react, "createElement")?.apply(react, &args)
}

fn absent_source() -> Result<JsValue, JsValue> {
    let snapshot =
        Closure::wrap(Box::new(move || JsValue::UNDEFINED) as Box<dyn FnMut() -> JsValue>);
    let subscribe = Closure::wrap(Box::new(move |_listener: Function| -> Function {
        Closure::wrap(Box::new(move || {}) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    object(&[
        ("getSnapshot", snapshot.into_js_value()),
        ("subscribe", subscribe.into_js_value()),
    ])
    .map(Into::into)
}

fn assembly_error(message: &str) -> JsValue {
    configured_modules()
        .and_then(|modules| {
            Reflect::construct(
                &modules.slot_assembly_error,
                &Array::of1(&JsValue::from_str(message)),
            )
        })
        .unwrap_or_else(|_| js_sys::Error::new(message).into())
}

#[allow(clippy::redundant_closure_for_method_calls)]
fn create_invoke_cell(action: Function) -> Result<JsValue, JsValue> {
    let state = Rc::new(RefCell::new(InvokeCellState {
        counter: InvokeCounter::default(),
        action,
        listeners: Vec::new(),
    }));
    let invoke_state = Rc::clone(&state);
    let invoke = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let changed = invoke_state.borrow_mut().counter.begin();
        if changed {
            notify(&invoke_state);
        }
        let action = invoke_state.borrow().action.clone();
        let promise = action.call0(&JsValue::UNDEFINED)?;
        let failure = Closure::wrap(Box::new(move |error: JsValue| {
            let global = js_sys::global();
            if let Ok(console) = Reflect::get(&global, &JsValue::from_str("console"))
                && let Ok(error_fn) = Reflect::get(&console, &JsValue::from_str("error"))
                    .and_then(|value| value.dyn_into::<Function>())
            {
                let _ = error_fn.call2(
                    &console,
                    &JsValue::from_str("useInvoke action failed:"),
                    &error,
                );
            }
            JsValue::UNDEFINED
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        let settled = call_method(&promise, "catch", &[failure.into_js_value()])?;
        let finish_state = Rc::clone(&invoke_state);
        let finish = Closure::wrap(Box::new(move || {
            let changed = finish_state.borrow_mut().counter.finish();
            if changed {
                notify(&finish_state);
            }
        }) as Box<dyn FnMut()>);
        call_method(&settled, "finally", &[finish.into_js_value()])?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);

    let subscribe_state = Rc::clone(&state);
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        subscribe_state
            .borrow_mut()
            .listeners
            .push(listener.clone());
        let cleanup_state = Rc::clone(&subscribe_state);
        Closure::wrap(Box::new(move || {
            cleanup_state
                .borrow_mut()
                .listeners
                .retain(|candidate| !Object::is(candidate, &listener));
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    let pending_state = Rc::clone(&state);
    let get_pending = Closure::wrap(
        Box::new(move || pending_state.borrow().counter.pending()) as Box<dyn FnMut() -> bool>
    );
    let set_state = state;
    let set_fn = Closure::wrap(Box::new(move |action: Function| {
        set_state.borrow_mut().action = action;
    }) as Box<dyn FnMut(Function)>);
    object(&[
        ("invoke", invoke.into_js_value()),
        ("subscribe", subscribe.into_js_value()),
        ("getPending", get_pending.into_js_value()),
        ("setFn", set_fn.into_js_value()),
    ])
    .map(Into::into)
}

fn notify(state: &Rc<RefCell<InvokeCellState>>) {
    let listeners = state.borrow().listeners.clone();
    for listener in listeners {
        let _ = listener.call0(&JsValue::UNDEFINED);
    }
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|slot| {
        slot.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-web-react module factory did not configure React bindings")
                .into()
        })
    })
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_sys::Error::new(&format!(
            "client-web-react: {owner} omitted required property {key:?}"
        ))
        .into())
    } else {
        Ok(property)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key:?} must be a string")).into())
}

fn function(value: &JsValue, name: &str) -> Result<Function, JsValue> {
    required_property(value, name, "JavaScript object")?.dyn_into::<Function>()
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(object.as_ref(), &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let callable = function(value, name)?;
    let args: Array = arguments.iter().collect();
    callable.apply(value, &args)
}
