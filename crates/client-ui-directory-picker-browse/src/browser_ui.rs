//! Compiled React directory browser over the persistent Rust controller face.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    DRAFT_PREVIEW_DEBOUNCE_MS, DirectoryBrowserState, DirectoryEntry, FocusRequest, LandingOptions,
    PARENT_LEG_WAIT_MS, SLOW_SCAN_DELAY_MS, create_directory_browser_state_controller,
    display_crumbs, target_name, target_path, visible_entries,
};

const PACKAGE_ID: &str = "@seekdeep-ai/seekdeep-client-ui-directory-picker-browse";
const BROWSER_CSS: &str = include_str!(
    "../../../packages/client/ui-directory-picker-browse/src/client/DirectoryBrowser.module.css"
);

thread_local! {
    static COMPONENTS: RefCell<Option<Components>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct Components {
    browser: JsValue,
    flow: JsValue,
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    primitives: JsValue,
}

impl BrowserModules {
    fn primitive(&self, name: &str) -> Result<JsValue, JsValue> {
        required(&self.primitives, name, "UI primitives")
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::TypeError::new(&format!("{owner} is missing {key}")).into())
    } else {
        Ok(property)
    }
}

fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!property.is_null() && !property.is_undefined()).then_some(property))
}

fn function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{owner} {key} must be a function")).into())
}

fn call(target: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let values = Array::new();
    for argument in arguments {
        values.push(argument);
    }
    function(target, name, "object")?.apply(target, &values)
}

fn element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let values = Array::new();
    values.push(kind);
    values.push(props.map_or(&JsValue::NULL, Object::as_ref));
    for child in children {
        values.push(child);
    }
    function(react, "createElement", "React")?.apply(react, &values)
}

fn tag(
    react: &JsValue,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, &JsValue::from_str(name), props, children)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    function(react, "useRef", "React")?.call1(react, initial)
}

fn use_effect(react: &JsValue, effect: JsValue, deps: &Array) -> Result<(), JsValue> {
    let result = function(react, "useEffect", "React")?
        .call2(react, &effect, deps)
        .map(|_| ());
    drop(effect);
    result
}

fn use_effect_each_render(react: &JsValue, effect: JsValue) -> Result<(), JsValue> {
    let result = function(react, "useEffect", "React")?
        .call1(react, &effect)
        .map(|_| ());
    drop(effect);
    result
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn encode<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true))
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

fn decode<T: DeserializeOwned>(value: JsValue, owner: &str) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| js_sys::TypeError::new(&format!("invalid {owner}: {error}")).into())
}

#[allow(clippy::needless_pass_by_value)] // Temporary JS payloads transfer directly into Function::call.
fn dispatch(controller: &JsValue, action: &str, payload: JsValue) -> Result<JsValue, JsValue> {
    function(controller, "dispatch", "directory browser controller")?.call2(
        controller,
        &JsValue::from_str(action),
        &payload,
    )
}

fn snapshot(controller: &JsValue) -> Result<DirectoryBrowserState, JsValue> {
    decode(
        function(controller, "snapshot", "directory browser controller")?.call0(controller)?,
        "directory browser snapshot",
    )
}

fn accepted(result: Result<JsValue, JsValue>) -> bool {
    result.ok().and_then(|value| value.as_bool()) == Some(true)
}

fn force(set_revision: &Function) {
    let update =
        Closure::wrap(Box::new(move |value: f64| value + 1.0) as Box<dyn FnMut(f64) -> f64>);
    let _ = set_revision.call1(&JsValue::UNDEFINED, &update.into_js_value());
}

fn css(name: &str) -> String {
    format!("seekdeep-directory-browser-{name}")
}

fn classes(names: &[(&str, bool)]) -> String {
    names
        .iter()
        .filter(|(_, enabled)| *enabled)
        .map(|(name, _)| css(name))
        .collect::<Vec<_>>()
        .join(" ")
}

fn prefix_css(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(source.len() + 256);
    let mut offset = 0;
    let mut quote = None;
    let mut comment = false;
    while offset < bytes.len() {
        if comment {
            output.push(bytes[offset]);
            if bytes[offset] == b'*' && bytes.get(offset + 1) == Some(&b'/') {
                output.push(b'/');
                offset += 2;
                comment = false;
            } else {
                offset += 1;
            }
            continue;
        }
        if let Some(delimiter) = quote {
            output.push(bytes[offset]);
            if bytes[offset] == b'\\' && offset + 1 < bytes.len() {
                output.push(bytes[offset + 1]);
                offset += 2;
            } else {
                if bytes[offset] == delimiter {
                    quote = None;
                }
                offset += 1;
            }
            continue;
        }
        if bytes[offset] == b'/' && bytes.get(offset + 1) == Some(&b'*') {
            output.extend_from_slice(b"/*");
            offset += 2;
            comment = true;
            continue;
        }
        if matches!(bytes[offset], b'\'' | b'"') {
            quote = Some(bytes[offset]);
            output.push(bytes[offset]);
            offset += 1;
            continue;
        }
        if bytes[offset] == b'.'
            && bytes
                .get(offset + 1)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(*byte, b'_' | b'-'))
        {
            output.extend_from_slice(b".seekdeep-directory-browser-");
            offset += 1;
            while offset < bytes.len()
                && (bytes[offset].is_ascii_alphanumeric() || matches!(bytes[offset], b'_' | b'-'))
            {
                output.push(bytes[offset]);
                offset += 1;
            }
            continue;
        }
        output.push(bytes[offset]);
        offset += 1;
    }
    String::from_utf8(output).expect("CSS prefixing preserves UTF-8")
}

fn inject_style() -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let identity = format!("{PACKAGE_ID}/DirectoryBrowser.module.css");
    if let Ok(query) = function(&document, "querySelector", "document")
        && !query
            .call1(
                &document,
                &JsValue::from_str(&format!("[data-plugin-css=\"{identity}\"]")),
            )?
            .is_null()
    {
        return Ok(());
    }
    let style = call(&document, "createElement", &[JsValue::from_str("style")])?;
    for (key, value) in [
        ("data-plugin-css", identity.as_str()),
        ("data-plugin", PACKAGE_ID),
    ] {
        call(
            &style,
            "setAttribute",
            &[JsValue::from_str(key), JsValue::from_str(value)],
        )?;
    }
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(&prefix_css(BROWSER_CSS)),
    )?;
    call(
        &required(&document, "head", "document")?,
        "appendChild",
        &[style],
    )?;
    Ok(())
}

fn rejection_text(reason: &JsValue) -> String {
    let name = Reflect::get(reason, &JsValue::from_str("name"))
        .ok()
        .and_then(|name| name.as_string());
    if name.as_deref() == Some("DirectoryBrowseError") {
        return Reflect::get(reason, &JsValue::from_str("rpcError"))
            .ok()
            .and_then(|rpc| Reflect::get(&rpc, &JsValue::from_str("message")).ok())
            .and_then(|message| message.as_string())
            .unwrap_or_default();
    }
    if reason.is_instance_of::<js_sys::Error>() {
        return Reflect::get(reason, &JsValue::from_str("message"))
            .ok()
            .and_then(|message| message.as_string())
            .unwrap_or_default();
    }
    function(&js_sys::global(), "String", "globalThis")
        .and_then(|string| string.call1(&JsValue::UNDEFINED, reason))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

fn new_abort_controller() -> Result<JsValue, JsValue> {
    let constructor =
        required(&js_sys::global(), "AbortController", "globalThis")?.dyn_into::<Function>()?;
    Reflect::construct(&constructor, &Array::new())
}

fn abort_current(scan_ref: &JsValue) -> Result<(), JsValue> {
    let scan = current(scan_ref)?;
    if !scan.is_null() && !scan.is_undefined() {
        call(&scan, "abort", &[])?;
        set_current(scan_ref, &JsValue::NULL)?;
    }
    Ok(())
}

fn arm_slow_scan(
    controller: &JsValue,
    set_revision: &Function,
    scan_window: JsValue,
) -> Result<(), JsValue> {
    let controller = controller.clone();
    let setter = set_revision.clone();
    let callback = Closure::wrap(Box::new(move || {
        if accepted(dispatch(
            &controller,
            "slowScanElapsed",
            object(&[("seq", scan_window.clone())])
                .map(JsValue::from)
                .unwrap_or(JsValue::UNDEFINED),
        )) {
            force(&setter);
        }
    }) as Box<dyn FnMut()>);
    call(
        &js_sys::global(),
        "setTimeout",
        &[
            callback.into_js_value(),
            JsValue::from_f64(f64::from(SLOW_SCAN_DELAY_MS)),
        ],
    )?;
    Ok(())
}

fn physical_listing(
    list_directory: &Function,
    scan_ref: &JsValue,
    controller: &JsValue,
    set_revision: &Function,
    launch: &JsValue,
) -> Result<Promise, JsValue> {
    let previous = current(scan_ref)?;
    if !previous.is_null() && !previous.is_undefined() {
        let _ = call(&previous, "abort", &[]);
    }
    let abort = new_abort_controller()?;
    set_current(scan_ref, &abort)?;
    let signal = required(&abort, "signal", "AbortController")?;
    let path = optional(launch, "path")?.unwrap_or(JsValue::UNDEFINED);
    arm_slow_scan(
        controller,
        set_revision,
        required(launch, "scanWindow", "listing launch")?,
    )?;
    Ok(Promise::resolve(&list_directory.call2(
        &JsValue::UNDEFINED,
        &path,
        &signal,
    )?))
}

fn attach_settlement(
    promise: &Promise,
    success: Closure<dyn FnMut(JsValue)>,
    failure: Closure<dyn FnMut(JsValue)>,
) {
    let _ = promise.then2(&success, &failure);
    drop(success.into_js_value());
    drop(failure.into_js_value());
}

fn start_parent_leg(
    controller: &JsValue,
    list_directory: &Function,
    scan_ref: &JsValue,
    set_revision: &Function,
    parent: &JsValue,
) -> Result<(), JsValue> {
    let launch = object(&[
        ("seq", required(parent, "seq", "parent leg")?),
        ("path", required(parent, "path", "parent leg")?),
        ("scanWindow", required(parent, "scanWindow", "parent leg")?),
    ])?;
    let promise = physical_listing(
        list_directory,
        scan_ref,
        controller,
        set_revision,
        launch.as_ref(),
    )?;
    let success_controller = controller.clone();
    let success_setter = set_revision.clone();
    let seq = required(parent, "seq", "parent leg")?;
    let success_seq = seq.clone();
    let success = Closure::wrap(Box::new(move |listing: JsValue| {
        if accepted(dispatch(
            &success_controller,
            "parentLanded",
            object(&[("seq", success_seq.clone()), ("parent", listing)])
                .map(JsValue::from)
                .unwrap_or(JsValue::UNDEFINED),
        )) {
            force(&success_setter);
        }
    }) as Box<dyn FnMut(JsValue)>);
    let failure_controller = controller.clone();
    let failure_setter = set_revision.clone();
    let failure_seq = seq.clone();
    let failure = Closure::wrap(Box::new(move |_reason: JsValue| {
        if accepted(dispatch(
            &failure_controller,
            "parentFailed",
            object(&[("seq", failure_seq.clone())])
                .map(JsValue::from)
                .unwrap_or(JsValue::UNDEFINED),
        )) {
            force(&failure_setter);
        }
    }) as Box<dyn FnMut(JsValue)>);
    attach_settlement(&promise, success, failure);
    if required(parent, "boundedWait", "parent leg")?.as_bool() == Some(true) {
        let wait_controller = controller.clone();
        let wait_setter = set_revision.clone();
        let callback = Closure::wrap(Box::new(move || {
            if accepted(dispatch(
                &wait_controller,
                "parentWaitElapsed",
                object(&[("seq", seq.clone())])
                    .map(JsValue::from)
                    .unwrap_or(JsValue::UNDEFINED),
            )) {
                force(&wait_setter);
            }
        }) as Box<dyn FnMut()>);
        call(
            &js_sys::global(),
            "setTimeout",
            &[
                callback.into_js_value(),
                JsValue::from_f64(f64::from(PARENT_LEG_WAIT_MS)),
            ],
        )?;
    }
    Ok(())
}

fn start_landing(
    controller: &JsValue,
    list_directory: &Function,
    scan_ref: &JsValue,
    set_revision: &Function,
    launch: JsValue,
    options: LandingOptions,
) -> Result<(), JsValue> {
    let promise = physical_listing(list_directory, scan_ref, controller, set_revision, &launch)?;
    let success_controller = controller.clone();
    let success_list = list_directory.clone();
    let success_scan = scan_ref.clone();
    let success_setter = set_revision.clone();
    let success_launch = launch.clone();
    let success_options = encode(&options)?;
    let success = Closure::wrap(Box::new(move |target: JsValue| {
        let Ok(outcome) = dispatch(
            &success_controller,
            "targetLanded",
            object(&[
                ("launch", success_launch.clone()),
                ("target", target),
                ("options", success_options.clone()),
            ])
            .map(JsValue::from)
            .unwrap_or(JsValue::UNDEFINED),
        ) else {
            return;
        };
        let kind = Reflect::get(&outcome, &JsValue::from_str("kind"))
            .ok()
            .and_then(|kind| kind.as_string());
        if kind.as_deref() == Some("stale") {
            return;
        }
        force(&success_setter);
        if kind.as_deref() == Some("parent")
            && let Ok(parent) = Reflect::get(&outcome, &JsValue::from_str("parent"))
        {
            let _ = start_parent_leg(
                &success_controller,
                &success_list,
                &success_scan,
                &success_setter,
                &parent,
            );
        }
    }) as Box<dyn FnMut(JsValue)>);
    let failure_controller = controller.clone();
    let failure_setter = set_revision.clone();
    let failure_launch = launch;
    let failure_options = encode(&options)?;
    let failure = Closure::wrap(Box::new(move |reason: JsValue| {
        if accepted(dispatch(
            &failure_controller,
            "targetFailed",
            object(&[
                (
                    "seq",
                    Reflect::get(&failure_launch, &JsValue::from_str("seq"))
                        .unwrap_or(JsValue::UNDEFINED),
                ),
                ("options", failure_options.clone()),
                ("message", JsValue::from_str(&rejection_text(&reason))),
            ])
            .map(JsValue::from)
            .unwrap_or(JsValue::UNDEFINED),
        )) {
            force(&failure_setter);
        }
    }) as Box<dyn FnMut(JsValue)>);
    attach_settlement(&promise, success, failure);
    Ok(())
}

#[allow(clippy::needless_pass_by_value)] // The owned launch is shared by asynchronous settlement closures.
fn start_selection_listing(
    controller: &JsValue,
    list_directory: &Function,
    scan_ref: &JsValue,
    set_revision: &Function,
    launch: JsValue,
) -> Result<(), JsValue> {
    let promise = physical_listing(list_directory, scan_ref, controller, set_revision, &launch)?;
    let success_controller = controller.clone();
    let success_setter = set_revision.clone();
    let seq = required(&launch, "seq", "selection launch")?;
    let success_seq = seq.clone();
    let success = Closure::wrap(Box::new(move |listing: JsValue| {
        if accepted(dispatch(
            &success_controller,
            "selectionLanded",
            object(&[("seq", success_seq.clone()), ("listing", listing)])
                .map(JsValue::from)
                .unwrap_or(JsValue::UNDEFINED),
        )) {
            force(&success_setter);
        }
    }) as Box<dyn FnMut(JsValue)>);
    let failure_controller = controller.clone();
    let failure_setter = set_revision.clone();
    let failure = Closure::wrap(Box::new(move |reason: JsValue| {
        if accepted(dispatch(
            &failure_controller,
            "selectionFailed",
            object(&[
                ("seq", seq.clone()),
                ("message", JsValue::from_str(&rejection_text(&reason))),
            ])
            .map(JsValue::from)
            .unwrap_or(JsValue::UNDEFINED),
        )) {
            force(&failure_setter);
        }
    }) as Box<dyn FnMut(JsValue)>);
    attach_settlement(&promise, success, failure);
    Ok(())
}

#[allow(clippy::too_many_lines)] // Creation, relist, and child preview are one source transaction.
fn start_creation(
    controller: &JsValue,
    create_directory: &Function,
    list_directory: &Function,
    scan_ref: &JsValue,
    set_revision: &Function,
    launch: JsValue,
) -> Result<(), JsValue> {
    let result = create_directory.call2(
        &JsValue::UNDEFINED,
        &required(&launch, "path", "create launch")?,
        &required(&launch, "name", "create launch")?,
    )?;
    let success_controller = controller.clone();
    let success_list = list_directory.clone();
    let success_scan = scan_ref.clone();
    let success_setter = set_revision.clone();
    let success_launch = launch.clone();
    let success = Closure::wrap(Box::new(move |created_path: JsValue| {
        let relist = dispatch(
            &success_controller,
            "creationSucceeded",
            object(&[
                ("launch", success_launch.clone()),
                ("createdPath", created_path),
            ])
            .map(JsValue::from)
            .unwrap_or(JsValue::UNDEFINED),
        );
        let Ok(relist) = relist else { return };
        if relist.is_null() || relist.is_undefined() {
            return;
        }
        force(&success_setter);
        let Ok(listing_launch) = Reflect::get(&relist, &JsValue::from_str("listing")) else {
            return;
        };
        let Ok(promise) = physical_listing(
            &success_list,
            &success_scan,
            &success_controller,
            &success_setter,
            &listing_launch,
        ) else {
            return;
        };
        let relist_controller = success_controller.clone();
        let relist_list = success_list.clone();
        let relist_scan = success_scan.clone();
        let relist_setter = success_setter.clone();
        let relist_seq =
            Reflect::get(&listing_launch, &JsValue::from_str("seq")).unwrap_or(JsValue::UNDEFINED);
        let landed_seq = relist_seq.clone();
        let landed = Closure::wrap(Box::new(move |listing: JsValue| {
            let child = dispatch(
                &relist_controller,
                "creationRelistLanded",
                object(&[("seq", landed_seq.clone()), ("listing", listing)])
                    .map(JsValue::from)
                    .unwrap_or(JsValue::UNDEFINED),
            );
            let Ok(child) = child else { return };
            if child.is_null() || child.is_undefined() {
                return;
            }
            force(&relist_setter);
            let _ = start_selection_listing(
                &relist_controller,
                &relist_list,
                &relist_scan,
                &relist_setter,
                child,
            );
        }) as Box<dyn FnMut(JsValue)>);
        let failed_controller = success_controller.clone();
        let failed_setter = success_setter.clone();
        let failed = Closure::wrap(Box::new(move |reason: JsValue| {
            if accepted(dispatch(
                &failed_controller,
                "creationRelistFailed",
                object(&[
                    ("seq", relist_seq.clone()),
                    ("message", JsValue::from_str(&rejection_text(&reason))),
                ])
                .map(JsValue::from)
                .unwrap_or(JsValue::UNDEFINED),
            )) {
                force(&failed_setter);
            }
        }) as Box<dyn FnMut(JsValue)>);
        attach_settlement(&promise, landed, failed);
    }) as Box<dyn FnMut(JsValue)>);
    let failure_controller = controller.clone();
    let failure_setter = set_revision.clone();
    let failure_launch = launch;
    let failure = Closure::wrap(Box::new(move |reason: JsValue| {
        if accepted(dispatch(
            &failure_controller,
            "creationFailed",
            object(&[
                ("launch", failure_launch.clone()),
                ("message", JsValue::from_str(&rejection_text(&reason))),
            ])
            .map(JsValue::from)
            .unwrap_or(JsValue::UNDEFINED),
        )) {
            force(&failure_setter);
        }
    }) as Box<dyn FnMut(JsValue)>);
    attach_settlement(&Promise::resolve(&result), success, failure);
    Ok(())
}

fn controller_ref(react: &JsValue) -> Result<(JsValue, JsValue), JsValue> {
    let reference = use_ref(react, &JsValue::NULL)?;
    let mut controller = current(&reference)?;
    if controller.is_null() || controller.is_undefined() {
        controller = create_directory_browser_state_controller()?;
        set_current(&reference, &controller)?;
    }
    Ok((reference, controller))
}

/// Configures React, UI primitives, and the compiled browser stylesheet.
///
/// # Errors
///
/// Returns missing dependency or DOM stylesheet failures.
#[wasm_bindgen(js_name = configureClientUiDirectoryPickerBrowse)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_directory_picker_browse(
    react: JsValue,
    primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "useEffect", "useRef", "useState"] {
        function(&react, method, "React")?;
    }
    required(&react, "Fragment", "React")?;
    for primitive in [
        "Button",
        "IconCheckOutline16",
        "IconChevronRightOutline14",
        "IconEditOutline16",
        "IconFolderClose16",
        "IconFolderOpen16",
        "IconPlusOutline16",
        "Modal",
    ] {
        required(&primitives, primitive, "UI primitives")?;
    }
    inject_style()?;
    let modules = BrowserModules { react, primitives };
    let rendered = modules.clone();
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_directory_browser(&rendered, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
    let browser = component.into_js_value();
    let flow_modules = modules;
    let flow =
        Closure::wrap(
            Box::new(move |props: JsValue| render_browse_flow(&flow_modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(Components {
            browser,
            flow: flow.into_js_value(),
        });
    });
    Ok(())
}

/// Returns the compiled `DirectoryBrowser` component.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = directoryBrowserComponent)]
pub fn directory_browser_component() -> Result<JsValue, JsValue> {
    COMPONENTS.with(|components| {
        components
            .borrow()
            .clone()
            .map(|components| components.browser)
            .ok_or_else(|| {
                js_sys::Error::new("client-ui-directory-picker-browse is not configured").into()
            })
    })
}

/// Returns the compiled `BrowseDirectoryFlow` component.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = browseDirectoryFlowComponent)]
pub fn browse_directory_flow_component() -> Result<JsValue, JsValue> {
    COMPONENTS.with(|components| {
        components
            .borrow()
            .clone()
            .map(|components| components.flow)
            .ok_or_else(|| {
                js_sys::Error::new("client-ui-directory-picker-browse is not configured").into()
            })
    })
}

fn render_browse_flow(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let source = props.clone().dyn_into::<Object>()?;
    let browser_props = Object::assign(&Object::new(), &source);
    Reflect::set(
        &browser_props,
        &JsValue::from_str("onOpen"),
        &required(props, "onPicked", "BrowseDirectoryFlow props")?,
    )?;
    Reflect::set(
        &browser_props,
        &JsValue::from_str("onClose"),
        &required(props, "onCancel", "BrowseDirectoryFlow props")?,
    )?;
    element(
        &modules.react,
        &directory_browser_component()?,
        Some(&browser_props),
        &[],
    )
}

fn translated(
    translate: &Function,
    key: &str,
    values: Option<&Object>,
) -> Result<JsValue, JsValue> {
    values.map_or_else(
        || translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key)),
        |values| translate.call2(&JsValue::UNDEFINED, &JsValue::from_str(key), values),
    )
}

#[allow(clippy::too_many_arguments)] // Mirrors the source LevelColumn presentational contract.
fn render_column(
    modules: &BrowserModules,
    entries: &[DirectoryEntry],
    selected_path: Option<&str>,
    busy: bool,
    show_hidden: bool,
    filter_prefix: Option<&str>,
    path_editing: bool,
    on_pick: &Function,
) -> Result<JsValue, JsValue> {
    let mut rows = Vec::new();
    for entry in visible_entries(entries, selected_path, show_hidden, filter_prefix) {
        let selected = selected_path == Some(entry.path.as_str());
        let pick = on_pick.clone();
        let picked = entry.clone();
        let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            pick.call1(&JsValue::UNDEFINED, &encode(&picked)?)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let on_mouse_down = path_editing.then(|| {
            Closure::wrap(Box::new(move |event: JsValue| {
                let _ = call(&event, "preventDefault", &[]);
            }) as Box<dyn FnMut(JsValue)>)
            .into_js_value()
        });
        let icon = element(
            &modules.react,
            &modules.primitive(if selected {
                "IconFolderOpen16"
            } else {
                "IconFolderClose16"
            })?,
            Some(&object(&[
                ("size", JsValue::from_f64(16.0)),
                (
                    "className",
                    JsValue::from_str(&css(if selected {
                        "rowIconSelected"
                    } else {
                        "rowIcon"
                    })),
                ),
            ])?),
            &[],
        )?;
        let button = tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "aria-current",
                    if selected {
                        JsValue::TRUE
                    } else {
                        JsValue::UNDEFINED
                    },
                ),
                (
                    "className",
                    JsValue::from_str(&classes(&[("row", true), ("rowSelected", selected)])),
                ),
                ("disabled", JsValue::from_bool(busy)),
                ("onMouseDown", on_mouse_down.unwrap_or(JsValue::UNDEFINED)),
                ("onClick", on_click.into_js_value()),
            ])?),
            &[
                icon,
                tag(
                    &modules.react,
                    "span",
                    Some(&object(&[(
                        "className",
                        JsValue::from_str(&css("rowName")),
                    )])?),
                    &[JsValue::from_str(&entry.name)],
                )?,
                element(
                    &modules.react,
                    &modules.primitive("IconChevronRightOutline14")?,
                    Some(&object(&[
                        ("size", JsValue::from_f64(12.0)),
                        ("className", JsValue::from_str(&css("rowChevron"))),
                    ])?),
                    &[],
                )?,
            ],
        )?;
        rows.push(tag(
            &modules.react,
            "span",
            Some(&object(&[
                ("key", JsValue::from_str(&entry.path)),
                ("role", JsValue::from_str("listitem")),
                ("className", JsValue::from_str(&css("rowSeat"))),
            ])?),
            &[button],
        )?);
    }
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("className", JsValue::from_str(&css("column"))),
            ("role", JsValue::from_str("list")),
        ])?),
        &rows,
    )
}

#[allow(clippy::too_many_lines)] // One source component owns the whole modal and nested create surface.
fn render_directory_browser(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let (_controller_ref, controller) = controller_ref(&modules.react)?;
    let (_revision, set_revision) = use_state(&modules.react, &JsValue::from_f64(0.0))?;
    let scan_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let path_input_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let edit_zone_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let crumb_trail_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let miller_row_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let composing_ref = use_ref(&modules.react, &JsValue::FALSE)?;
    let open = required(props, "open", "DirectoryBrowser props")?
        .as_bool()
        .unwrap_or(false);
    let list_directory = function(props, "listDirectory", "DirectoryBrowser props")?;
    let create_directory = function(props, "createDirectory", "DirectoryBrowser props")?;
    let dispose_controller = controller.clone();
    let dispose_scan = scan_ref.clone();
    let dispose_effect = Closure::wrap(Box::new(move || -> JsValue {
        let cleanup_controller = dispose_controller.clone();
        let cleanup_scan = dispose_scan.clone();
        Closure::wrap(Box::new(move || {
            let _ = abort_current(&cleanup_scan);
            let _ = dispatch(&cleanup_controller, "dispose", JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    use_effect(
        &modules.react,
        dispose_effect.into_js_value(),
        &Array::new(),
    )?;
    let effect_controller = controller.clone();
    let effect_list = list_directory.clone();
    let effect_scan = scan_ref.clone();
    let effect_setter = set_revision.clone();
    let effect = Closure::wrap(Box::new(move || -> JsValue {
        if open {
            if let Ok(launch) = dispatch(&effect_controller, "open", JsValue::UNDEFINED) {
                force(&effect_setter);
                let _ = start_landing(
                    &effect_controller,
                    &effect_list,
                    &effect_scan,
                    &effect_setter,
                    launch,
                    LandingOptions::SUBMITTED,
                );
            }
        } else {
            let _ = abort_current(&effect_scan);
            let _ = dispatch(&effect_controller, "close", JsValue::UNDEFINED);
            force(&effect_setter);
        }
        let cleanup_controller = effect_controller.clone();
        let cleanup_scan = effect_scan.clone();
        Closure::wrap(Box::new(move || {
            let _ = abort_current(&cleanup_scan);
            let _ = dispatch(&cleanup_controller, "supersede", JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    use_effect(
        &modules.react,
        effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(open), list_directory.as_ref()),
    )?;
    let state = snapshot(&controller)?;
    let focus_controller = controller.clone();
    let focus_path = path_input_ref.clone();
    let focus_edit = edit_zone_ref.clone();
    let focus_miller = miller_row_ref.clone();
    let focus = state.focus;
    let focus_effect = Closure::wrap(Box::new(move || {
        if focus == FocusRequest::None {
            return;
        }
        let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))
            .unwrap_or(JsValue::UNDEFINED);
        let body = Reflect::get(&document, &JsValue::from_str("body")).unwrap_or(JsValue::NULL);
        let active =
            Reflect::get(&document, &JsValue::from_str("activeElement")).unwrap_or(JsValue::NULL);
        match focus {
            FocusRequest::PathInput => {
                if Object::is(&active, &body) {
                    let target = current(&focus_path).unwrap_or(JsValue::NULL);
                    if !target.is_null() && !target.is_undefined() {
                        let _ = call(&target, "focus", &[]);
                    }
                }
            }
            FocusRequest::Selection => {
                let host = current(&focus_miller).unwrap_or(JsValue::NULL);
                if !host.is_null()
                    && !host.is_undefined()
                    && let Ok(row) = call(
                        &host,
                        "querySelector",
                        &[JsValue::from_str("button[aria-current=\"true\"]")],
                    )
                    && !row.is_null()
                {
                    let _ = call(&row, "focus", &[]);
                }
            }
            FocusRequest::EditZone => {
                if Object::is(&active, &body) {
                    let target = current(&focus_edit).unwrap_or(JsValue::NULL);
                    if !target.is_null() && !target.is_undefined() {
                        let _ = call(&target, "focus", &[]);
                    }
                }
            }
            FocusRequest::None => {}
        }
        let _ = dispatch(&focus_controller, "consumeFocus", JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>);
    use_effect_each_render(&modules.react, focus_effect.into_js_value())?;
    let crumb_tail = state
        .child
        .as_ref()
        .or(state.parent.as_ref())
        .and_then(|listing| listing.crumbs.last())
        .map(|crumb| crumb.path.clone());
    let scroll_crumb_ref = crumb_trail_ref.clone();
    let scroll_crumb = Closure::wrap(Box::new(move || {
        let trail = current(&scroll_crumb_ref).unwrap_or(JsValue::NULL);
        if !trail.is_null() && !trail.is_undefined() {
            let width = Reflect::get(&trail, &JsValue::from_str("scrollWidth"))
                .unwrap_or(JsValue::UNDEFINED);
            let _ = Reflect::set(&trail, &JsValue::from_str("scrollLeft"), &width);
        }
    }) as Box<dyn FnMut()>);
    use_effect(
        &modules.react,
        scroll_crumb.into_js_value(),
        &Array::of1(
            &crumb_tail
                .as_ref()
                .map_or(JsValue::UNDEFINED, |path| JsValue::from_str(path)),
        ),
    )?;
    let child_path = state.child.as_ref().map(|child| child.path.clone());
    let scroll_miller_ref = miller_row_ref.clone();
    let scroll_miller = Closure::wrap(Box::new(move || {
        if child_path.is_none() {
            return;
        }
        let row = current(&scroll_miller_ref).unwrap_or(JsValue::NULL);
        if !row.is_null() && !row.is_undefined() {
            let width =
                Reflect::get(&row, &JsValue::from_str("scrollWidth")).unwrap_or(JsValue::UNDEFINED);
            let _ = Reflect::set(&row, &JsValue::from_str("scrollLeft"), &width);
        }
    }) as Box<dyn FnMut()>);
    use_effect(
        &modules.react,
        scroll_miller.into_js_value(),
        &Array::of1(
            &state
                .child
                .as_ref()
                .map_or(JsValue::UNDEFINED, |child| JsValue::from_str(&child.path)),
        ),
    )?;
    if !open {
        return Ok(JsValue::NULL);
    }
    let translate = function(props, "t", "DirectoryBrowser props")?;
    let busy = required(props, "busy", "DirectoryBrowser props")?
        .as_bool()
        .unwrap_or(false);
    let parent_inert = busy || state.folder_draft.is_some();
    let draft_pending = state.path_draft.is_some();
    let crumb_source = state.child.as_ref().or(state.parent.as_ref());
    let typed_prefix =
        crumb_source
            .zip(state.path_draft.as_deref())
            .and_then(|(listing, draft)| {
                crate::read_draft(listing, draft, state.scanned.as_ref()).tail
            });
    let crumbs = crumb_source.map_or_else(Vec::new, |listing| {
        display_crumbs(
            listing,
            &translated(&translate, "browser.home", None)
                .ok()
                .and_then(|value| value.as_string())
                .unwrap_or_default(),
        )
    });
    let selection_controller = controller.clone();
    let selection_list = list_directory.clone();
    let selection_scan = scan_ref.clone();
    let selection_setter = set_revision.clone();
    let select = Closure::wrap(Box::new(move |entry: JsValue| -> Result<(), JsValue> {
        let launch = dispatch(&selection_controller, "beginSelection", entry)?;
        force(&selection_setter);
        start_selection_listing(
            &selection_controller,
            &selection_list,
            &selection_scan,
            &selection_setter,
            launch,
        )
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value()
    .dyn_into::<Function>()?;
    let advance_controller = controller.clone();
    let advance_list = list_directory.clone();
    let advance_scan = scan_ref.clone();
    let advance_setter = set_revision.clone();
    let advance = Closure::wrap(Box::new(move |entry: JsValue| -> Result<(), JsValue> {
        let launch = dispatch(&advance_controller, "advance", entry)?;
        if !launch.is_null() && !launch.is_undefined() {
            force(&advance_setter);
            start_selection_listing(
                &advance_controller,
                &advance_list,
                &advance_scan,
                &advance_setter,
                launch,
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value()
    .dyn_into::<Function>()?;
    let mut columns = Vec::new();
    if let Some(parent) = &state.parent {
        columns.push(render_column(
            modules,
            &parent.entries,
            state.selected.as_ref().map(|entry| entry.path.as_str()),
            parent_inert,
            state.show_hidden,
            if state.child.is_none() {
                typed_prefix.as_deref()
            } else {
                None
            },
            draft_pending,
            &select,
        )?);
    }
    if state.selected.is_some() {
        columns.push(tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str(&css("divider")),
            )])?),
            &[],
        )?);
        if let Some(child) = &state.child {
            columns.push(render_column(
                modules,
                &child.entries,
                None,
                parent_inert,
                state.show_hidden,
                typed_prefix.as_deref(),
                draft_pending,
                &advance,
            )?);
        }
    }
    let controller_for_path = controller.clone();
    let scan_for_path = scan_ref.clone();
    let path_setter = set_revision.clone();
    let open_editor = Closure::wrap(Box::new(move || {
        let _ = abort_current(&scan_for_path);
        let _ = dispatch(&controller_for_path, "openPathEditor", JsValue::UNDEFINED);
        force(&path_setter);
    }) as Box<dyn FnMut()>);
    let crumb_children = if let Some(path_draft) = &state.path_draft {
        let edit_controller = controller.clone();
        let edit_list = list_directory.clone();
        let edit_scan = scan_ref.clone();
        let change_scan = scan_ref.clone();
        let edit_setter = set_revision.clone();
        let on_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            abort_current(&change_scan)?;
            let target = required(&event, "target", "path input event")?;
            let draft = required(&target, "value", "path input")?;
            let token = dispatch(
                &edit_controller,
                "editPath",
                object(&[("draft", draft)])?.into(),
            )?;
            force(&edit_setter);
            let preview_controller = edit_controller.clone();
            let preview_list = edit_list.clone();
            let preview_scan = edit_scan.clone();
            let preview_setter = edit_setter.clone();
            let callback = Closure::wrap(Box::new(move || {
                if let Ok(launch) = dispatch(&preview_controller, "previewElapsed", token.clone())
                    && !launch.is_null()
                    && !launch.is_undefined()
                {
                    force(&preview_setter);
                    let _ = start_landing(
                        &preview_controller,
                        &preview_list,
                        &preview_scan,
                        &preview_setter,
                        launch,
                        LandingOptions::PREVIEW,
                    );
                }
            }) as Box<dyn FnMut()>);
            call(
                &js_sys::global(),
                "setTimeout",
                &[
                    callback.into_js_value(),
                    JsValue::from_f64(f64::from(DRAFT_PREVIEW_DEBOUNCE_MS)),
                ],
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let key_controller = controller.clone();
        let key_list = list_directory.clone();
        let key_scan = scan_ref.clone();
        let key_setter = set_revision.clone();
        let key_composing = composing_ref.clone();
        let on_key = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if required(&event, "key", "path keyboard event")?
                .as_string()
                .as_deref()
                == Some("Enter")
                && current(&key_composing)?.as_bool() != Some(true)
            {
                call(&event, "preventDefault", &[])?;
                let launch = dispatch(&key_controller, "submitPath", JsValue::UNDEFINED)?;
                if !launch.is_null() && !launch.is_undefined() {
                    force(&key_setter);
                    start_landing(
                        &key_controller,
                        &key_list,
                        &key_scan,
                        &key_setter,
                        launch,
                        LandingOptions::SUBMITTED,
                    )?;
                }
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let start_composing = composing_ref.clone();
        let on_composition_start = Closure::wrap(Box::new(move || {
            let _ = set_current(&start_composing, &JsValue::TRUE);
        }) as Box<dyn FnMut()>);
        let end_composing = composing_ref.clone();
        let on_composition_end = Closure::wrap(Box::new(move || {
            let _ = set_current(&end_composing, &JsValue::FALSE);
        }) as Box<dyn FnMut()>);
        vec![tag(
            &modules.react,
            "input",
            Some(&object(&[
                ("className", JsValue::from_str(&css("pathInput"))),
                ("value", JsValue::from_str(path_draft)),
                (
                    "aria-label",
                    translated(&translate, "browser.editPath", None)?,
                ),
                ("autoFocus", JsValue::TRUE),
                ("ref", path_input_ref.clone()),
                ("disabled", JsValue::from_bool(parent_inert)),
                ("onChange", on_change.into_js_value()),
                ("onCompositionStart", on_composition_start.into_js_value()),
                ("onCompositionEnd", on_composition_end.into_js_value()),
                ("onKeyDown", on_key.into_js_value()),
            ])?),
            &[],
        )?]
    } else {
        let mut children = Vec::new();
        let mut trail = Vec::new();
        for (index, crumb) in crumbs.iter().enumerate() {
            let crumb_controller = controller.clone();
            let crumb_list = list_directory.clone();
            let crumb_scan = scan_ref.clone();
            let crumb_setter = set_revision.clone();
            let path = crumb.path.clone();
            let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                let launch = dispatch(
                    &crumb_controller,
                    "beginLanding",
                    object(&[
                        ("path", JsValue::from_str(&path)),
                        ("options", encode(&LandingOptions::SUBMITTED)?),
                    ])?
                    .into(),
                )?;
                force(&crumb_setter);
                start_landing(
                    &crumb_controller,
                    &crumb_list,
                    &crumb_scan,
                    &crumb_setter,
                    launch,
                    LandingOptions::SUBMITTED,
                )
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            let mut seat = Vec::new();
            if index > 0 {
                seat.push(element(
                    &modules.react,
                    &modules.primitive("IconChevronRightOutline14")?,
                    Some(&object(&[
                        ("size", JsValue::from_f64(12.0)),
                        ("className", JsValue::from_str(&css("crumbChevron"))),
                    ])?),
                    &[],
                )?);
            }
            seat.push(tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    ("className", JsValue::from_str(&css("crumb"))),
                    ("disabled", JsValue::from_bool(parent_inert)),
                    ("onClick", on_click.into_js_value()),
                ])?),
                &[JsValue::from_str(&crumb.name)],
            )?);
            trail.push(tag(
                &modules.react,
                "span",
                Some(&object(&[
                    ("key", JsValue::from_str(&crumb.path)),
                    ("className", JsValue::from_str(&css("crumbSeat"))),
                ])?),
                &seat,
            )?);
        }
        children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[
                ("className", JsValue::from_str(&css("crumbTrail"))),
                ("role", JsValue::from_str("navigation")),
                ("ref", crumb_trail_ref.clone()),
            ])?),
            &trail,
        )?);
        children.push(tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("className", JsValue::from_str(&css("crumbEditZone"))),
                (
                    "aria-label",
                    translated(&translate, "browser.editPath", None)?,
                ),
                ("title", translated(&translate, "browser.editPath", None)?),
                ("disabled", JsValue::from_bool(parent_inert)),
                ("ref", edit_zone_ref.clone()),
                ("onClick", open_editor.into_js_value()),
            ])?),
            &[element(
                &modules.react,
                &modules.primitive("IconEditOutline16")?,
                Some(&object(&[
                    ("size", JsValue::from_f64(14.0)),
                    ("className", JsValue::from_str(&css("crumbEditGlyph"))),
                ])?),
                &[],
            )?],
        )?);
        children
    };
    let error_node = match &state.error {
        Some(error) => tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("className", JsValue::from_str(&css("error"))),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[JsValue::from_str(error)],
        )?,
        None => JsValue::NULL,
    };
    let content = tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&css("content")),
        )])?),
        &[
            tag(
                &modules.react,
                "div",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("millerRow"))),
                    ("ref", miller_row_ref.clone()),
                ])?),
                &columns,
            )?,
            if state.loading && state.slow_scan {
                tag(
                    &modules.react,
                    "div",
                    Some(&object(&[
                        (
                            "className",
                            JsValue::from_str(&classes(&[
                                ("status", true),
                                ("loadingFloat", true),
                            ])),
                        ),
                        ("role", JsValue::from_str("status")),
                    ])?),
                    &[translated(&translate, "browser.loading", None)?],
                )?
            } else {
                JsValue::NULL
            },
            if state.parent.as_ref().is_some_and(|level| level.truncated)
                || state.child.as_ref().is_some_and(|level| level.truncated)
            {
                tag(
                    &modules.react,
                    "div",
                    Some(&object(&[
                        ("className", JsValue::from_str(&css("status"))),
                        ("role", JsValue::from_str("status")),
                    ])?),
                    &[translated(&translate, "browser.truncated", None)?],
                )?
            } else {
                JsValue::NULL
            },
            error_node,
        ],
    )?;
    let target = target_path(state.parent.as_ref(), state.selected.as_ref()).map(str::to_owned);
    let new_folder_controller = controller.clone();
    let new_folder_setter = set_revision.clone();
    let open_create = Closure::wrap(Box::new(move || {
        if accepted(dispatch(
            &new_folder_controller,
            "openCreateDialog",
            JsValue::UNDEFINED,
        )) {
            force(&new_folder_setter);
        }
    }) as Box<dyn FnMut()>);
    let hidden_controller = controller.clone();
    let hidden_setter = set_revision.clone();
    let toggle_hidden = Closure::wrap(Box::new(move || {
        let _ = dispatch(&hidden_controller, "toggleShowHidden", JsValue::UNDEFINED);
        force(&hidden_setter);
    }) as Box<dyn FnMut()>);
    let hidden_mouse_down = draft_pending.then(|| {
        Closure::wrap(Box::new(move |event: JsValue| {
            let _ = call(&event, "preventDefault", &[]);
        }) as Box<dyn FnMut(JsValue)>)
        .into_js_value()
    });
    let on_close = function(props, "onClose", "DirectoryBrowser props")?;
    let owner_close = on_close.clone();
    let outer_close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if !parent_inert {
            owner_close.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let cancel = on_close;
    let cancel_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        cancel.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let on_open = function(props, "onOpen", "DirectoryBrowser props")?;
    let target_for_open = target.clone();
    let open_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if let Some(target) = &target_for_open {
            on_open.call1(&JsValue::UNDEFINED, &JsValue::from_str(target))?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let footer = tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&css("footerBar")),
        )])?),
        &[
            element(
                &modules.react,
                &modules.primitive("Button")?,
                Some(&object(&[
                    ("variant", JsValue::from_str("outline")),
                    (
                        "icon",
                        element(
                            &modules.react,
                            &modules.primitive("IconPlusOutline16")?,
                            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
                            &[],
                        )?,
                    ),
                    (
                        "disabled",
                        JsValue::from_bool(
                            state.parent.is_none()
                                || state.loading
                                || parent_inert
                                || draft_pending,
                        ),
                    ),
                    ("onClick", open_create.into_js_value()),
                ])?),
                &[translated(&translate, "browser.newFolder", None)?],
            )?,
            tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str(&classes(&[
                            ("showHiddenToggle", true),
                            ("showHiddenToggleActive", state.show_hidden),
                        ])),
                    ),
                    ("aria-pressed", JsValue::from_bool(state.show_hidden)),
                    ("disabled", JsValue::from_bool(parent_inert)),
                    (
                        "onMouseDown",
                        hidden_mouse_down.unwrap_or(JsValue::UNDEFINED),
                    ),
                    ("onClick", toggle_hidden.into_js_value()),
                ])?),
                &[
                    translated(&translate, "browser.showHidden", None)?,
                    if state.show_hidden {
                        element(
                            &modules.react,
                            &modules.primitive("IconCheckOutline16")?,
                            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
                            &[],
                        )?
                    } else {
                        JsValue::NULL
                    },
                ],
            )?,
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&css("footerGap")),
                )])?),
                &[],
            )?,
            element(
                &modules.react,
                &modules.primitive("Button")?,
                Some(&object(&[
                    ("variant", JsValue::from_str("outline")),
                    ("className", JsValue::from_str(&css("footerAction"))),
                    ("disabled", JsValue::from_bool(parent_inert)),
                    ("onClick", cancel_click.into_js_value()),
                ])?),
                &[translated(&translate, "browser.cancel", None)?],
            )?,
            element(
                &modules.react,
                &modules.primitive("Button")?,
                Some(&object(&[
                    ("variant", JsValue::from_str("primary")),
                    ("className", JsValue::from_str(&css("footerAction"))),
                    (
                        "disabled",
                        JsValue::from_bool(
                            target.is_none() || state.loading || parent_inert || draft_pending,
                        ),
                    ),
                    ("onClick", open_click.into_js_value()),
                ])?),
                &[translated(&translate, "browser.open", None)?],
            )?,
        ],
    )?;
    let header = tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&css("header")),
        )])?),
        &[
            tag(
                &modules.react,
                "h2",
                Some(&object(&[("className", JsValue::from_str(&css("title")))])?),
                &[translated(&translate, "browser.title", None)?],
            )?,
            tag(
                &modules.react,
                "div",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&css("crumbBar")),
                )])?),
                &crumb_children,
            )?,
        ],
    )?;
    let target_name = target_name(
        state.parent.as_ref(),
        state.selected.as_ref(),
        &translated(&translate, "browser.home", None)?
            .as_string()
            .unwrap_or_default(),
    );
    let create_controller = controller.clone();
    let create_setter = set_revision.clone();
    let folder_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let target = required(&event, "target", "folder input event")?;
        dispatch(
            &create_controller,
            "editFolderName",
            object(&[("draft", required(&target, "value", "folder input")?)])?.into(),
        )?;
        force(&create_setter);
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let confirm_controller = controller.clone();
    let confirm_create = create_directory.clone();
    let confirm_list = list_directory.clone();
    let confirm_scan = scan_ref.clone();
    let confirm_setter = set_revision.clone();
    let confirm = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let launch = dispatch(&confirm_controller, "confirmCreate", JsValue::UNDEFINED)?;
        if !launch.is_null() && !launch.is_undefined() {
            force(&confirm_setter);
            start_creation(
                &confirm_controller,
                &confirm_create,
                &confirm_list,
                &confirm_scan,
                &confirm_setter,
                launch,
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let close_create_controller = controller.clone();
    let close_create_setter = set_revision.clone();
    let close_create = Closure::wrap(Box::new(move || {
        if accepted(dispatch(
            &close_create_controller,
            "closeCreateDialog",
            JsValue::UNDEFINED,
        )) {
            force(&close_create_setter);
        }
    }) as Box<dyn FnMut()>)
    .into_js_value();
    let folder_confirm = confirm.clone().dyn_into::<Function>()?;
    let folder_close = close_create.clone().dyn_into::<Function>()?;
    let folder_composing = composing_ref.clone();
    let folder_creating = state.creating_folder;
    let folder_key = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = required(&event, "key", "folder keyboard event")?
            .as_string()
            .unwrap_or_default();
        if key == "Enter" && current(&folder_composing)?.as_bool() != Some(true) {
            call(&event, "preventDefault", &[])?;
            folder_confirm.call0(&JsValue::UNDEFINED)?;
        }
        if key == "Escape" {
            call(&event, "stopPropagation", &[])?;
            if !folder_creating {
                folder_close.call0(&JsValue::UNDEFINED)?;
            }
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let folder_start_ref = composing_ref.clone();
    let folder_composition_start = Closure::wrap(Box::new(move || {
        let _ = set_current(&folder_start_ref, &JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let folder_end_ref = composing_ref.clone();
    let folder_composition_end = Closure::wrap(Box::new(move || {
        let _ = set_current(&folder_end_ref, &JsValue::FALSE);
    }) as Box<dyn FnMut()>);
    let create_error_node = match &state.create_error {
        Some(error) => tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("className", JsValue::from_str(&css("error"))),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[JsValue::from_str(error)],
        )?,
        None => JsValue::NULL,
    };
    let create_dialog = element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(state.folder_draft.is_some())),
            ("onClose", close_create.clone()),
            ("title", translated(&translate, "browser.newFolder", None)?),
            ("className", JsValue::from_str(&css("createDialog"))),
            ("headless", JsValue::TRUE),
        ])?),
        &[tag(
            &modules.react,
            "div",
            Some(&object(&[(
                "className",
                JsValue::from_str(&css("createBody")),
            )])?),
            &[
                tag(
                    &modules.react,
                    "h3",
                    Some(&object(&[(
                        "className",
                        JsValue::from_str(&css("createTitle")),
                    )])?),
                    &[translated(&translate, "browser.newFolder", None)?],
                )?,
                tag(
                    &modules.react,
                    "p",
                    Some(&object(&[(
                        "className",
                        JsValue::from_str(&css("createIn")),
                    )])?),
                    &[translated(
                        &translate,
                        "browser.createIn",
                        Some(&object(&[("name", JsValue::from_str(&target_name))])?),
                    )?],
                )?,
                tag(
                    &modules.react,
                    "input",
                    Some(&object(&[
                        ("className", JsValue::from_str(&css("createInput"))),
                        (
                            "value",
                            JsValue::from_str(state.folder_draft.as_deref().unwrap_or_default()),
                        ),
                        (
                            "aria-label",
                            translated(&translate, "browser.folderName", None)?,
                        ),
                        (
                            "placeholder",
                            translated(&translate, "browser.untitledFolder", None)?,
                        ),
                        ("autoFocus", JsValue::TRUE),
                        ("disabled", JsValue::from_bool(state.creating_folder)),
                        ("onChange", folder_change.into_js_value()),
                        (
                            "onCompositionStart",
                            folder_composition_start.into_js_value(),
                        ),
                        ("onCompositionEnd", folder_composition_end.into_js_value()),
                        ("onKeyDown", folder_key.into_js_value()),
                    ])?),
                    &[],
                )?,
                create_error_node,
                tag(
                    &modules.react,
                    "div",
                    Some(&object(&[(
                        "className",
                        JsValue::from_str(&css("createActions")),
                    )])?),
                    &[
                        element(
                            &modules.react,
                            &modules.primitive("Button")?,
                            Some(&object(&[
                                ("variant", JsValue::from_str("outline")),
                                ("disabled", JsValue::from_bool(state.creating_folder)),
                                ("onClick", close_create),
                            ])?),
                            &[translated(&translate, "browser.cancel", None)?],
                        )?,
                        element(
                            &modules.react,
                            &modules.primitive("Button")?,
                            Some(&object(&[
                                ("variant", JsValue::from_str("primary")),
                                (
                                    "disabled",
                                    JsValue::from_bool(
                                        state.creating_folder
                                            || state
                                                .folder_draft
                                                .as_deref()
                                                .is_none_or(|draft| draft.trim().is_empty()),
                                    ),
                                ),
                                ("onClick", confirm),
                            ])?),
                            &[translated(&translate, "browser.create", None)?],
                        )?,
                    ],
                )?,
            ],
        )?],
    )?;
    let scope_path_editing = state.path_draft.is_some();
    let scope_key_controller = controller.clone();
    let scope_key_list = list_directory.clone();
    let scope_key_scan = scan_ref.clone();
    let scope_key_abort = scan_ref.clone();
    let scope_key_setter = set_revision.clone();
    let scope_key_input = path_input_ref.clone();
    let scope_key = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if !scope_path_editing
            || required(&event, "key", "editor keyboard event")?
                .as_string()
                .as_deref()
                != Some("Escape")
        {
            return Ok(());
        }
        call(&event, "stopPropagation", &[])?;
        abort_current(&scope_key_abort)?;
        let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
        let active = Reflect::get(&document, &JsValue::from_str("activeElement"))?;
        let input = current(&scope_key_input)?;
        let launch = dispatch(
            &scope_key_controller,
            "cancelPathEdit",
            object(&[("focus", JsValue::from_bool(Object::is(&active, &input)))])?.into(),
        )?;
        force(&scope_key_setter);
        if !launch.is_null() && !launch.is_undefined() {
            start_landing(
                &scope_key_controller,
                &scope_key_list,
                &scope_key_scan,
                &scope_key_setter,
                launch,
                LandingOptions::SUBMITTED,
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let scope_blur_controller = controller.clone();
    let scope_blur_list = list_directory.clone();
    let scope_blur_scan = scan_ref.clone();
    let scope_blur_abort = scan_ref.clone();
    let scope_blur_setter = set_revision.clone();
    let scope_blur = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if !scope_path_editing {
            return Ok(());
        }
        let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
        if call(&document, "hasFocus", &[])?.as_bool() != Some(true) {
            return Ok(());
        }
        let current_target = required(&event, "currentTarget", "editor blur event")?;
        let card = call(
            &current_target,
            "closest",
            &[JsValue::from_str("[role=\"dialog\"]")],
        )?;
        if card.is_null() {
            return Ok(());
        }
        let related = Reflect::get(&event, &JsValue::from_str("relatedTarget"))?;
        if related.is_object() && call(&card, "contains", &[related])?.as_bool() == Some(true) {
            return Ok(());
        }
        abort_current(&scope_blur_abort)?;
        let launch = dispatch(
            &scope_blur_controller,
            "cancelPathEdit",
            object(&[("focus", JsValue::FALSE)])?.into(),
        )?;
        force(&scope_blur_setter);
        if !launch.is_null() && !launch.is_undefined() {
            start_landing(
                &scope_blur_controller,
                &scope_blur_list,
                &scope_blur_scan,
                &scope_blur_setter,
                launch,
                LandingOptions::SUBMITTED,
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let outer = element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::TRUE),
            ("onClose", outer_close.into_js_value()),
            ("title", translated(&translate, "browser.title", None)?),
            ("className", JsValue::from_str(&css("dialog"))),
            ("headless", JsValue::TRUE),
        ])?),
        &[tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("className", JsValue::from_str(&css("editorScope"))),
                ("onKeyDown", scope_key.into_js_value()),
                ("onBlur", scope_blur.into_js_value()),
            ])?),
            &[header, content, footer],
        )?],
    )?;
    element(
        &modules.react,
        &required(&modules.react, "Fragment", "React")?,
        None,
        &[outer, create_dialog],
    )
}
