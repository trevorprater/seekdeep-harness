//! Browser renderless flow and transactional Slot registration.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{INJECT, NativeDirectoryFlowState};

thread_local! {
    static REACT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// Configures React for the renderless occupant.
#[wasm_bindgen(js_name = configureClientUiDirectoryPickerNative)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_directory_picker_native(react: JsValue) {
    REACT.with(|configured| *configured.borrow_mut() = Some(react));
}

/// Applies the browser native-directory flow plugin.
///
/// # Errors
///
/// Returns missing service, Slot deferral, duplicate registration, rollback, or component failures.
#[wasm_bindgen(js_name = applyClientUiDirectoryPickerNative)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_directory_picker_native(ctx: JsValue) -> Result<(), JsValue> {
    let slots = required(&ctx, "slots", "Client Context")?;
    let workspaces = required(&ctx, "workspaces", "Client Context")?;
    let component = native_directory_flow_component()?;
    let outer_slots = slots.clone();
    let outer_workspaces = workspaces;
    let outer_component = component;
    let outer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let inner_slots = outer_slots.clone();
        let inner_workspaces = outer_workspaces.clone();
        let inner_component = outer_component.clone();
        let inner = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            register_pair(&inner_slots, &inner_workspaces, &inner_component)
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        call_method(
            &outer_slots,
            "inject",
            &[
                JsValue::from_str("sidebar.workspaces.directoryFlow"),
                inner.into_js_value(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.hero.workspace.directoryFlow"),
            outer.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = directoryPickerNativeInject)]
pub fn directory_picker_native_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled renderless flow component.
///
/// # Errors
///
/// Returns before React is configured.
#[wasm_bindgen(js_name = nativeDirectoryFlowComponent)]
pub fn native_directory_flow_component() -> Result<JsValue, JsValue> {
    let react = REACT.with(|react| {
        react.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-directory-picker-native is not configured")
        })
    })?;
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_flow(&react, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

fn register_pair(
    slots: &JsValue,
    workspaces: &JsValue,
    component: &JsValue,
) -> Result<JsValue, JsValue> {
    let inject_workspaces = workspaces.clone();
    let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let pick_workspaces = inject_workspaces.clone();
        let pick =
            Closure::wrap(
                Box::new(move || call_method(&pick_workspaces, "pickDirectory", &[]))
                    as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
            );
        object(&[("pick", pick.into_js_value())]).map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let hero = object(&[
        (
            "name",
            JsValue::from_str("conversation.hero.workspace.directoryFlow"),
        ),
        ("inject", inject.into_js_value()),
    ])?;
    let first = call_method(slots, "register", &[hero.into(), component.clone()])?;
    let inject_workspaces = workspaces.clone();
    let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let pick_workspaces = inject_workspaces.clone();
        let pick =
            Closure::wrap(
                Box::new(move || call_method(&pick_workspaces, "pickDirectory", &[]))
                    as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
            );
        object(&[("pick", pick.into_js_value())]).map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let sidebar = object(&[
        (
            "name",
            JsValue::from_str("sidebar.workspaces.directoryFlow"),
        ),
        ("inject", inject.into_js_value()),
    ])?;
    let second = match call_method(slots, "register", &[sidebar.into(), component.clone()]) {
        Ok(disposer) => disposer,
        Err(error) => {
            call_disposer(&first);
            return Err(error);
        }
    };
    let cleanup = Closure::wrap(Box::new(move || {
        call_disposer(&second);
        call_disposer(&first);
    }) as Box<dyn FnMut()>);
    Ok(cleanup.into_js_value())
}

fn state_face() -> Result<JsValue, JsValue> {
    let state = Rc::new(RefCell::new(NativeDirectoryFlowState::new()));
    let face = Object::new();
    let mount_state = state.clone();
    let mount =
        Closure::wrap(Box::new(move || mount_state.borrow_mut().mount()) as Box<dyn FnMut()>);
    set(&face, "mount", &mount.into_js_value())?;
    let unmount_state = state.clone();
    let unmount =
        Closure::wrap(Box::new(move || unmount_state.borrow_mut().unmount()) as Box<dyn FnMut()>);
    set(&face, "unmount", &unmount.into_js_value())?;
    let reconcile_state = state.clone();
    let reconcile =
        Closure::wrap(
            Box::new(move |open: bool| reconcile_state.borrow_mut().reconcile_open(open))
                as Box<dyn FnMut(bool) -> bool>,
        );
    set(&face, "reconcile", &reconcile.into_js_value())?;
    let accepts = Closure::wrap(
        Box::new(move || state.borrow().accepts_settlement()) as Box<dyn FnMut() -> bool>
    );
    set(&face, "accepts", &accepts.into_js_value())?;
    Ok(face.into())
}

fn render_flow(react: &JsValue, props: &JsValue) -> Result<JsValue, JsValue> {
    let open = required(props, "open", "NativeDirectoryFlow")?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new("NativeDirectoryFlow open must be boolean"))?;
    let pick = required_function(props, "pick", "NativeDirectoryFlow")?;
    let state_ref = use_ref(react, &JsValue::UNDEFINED)?;
    let mut state = Reflect::get(&state_ref, &JsValue::from_str("current"))?;
    if state.is_undefined() {
        state = state_face()?;
        Reflect::set(&state_ref, &JsValue::from_str("current"), &state)?;
    }
    let outcome_ref = use_ref(react, props)?;
    Reflect::set(&outcome_ref, &JsValue::from_str("current"), props)?;

    let lifetime_state = state.clone();
    let lifetime = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        call_method(&lifetime_state, "mount", &[])?;
        let cleanup_state = lifetime_state.clone();
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(&cleanup_state, "unmount", &[]);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(react, &lifetime.into_js_value(), &Array::new())?;

    let effect_state = state.clone();
    let effect_outcome = outcome_ref;
    let effect_pick = pick.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if call_method(&effect_state, "reconcile", &[JsValue::from_bool(open)])?.as_bool()
            != Some(true)
        {
            return Ok(());
        }
        let returned = effect_pick.call0(&JsValue::UNDEFINED)?;
        let promise = Promise::resolve(&returned);
        let success_state = effect_state.clone();
        let success_outcome = effect_outcome.clone();
        let success = Closure::wrap(Box::new(move |path: JsValue| -> Result<(), JsValue> {
            if call_method(&success_state, "accepts", &[])?.as_bool() != Some(true) {
                return Ok(());
            }
            let outcome = Reflect::get(&success_outcome, &JsValue::from_str("current"))?;
            if path.is_null() {
                call_method(&outcome, "onCancel", &[])?;
            } else {
                call_method(&outcome, "onPicked", &[path])?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let failure_state = effect_state.clone();
        let failure_outcome = effect_outcome.clone();
        let failure = Closure::wrap(Box::new(move |error: JsValue| -> Result<(), JsValue> {
            if call_method(&failure_state, "accepts", &[])?.as_bool() != Some(true) {
                return Ok(());
            }
            let outcome = Reflect::get(&failure_outcome, &JsValue::from_str("current"))?;
            call_method(
                &outcome,
                "onError",
                &[JsValue::from_str(&js_error_text(&error))],
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        call_method(
            promise.as_ref(),
            "then",
            &[success.into_js_value(), failure.into_js_value()],
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(open), pick.as_ref()),
    )?;
    Ok(JsValue::NULL)
}

fn call_disposer(value: &JsValue) {
    if let Some(disposer) = value.dyn_ref::<Function>() {
        let _ = disposer.call0(&JsValue::UNDEFINED);
    }
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn use_effect(react: &JsValue, effect: &JsValue, dependencies: &Array) -> Result<(), JsValue> {
    required_function(react, "useEffect", "React")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

fn set(value: &Object, key: &str, entry: &JsValue) -> Result<(), JsValue> {
    Reflect::set(value, &JsValue::from_str(key), entry).map(|_| ())
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let entry = Reflect::get(value, &JsValue::from_str(key))?;
    if entry.is_null() || entry.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into())
    } else {
        Ok(entry)
    }
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into()
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_error_text(value: &JsValue) -> String {
    Reflect::get(value, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
        .or_else(|| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}
