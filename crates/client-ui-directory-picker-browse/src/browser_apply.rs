//! Browser locale and transactional dual-Slot assembly.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{DIRECTORY_BROWSER_LOCALES, DIRECTORY_BROWSER_NS, browse_directory_flow_component};

const INJECT: &[&str] = &["slots", "workspaces", "locale"];

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

fn dictionary(language: &str) -> Result<JsValue, JsValue> {
    let output = Object::new();
    for (key, zh, en) in DIRECTORY_BROWSER_LOCALES {
        Reflect::set(
            &output,
            &JsValue::from_str(key),
            &JsValue::from_str(if language == "zh" { zh } else { en }),
        )?;
    }
    Ok(output.into())
}

fn own_locales(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let locale = locale.clone();
    let install = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let zh = call(
            &locale,
            "register",
            &[
                JsValue::from_str(DIRECTORY_BROWSER_NS),
                JsValue::from_str("zh"),
                dictionary("zh")?,
            ],
        )?
        .dyn_into::<Function>()?;
        let en = match call(
            &locale,
            "register",
            &[
                JsValue::from_str(DIRECTORY_BROWSER_NS),
                JsValue::from_str("en"),
                dictionary("en")?,
            ],
        ) {
            Ok(dispose) => dispose.dyn_into::<Function>()?,
            Err(error) => {
                let _ = zh.call0(&JsValue::UNDEFINED);
                return Err(error);
            }
        };
        Ok(Closure::wrap(Box::new(move || {
            let _ = zh.call0(&JsValue::UNDEFINED);
            let _ = en.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call(
        ctx,
        "effect",
        &[
            install.into_js_value(),
            JsValue::from_str("directory-picker-browse: dialog dictionaries"),
        ],
    )?;
    Ok(())
}

fn injected_face(workspaces: &JsValue, translate: &Function) -> Result<JsValue, JsValue> {
    let list_workspaces = workspaces.clone();
    let list = Closure::wrap(Box::new(move |path: JsValue, signal: JsValue| {
        call(&list_workspaces, "listDirectory", &[path, signal])
    })
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    let create_workspaces = workspaces.clone();
    let create = Closure::wrap(Box::new(move |path: JsValue, name: JsValue| {
        call(&create_workspaces, "createDirectory", &[path, name])
    })
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    Ok(object(&[
        ("listDirectory", list.into_js_value()),
        ("createDirectory", create.into_js_value()),
        ("t", translate.clone().into()),
    ])?
    .into())
}

fn register_pair(
    slots: &JsValue,
    inject: &Function,
    component: &JsValue,
) -> Result<JsValue, JsValue> {
    let first = call(
        slots,
        "register",
        &[
            object(&[
                (
                    "name",
                    JsValue::from_str("conversation.hero.workspace.directoryFlow"),
                ),
                ("inject", inject.clone().into()),
            ])?
            .into(),
            component.clone(),
        ],
    )?
    .dyn_into::<Function>()?;
    let second = match call(
        slots,
        "register",
        &[
            object(&[
                (
                    "name",
                    JsValue::from_str("sidebar.workspaces.directoryFlow"),
                ),
                ("inject", inject.clone().into()),
            ])?
            .into(),
            component.clone(),
        ],
    ) {
        Ok(dispose) => dispose.dyn_into::<Function>()?,
        Err(error) => {
            let _ = first.call0(&JsValue::UNDEFINED);
            return Err(error);
        }
    };
    Ok(Closure::wrap(Box::new(move || {
        let _ = second.call0(&JsValue::UNDEFINED);
        let _ = first.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>)
    .into_js_value())
}

/// Applies locale ownership and the all-or-nothing pair of directory-flow entries.
///
/// # Errors
///
/// Returns missing services, locale conflicts, Slot conflicts, or rollback failures.
#[wasm_bindgen(js_name = applyClientUiDirectoryPickerBrowse)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_directory_picker_browse(ctx: JsValue) -> Result<(), JsValue> {
    let slots = required(&ctx, "slots", "Client Context")?;
    let workspaces = required(&ctx, "workspaces", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    own_locales(&ctx, &locale)?;
    let translate = call(&locale, "bind", &[JsValue::from_str(DIRECTORY_BROWSER_NS)])?
        .dyn_into::<Function>()?;
    let face = injected_face(&workspaces, &translate)?;
    let face_factory = Closure::wrap(Box::new(move || face.clone()) as Box<dyn FnMut() -> JsValue>)
        .into_js_value()
        .dyn_into::<Function>()?;
    let component = browse_directory_flow_component()?;
    let inner_slots = slots.clone();
    let inner_face = face_factory;
    let inner_component = component;
    let inner =
        Closure::wrap(
            Box::new(move || register_pair(&inner_slots, &inner_face, &inner_component))
                as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
        );
    let outer_slots = slots.clone();
    let inner_value = inner.into_js_value();
    let outer = Closure::wrap(Box::new(move || {
        call(
            &outer_slots,
            "inject",
            &[
                JsValue::from_str("sidebar.workspaces.directoryFlow"),
                inner_value.clone(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.hero.workspace.directoryFlow"),
            outer.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Exact Client service dependencies.
#[wasm_bindgen(js_name = directoryPickerBrowseInject)]
pub fn directory_picker_browse_inject() -> Array {
    INJECT
        .iter()
        .map(|value| JsValue::from_str(value))
        .collect()
}
