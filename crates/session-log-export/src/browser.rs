//! Compiled browser controller face, React surfaces, and Client plugin assembly.

use std::{cell::RefCell, rc::Rc};

use futures::FutureExt as _;
use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_identity::SessionId;
use serde::Serialize as _;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};
use web_sys::{AbortController, HtmlAnchorElement, Response};

use crate::{
    DownloadFetcher, DownloadRequest, DownloadResponse, DownloadSaver,
    SessionLogDownloadController, SessionLogDownloadState,
    locales::{EN, NS, ZH},
};

const PACKAGE_ID: &str = "@seekdeep-ai/seekdeep-session-log-export";
const INJECT: &[&str] = &["slots", "locale"];
const HEADER_CSS: &str = include_str!(
    "../../../packages/session-query/session-log-export/src/client/HeaderAction.module.css"
);

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
    static COMPONENTS: RefCell<Option<Components>> = const { RefCell::new(None) };
    static CONTROLLER_CONSTRUCTOR: RefCell<Option<Function>> = const { RefCell::new(None) };
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

#[derive(Clone)]
struct Components {
    dialog: JsValue,
    header: JsValue,
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

fn component(
    modules: &BrowserModules,
    renderer: fn(&BrowserModules, &JsValue) -> Result<JsValue, JsValue>,
) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(Box::new(move |props: JsValue| renderer(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

fn rejection_text(reason: &JsValue) -> String {
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

fn prefix_css(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(source.len() + 128);
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
            output.extend_from_slice(b".seekdeep-session-log-");
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

fn css(name: &str) -> String {
    format!("seekdeep-session-log-{name}")
}

fn inject_style() -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let identity = format!("{PACKAGE_ID}/HeaderAction.module.css");
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
        &JsValue::from_str(&prefix_css(HEADER_CSS)),
    )?;
    call(
        &required(&document, "head", "document")?,
        "appendChild",
        &[style],
    )?;
    Ok(())
}

fn configured_components() -> Result<Components, JsValue> {
    COMPONENTS.with(|components| {
        components
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("session-log-export is not configured").into())
    })
}

/// Configures page-owned React, primitives, and the Header action stylesheet.
///
/// # Errors
///
/// Returns missing dependency or stylesheet-injection failures.
#[wasm_bindgen(js_name = configureSessionLogExport)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_session_log_export(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    function(&react, "createElement", "React")?;
    required(&react, "Fragment", "React")?;
    for primitive in ["Button", "IconDownloadOutline16", "Modal"] {
        required(&primitives, primitive, "UI primitives")?;
    }
    inject_style()?;
    let modules = BrowserModules { react, primitives };
    let dialog = component(&modules, render_dialog);
    COMPONENTS.with(|components| {
        *components.borrow_mut() = Some(Components {
            dialog,
            header: component(&modules, render_header),
        });
    });
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules));
    Ok(())
}

/// Configures the public Controller constructor used by Client apply.
#[wasm_bindgen(js_name = configureSessionLogExportApply)]
pub fn configure_session_log_export_apply(constructor: Function) {
    CONTROLLER_CONSTRUCTOR.with(|configured| *configured.borrow_mut() = Some(constructor));
}

/// Returns the compiled `SessionLogDownloadDialog` component.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = sessionLogDownloadDialogComponent)]
pub fn session_log_download_dialog_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.dialog)
}

/// Returns the compiled `SessionLogDownloadHeaderAction` component.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = sessionLogDownloadHeaderActionComponent)]
pub fn session_log_download_header_action_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.header)
}

fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn selected_entry(props: &JsValue) -> Result<(String, JsValue), JsValue> {
    let session_id = required(props, "sessionId", "Session log props")?
        .as_string()
        .unwrap_or_default();
    let selected_id = session_id.clone();
    let selector = Closure::wrap(Box::new(move |state: JsValue| -> Result<JsValue, JsValue> {
        let by_session = required(&state, "bySession", "Session log state")?;
        Reflect::get(&by_session, &JsValue::from_str(&selected_id))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let entry = function(props, "useSessionLogDownload", "Session log props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())?;
    Ok((session_id, entry))
}

fn dismiss_callback(props: &JsValue, session_id: &str) -> Result<JsValue, JsValue> {
    let dismiss = function(props, "dismiss", "Session log props")?;
    let session_id = session_id.to_owned();
    Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        dismiss.call1(&JsValue::UNDEFINED, &JsValue::from_str(&session_id))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value())
}

fn render_dialog(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let (session_id, entry) = selected_entry(props)?;
    let status = optional(&entry, "status")?.and_then(|status| status.as_string());
    let open = optional(&entry, "open")?.and_then(|open| open.as_bool()) == Some(true);
    let translate = function(props, "t", "Session log props")?;
    let error = if status.as_deref() == Some("error") {
        match optional(&entry, "error")?
            .and_then(|error| error.as_string())
            .filter(|error| !error.is_empty())
        {
            Some(error) => JsValue::from_str(&error),
            None => translated(&translate, "dialog.commandFailed")?,
        }
    } else {
        JsValue::NULL
    };
    let (title_key, description) = match status.as_deref() {
        Some("downloading") => (
            "dialog.preparingTitle",
            translated(&translate, "dialog.preparingDescription")?,
        ),
        Some("success") => (
            "dialog.successTitle",
            translated(&translate, "dialog.successDescription")?,
        ),
        _ => (
            "dialog.errorTitle",
            if error.is_null() {
                translated(&translate, "dialog.commandFailed")?
            } else {
                error
            },
        ),
    };
    let on_close = dismiss_callback(props, &session_id)?;
    let footer = element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&[
            ("variant", JsValue::from_str("primary")),
            ("onClick", on_close.clone()),
        ])?),
        &[translated(&translate, "dialog.close")?],
    )?;
    element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("onClose", on_close),
            ("title", translated(&translate, title_key)?),
            ("description", description),
            ("closeLabel", translated(&translate, "dialog.close")?),
            ("footer", footer),
        ])?),
        &[],
    )
}

fn render_header(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let (session_id, entry) = selected_entry(props)?;
    let busy = optional(&entry, "status")?
        .and_then(|status| status.as_string())
        .as_deref()
        == Some("downloading");
    let request = function(props, "request", "Session log props")?;
    let request_id = session_id.clone();
    let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        request.call1(&JsValue::UNDEFINED, &JsValue::from_str(&request_id))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let button = tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&css("sessionLogButton"))),
            ("disabled", JsValue::from_bool(busy)),
            ("aria-busy", JsValue::from_bool(busy)),
            ("onClick", on_click.into_js_value()),
        ])?),
        &[
            tag(
                &modules.react,
                "span",
                None,
                &[JsValue::from_str("Session log")],
            )?,
            element(
                &modules.react,
                &modules.primitive("IconDownloadOutline16")?,
                Some(&object(&[("size", JsValue::from_f64(12.0))])?),
                &[],
            )?,
        ],
    )?;
    let dialog = element(
        &modules.react,
        &configured_components()?.dialog,
        Some(&props.clone().dyn_into::<Object>()?),
        &[],
    )?;
    element(
        &modules.react,
        &required(&modules.react, "Fragment", "React")?,
        None,
        &[button, dialog],
    )
}

fn notify_store(controller: &Rc<SessionLogDownloadController>) -> Result<JsValue, JsValue> {
    let snapshot_controller = Rc::downgrade(controller);
    let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let Some(controller) = snapshot_controller.upgrade() else {
            return SessionLogDownloadState::default()
                .serialize(&serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true))
                .map_err(|error| js_sys::Error::new(&error.to_string()).into());
        };
        controller
            .state()
            .serialize(&serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true))
            .map_err(|error| js_sys::Error::new(&error.to_string()).into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let subscribe_controller = Rc::downgrade(controller);
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> JsValue {
        let Some(controller) = subscribe_controller.upgrade() else {
            return Closure::wrap(Box::new(move || {}) as Box<dyn FnMut()>).into_js_value();
        };
        let listener = Rc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        });
        let subscription = Rc::new(controller.subscribe(listener));
        Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>).into_js_value()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    let set_controller = Rc::downgrade(controller);
    let set = Closure::wrap(Box::new(move |state: JsValue| -> Result<(), JsValue> {
        let Some(controller) = set_controller.upgrade() else {
            return Ok(());
        };
        let state = serde_wasm_bindgen::from_value::<SessionLogDownloadState>(state)
            .map_err(|error| js_sys::TypeError::new(&error.to_string()))?;
        controller.set_state(state);
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    Ok(object(&[
        ("getSnapshot", get_snapshot.into_js_value()),
        ("subscribe", subscribe.into_js_value()),
        ("set", set.into_js_value()),
    ])?
    .into())
}

#[allow(clippy::too_many_lines)]
fn browser_operations(
    fetcher: Option<Function>,
    saver: Option<Function>,
) -> Result<Rc<SessionLogDownloadController>, JsValue> {
    let window = web_sys::window();
    let origin = Reflect::get(&js_sys::global(), &JsValue::from_str("location"))
        .ok()
        .and_then(|location| Reflect::get(&location, &JsValue::from_str("origin")).ok())
        .and_then(|origin| origin.as_string())
        .or_else(|| {
            window
                .as_ref()
                .and_then(|window| window.location().origin().ok())
        })
        .unwrap_or_else(|| "null".to_owned());
    if fetcher.is_none() && window.is_none() {
        return Err(JsValue::from_str("window unavailable"));
    }
    if saver.is_none() && window.is_none() {
        return Err(JsValue::from_str("window unavailable"));
    }
    let fetch_window = window.clone();
    let injected_fetcher = fetcher;
    let fetcher: DownloadFetcher = Rc::new(move |request: DownloadRequest| {
        let window = fetch_window.clone();
        let injected = injected_fetcher.clone();
        async move {
            let web_abort = AbortController::new().map_err(|error| rejection_text(&error))?;
            let bridge = web_abort.clone();
            let signal = request.signal.clone();
            let bridge_done = crate::DownloadAbortSignal::default();
            let wait_done = bridge_done.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let cancelled = signal.cancelled().fuse();
                let completed = wait_done.cancelled().fuse();
                futures::pin_mut!(cancelled, completed);
                futures::select_biased! {
                    () = cancelled => bridge.abort(),
                    () = completed => {},
                }
            });
            let result = async {
                let url_constructor =
                    required(&js_sys::global(), "URL", "globalThis")?.dyn_into::<Function>()?;
                let url = Reflect::construct(
                    &url_constructor,
                    &Array::of1(&JsValue::from_str(&request.url)),
                )?;
                let init = object(&[
                    ("method", JsValue::from_str("HEAD")),
                    ("signal", web_abort.signal().into()),
                ])?;
                let response = if let Some(fetcher) = injected {
                    fetcher.call2(&JsValue::UNDEFINED, &url, &init)?
                } else {
                    let window = window
                        .as_ref()
                        .ok_or_else(|| JsValue::from_str("window unavailable"))?;
                    function(window.as_ref(), "fetch", "window")?.call2(
                        window.as_ref(),
                        &url,
                        &init,
                    )?
                };
                let response = JsFuture::from(Promise::resolve(&response))
                    .await?
                    .dyn_into::<Response>()?;
                let detail = match response.text() {
                    Ok(text) => JsFuture::from(text)
                        .await
                        .map_err(|error| rejection_text(&error))
                        .and_then(|value| value.as_string().ok_or_else(String::new)),
                    Err(error) => Err(rejection_text(&error)),
                };
                Ok::<_, JsValue>(DownloadResponse {
                    status: response.status(),
                    detail,
                })
            }
            .await
            .map_err(|error| rejection_text(&error));
            bridge_done.abort();
            result
        }
        .boxed_local()
    });
    let save_window = window;
    let save: DownloadSaver = Rc::new(move |url, filename| {
        if let Some(saver) = &saver {
            saver
                .call2(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(url),
                    &JsValue::from_str(filename),
                )
                .map(|_| ())
                .map_err(|error| rejection_text(&error))
        } else {
            let anchor = save_window
                .as_ref()
                .ok_or_else(|| "window unavailable".to_owned())?
                .document()
                .and_then(|document| document.create_element("a").ok())
                .and_then(|anchor| anchor.dyn_into::<HtmlAnchorElement>().ok())
                .ok_or_else(|| "download anchor unavailable".to_owned())?;
            anchor.set_href(url);
            anchor.set_download(filename);
            anchor.click();
            Ok(())
        }
    });
    Ok(SessionLogDownloadController::new(
        fetcher,
        save,
        Some(&origin),
    ))
}

fn controller_face(fetcher: Option<Function>, saver: Option<Function>) -> Result<JsValue, JsValue> {
    let controller = browser_operations(fetcher, saver)?;
    let store = notify_store(&controller)?;
    let download_controller = controller.clone();
    let download = Closure::wrap(Box::new(move |session_id: String| -> Promise {
        let done = download_controller.download(SessionId::new(session_id));
        future_to_promise(async move {
            done.await;
            Ok(JsValue::UNDEFINED)
        })
    }) as Box<dyn FnMut(String) -> Promise>);
    let dismiss_controller = controller.clone();
    let dismiss = Closure::wrap(Box::new(move |session_id: String| {
        dismiss_controller.dismiss(&SessionId::new(session_id));
    }) as Box<dyn FnMut(String)>);
    let dispose_controller = controller;
    let dispose = Closure::wrap(Box::new(move || -> Promise {
        let controller = dispose_controller.clone();
        future_to_promise(async move {
            controller.dispose().await;
            Ok(JsValue::UNDEFINED)
        })
    }) as Box<dyn FnMut() -> Promise>);
    Ok(object(&[
        ("store", store),
        ("download", download.into_js_value()),
        ("dismiss", dismiss.into_js_value()),
        ("dispose", dispose.into_js_value()),
    ])?
    .into())
}

/// Creates one browser download controller face.
///
/// # Errors
///
/// Returns missing browser globals or JavaScript operation failures.
#[wasm_bindgen(js_name = createSessionLogDownloadController)]
pub fn create_session_log_download_controller(
    fetcher: Option<Function>,
    saver: Option<Function>,
) -> Result<JsValue, JsValue> {
    controller_face(fetcher, saver)
}

/// Returns the renamed, sanitized browser filename for one Session.
#[wasm_bindgen(js_name = sessionLogZipFilename)]
pub fn session_log_zip_filename_browser(session_id: String) -> String {
    crate::session_log_zip_filename(&SessionId::new(session_id))
}

fn locale_dictionaries() -> Result<JsValue, JsValue> {
    let en = Object::new();
    let zh = Object::new();
    for (key, value) in EN {
        Reflect::set(&en, &JsValue::from_str(key), &JsValue::from_str(value))?;
    }
    for (key, value) in ZH {
        Reflect::set(&zh, &JsValue::from_str(key), &JsValue::from_str(value))?;
    }
    Ok(object(&[("en", en.into()), ("zh", zh.into())])?.into())
}

fn configured_constructor() -> Result<Function, JsValue> {
    CONTROLLER_CONSTRUCTOR.with(|configured| {
        configured
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("session-log-export apply was not configured").into())
    })
}

/// Applies the shared controller, command event listener, locale, and Header entry.
///
/// # Errors
///
/// Returns missing context, service, constructor, or Slot failures.
#[wasm_bindgen(js_name = applySessionLogExport)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn apply_session_log_export(ctx: JsValue) -> Result<(), JsValue> {
    let slots = required(&ctx, "slots", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    let controller = Reflect::construct(&configured_constructor()?, &Array::new())?;
    call(
        &ctx,
        "provide",
        &[JsValue::from_str("sessionLogDownload"), controller.clone()],
    )?;
    let lifecycle_controller = controller.clone();
    let lifecycle = Closure::wrap(Box::new(move || -> JsValue {
        let cleanup_controller = lifecycle_controller.clone();
        Closure::wrap(Box::new(move || call(&cleanup_controller, "dispose", &[]))
            as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    call(
        &ctx,
        "effect",
        &[
            lifecycle.into_js_value(),
            JsValue::from_str("session-log-download: browser download lifecycle"),
        ],
    )?;
    let dictionaries = locale_dictionaries()?;
    let locale = locale.clone();
    let install_locale = Closure::wrap(Box::new(move || {
        call(
            &locale,
            "register",
            &[JsValue::from_str(NS), dictionaries.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call(
        &ctx,
        "effect",
        &[
            install_locale.into_js_value(),
            JsValue::from_str("session-log-download: browser dictionaries"),
        ],
    )?;
    let event_controller = controller.clone();
    let executed = Closure::wrap(Box::new(
        move |session_id: String, command_name: String, result: JsValue| {
            if command_name == "export"
                && Reflect::get(&result, &JsValue::from_str("kind"))
                    .ok()
                    .and_then(|kind| kind.as_string())
                    .as_deref()
                    == Some("success")
            {
                let _ = call(
                    &event_controller,
                    "download",
                    &[JsValue::from_str(&session_id)],
                );
            }
        },
    ) as Box<dyn FnMut(String, String, JsValue)>);
    call(
        &ctx,
        "on",
        &[
            JsValue::from_str("command/executed"),
            executed.into_js_value(),
        ],
    )?;
    let registration_slots = slots.clone();
    let registration_controller = controller;
    let install = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let hooks = object(&[(
            "sessionLogDownload",
            required(&registration_controller, "store", "download controller")?,
        )])?;
        let request_controller = registration_controller.clone();
        let request = Closure::wrap(Box::new(move |session_id: JsValue| {
            call(&request_controller, "download", &[session_id])
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        let dismiss_controller = registration_controller.clone();
        let dismiss = Closure::wrap(Box::new(move |session_id: JsValue| {
            call(&dismiss_controller, "dismiss", &[session_id]).map(|_| ())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let face: JsValue = object(&[
            ("hooks", hooks.into()),
            ("request", request.into_js_value()),
            ("dismiss", dismiss.into_js_value()),
        ])?
        .into();
        let inject = Closure::wrap(Box::new(move || face.clone()) as Box<dyn FnMut() -> JsValue>);
        call(
            &registration_slots,
            "register",
            &[
                object(&[
                    (
                        "name",
                        JsValue::from_str("conversation.session.header.utilities"),
                    ),
                    ("id", JsValue::from_str("session-log-download")),
                    ("locale", JsValue::from_str(NS)),
                    ("inject", inject.into_js_value()),
                ])?
                .into(),
                session_log_download_header_action_component()?,
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.session.header.utilities"),
            install.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Exact browser service dependencies.
#[wasm_bindgen(js_name = sessionLogExportInject)]
pub fn session_log_export_inject() -> Array {
    INJECT
        .iter()
        .map(|value| JsValue::from_str(value))
        .collect()
}
