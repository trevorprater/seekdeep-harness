//! Browser utilities shared by compiled UI primitives.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

/// Pointer transit grace shared by hover-dismissed popups.
pub const POINTER_GRACE_MS: u32 = 200;
/// Successful-copy feedback lifetime.
pub const COPIED_FEEDBACK_MS: u32 = 1_000;
/// Viewport-top margin for bottom-anchored overlays.
pub const ANCHORED_MARGIN_PX: f64 = 12.0;

thread_local! {
    static REACT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// Installs the React hook module used by compiled primitive hooks.
#[wasm_bindgen(js_name = configureClientUiPrimitiveHooks)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_hooks(react: JsValue) {
    REACT.with(|configured| *configured.borrow_mut() = Some(react));
}

/// JavaScript-facing pointer grace duration.
#[wasm_bindgen(js_name = pointerGraceMs)]
#[must_use]
pub fn pointer_grace_ms() -> u32 {
    POINTER_GRACE_MS
}

/// JavaScript-facing copy-feedback duration.
#[wasm_bindgen(js_name = copiedFeedbackMs)]
#[must_use]
pub fn copied_feedback_ms() -> u32 {
    COPIED_FEEDBACK_MS
}

/// Writes exact text through the async Clipboard API or the textarea fallback.
#[wasm_bindgen(js_name = writeClipboard)]
pub fn write_clipboard(text: String) -> Promise {
    future_to_promise(async move { write_clipboard_inner(&text).await.map(JsValue::from_bool) })
}

async fn write_clipboard_inner(text: &str) -> Result<bool, JsValue> {
    let global = js_sys::global();
    let navigator = Reflect::get(&global, &JsValue::from_str("navigator"))?;
    let clipboard = Reflect::get(&navigator, &JsValue::from_str("clipboard"))?;
    if !clipboard.is_null() && !clipboard.is_undefined() {
        let write_text = Reflect::get(&clipboard, &JsValue::from_str("writeText"))?;
        if write_text.is_truthy() {
            let Ok(write_text) = write_text.dyn_into::<Function>() else {
                return Ok(false);
            };
            let Ok(pending) = write_text.call1(&clipboard, &JsValue::from_str(text)) else {
                return Ok(false);
            };
            return Ok(JsFuture::from(Promise::resolve(&pending)).await.is_ok());
        }
    }

    let document = required_property(&global, "document", "global")?;
    let exec = Reflect::get(&document, &JsValue::from_str("execCommand"))?;
    let Ok(exec) = exec.dyn_into::<Function>() else {
        return Ok(false);
    };
    let textarea = call_method(&document, "createElement", &[JsValue::from_str("textarea")])?;
    Reflect::set(
        &textarea,
        &JsValue::from_str("value"),
        &JsValue::from_str(text),
    )?;
    call_method(
        &textarea,
        "setAttribute",
        &[JsValue::from_str("readonly"), JsValue::from_str("")],
    )?;
    let style = required_property(&textarea, "style", "textarea")?;
    Reflect::set(
        &style,
        &JsValue::from_str("position"),
        &JsValue::from_str("fixed"),
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("left"),
        &JsValue::from_str("-9999px"),
    )?;
    let body = required_property(&document, "body", "document")?;
    call_method(&body, "appendChild", std::slice::from_ref(&textarea))?;
    call_method(&textarea, "select", &[])?;
    let accepted = exec.call1(&document, &JsValue::from_str("copy"));
    call_method(&textarea, "remove", &[])?;
    Ok(accepted
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false))
}

/// Compiled React hook implementing the cancelable popup-close grace.
///
/// # Errors
///
/// Returns missing React configuration or hook/timer boundary failures.
#[wasm_bindgen(js_name = usePointerGrace)]
#[allow(clippy::needless_pass_by_value)]
pub fn use_pointer_grace(close: Function) -> Result<JsValue, JsValue> {
    let react = configured_react()?;
    let timer = use_ref(&react, &JsValue::NULL)?;
    let close_ref = use_ref(&react, close.as_ref())?;
    Reflect::set(&close_ref, &JsValue::from_str("current"), close.as_ref())?;

    let cancel_timer = timer.clone();
    let cancel = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let current = Reflect::get(&cancel_timer, &JsValue::from_str("current"))?;
        if current.is_null() {
            return Ok(());
        }
        let global = js_sys::global();
        function(&global, "clearTimeout")?.call1(&global, &current)?;
        Reflect::set(&cancel_timer, &JsValue::from_str("current"), &JsValue::NULL)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let cancel = use_callback(&react, &cancel.into_js_value(), &Array::new())?;

    let arm_cancel = cancel.clone();
    let arm_timer = timer;
    let arm_close = close_ref;
    let arm = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        arm_cancel.call0(&JsValue::UNDEFINED)?;
        let callback_timer = arm_timer.clone();
        let callback_close = arm_close.clone();
        let callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            Reflect::set(
                &callback_timer,
                &JsValue::from_str("current"),
                &JsValue::NULL,
            )?;
            let close = Reflect::get(&callback_close, &JsValue::from_str("current"))?
                .dyn_into::<Function>()?;
            close.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let global = js_sys::global();
        let handle = function(&global, "setTimeout")?.call2(
            &global,
            &callback.into_js_value(),
            &JsValue::from_f64(f64::from(POINTER_GRACE_MS)),
        )?;
        Reflect::set(&arm_timer, &JsValue::from_str("current"), &handle)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let arm = use_callback(&react, &arm.into_js_value(), &Array::of1(cancel.as_ref()))?;

    let cleanup = cancel.clone();
    let effect = Closure::wrap(
        Box::new(move || -> JsValue { cleanup.clone().into() }) as Box<dyn FnMut() -> JsValue>
    );
    function(&react, "useEffect")?.call2(
        &react,
        &effect.into_js_value(),
        &Array::of1(cancel.as_ref()),
    )?;
    object(&[("arm", arm.into()), ("cancel", cancel.into())]).map(Into::into)
}

/// Compiled React copy-feedback hook.
///
/// # Errors
///
/// Returns missing React configuration or hook boundary failures.
#[wasm_bindgen(js_name = useCopyFeedback)]
pub fn use_copy_feedback(text: String) -> Result<JsValue, JsValue> {
    let react = configured_react()?;
    let (copied, set_copied) = use_state(&react, &JsValue::FALSE)?;
    let copied_flag = copied.as_bool().unwrap_or(false);
    let text_dependency = JsValue::from_str(&text);
    let callback_text = text;
    let callback_setter = set_copied.clone();
    let on_copy = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if copied_flag {
            return Ok(());
        }
        let pending = write_clipboard(callback_text.clone());
        let setter = callback_setter.clone();
        let settled = Closure::wrap(Box::new(move |accepted: JsValue| -> Result<(), JsValue> {
            if accepted.as_bool() != Some(true) {
                return Ok(());
            }
            set_state(&setter, &JsValue::TRUE)?;
            let reset = setter.clone();
            let callback = Closure::wrap(Box::new(move || set_state(&reset, &JsValue::FALSE))
                as Box<dyn FnMut() -> Result<(), JsValue>>);
            let window = required_property(&js_sys::global(), "window", "global")?;
            function(&window, "setTimeout")?.call2(
                &window,
                &callback.into_js_value(),
                &JsValue::from_f64(f64::from(COPIED_FEEDBACK_MS)),
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        call_method(&pending, "then", &[settled.into_js_value()])?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let dependencies = Array::new();
    dependencies.push(&copied);
    dependencies.push(&text_dependency);
    let on_copy = use_callback(&react, &on_copy.into_js_value(), &dependencies)?;
    object(&[("copied", copied), ("onCopy", on_copy.into())]).map(Into::into)
}

/// Compiled viewport-fit hook for bottom-anchored overlays.
///
/// # Errors
///
/// Returns missing React configuration or DOM/event boundary failures.
#[wasm_bindgen(js_name = useAnchoredMaxHeight)]
#[allow(clippy::needless_pass_by_value)]
pub fn use_anchored_max_height(
    reference: JsValue,
    cap: f64,
    signal: JsValue,
) -> Result<f64, JsValue> {
    let react = configured_react()?;
    let (height, set_height) = use_state(&react, &JsValue::from_f64(cap))?;
    let effect_reference = reference.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let element = Reflect::get(&effect_reference, &JsValue::from_str("current"))?;
        if element.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        let fit_element = element.clone();
        let fit_setter = set_height.clone();
        let fit = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let bounds = call_method(&fit_element, "getBoundingClientRect", &[])?;
            let bottom = required_property(&bounds, "bottom", "DOMRect")?
                .as_f64()
                .ok_or_else(|| js_error("DOMRect bottom must be a number"))?;
            let fitted =
                js_sys::Math::min(cap, js_sys::Math::max(0.0, bottom - ANCHORED_MARGIN_PX));
            set_state(&fit_setter, &JsValue::from_f64(fitted))
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let fit = fit.into_js_value().dyn_into::<Function>()?;
        fit.call0(&JsValue::UNDEFINED)?;
        let window = required_property(&js_sys::global(), "window", "global")?;
        call_method(
            &window,
            "addEventListener",
            &[JsValue::from_str("resize"), fit.clone().into()],
        )?;
        call_method(
            &window,
            "addEventListener",
            &[
                JsValue::from_str("scroll"),
                fit.clone().into(),
                JsValue::TRUE,
            ],
        )?;
        let cleanup_window = window;
        let cleanup_fit = fit;
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(
                &cleanup_window,
                "removeEventListener",
                &[JsValue::from_str("resize"), cleanup_fit.clone().into()],
            )?;
            call_method(
                &cleanup_window,
                "removeEventListener",
                &[
                    JsValue::from_str("scroll"),
                    cleanup_fit.clone().into(),
                    JsValue::TRUE,
                ],
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dependencies = Array::new();
    dependencies.push(&reference);
    dependencies.push(&JsValue::from_f64(cap));
    dependencies.push(&signal);
    function(&react, "useLayoutEffect")?.call2(&react, &effect.into_js_value(), &dependencies)?;
    height
        .as_f64()
        .ok_or_else(|| js_error("useState returned a non-number max height"))
}

fn configured_react() -> Result<JsValue, JsValue> {
    REACT.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_error("client-ui-primitives hook module was not configured with React")
        })
    })
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&function(react, "useState")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into::<Function>()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    function(react, "useRef")?.call1(react, initial)
}

fn use_callback(
    react: &JsValue,
    callback: &JsValue,
    dependencies: &Array,
) -> Result<Function, JsValue> {
    function(react, "useCallback")?
        .call2(react, callback, dependencies)?
        .dyn_into::<Function>()
}

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required_property(value, key, "object")?.dyn_into::<Function>()
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!(
            "client-ui-primitives: {owner} omitted required property {key:?}"
        ))
        .into())
    } else {
        Ok(property)
    }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
