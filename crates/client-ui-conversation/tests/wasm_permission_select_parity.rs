//! Live WASM coverage for permission selection and risk confirmation.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_permission_select, permission_select_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let lines = []
let pending = []
let commandMode = 'pending'
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
export function installPermissionBench() {
  hooks = []; cursor = 0; lines = []; pending = []; commandMode = 'pending'
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    Fragment: 'Fragment', createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
    useEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        hooks[index]?.cleanup?.()
        hooks[index] = { type: 'effect', deps: [...deps], cleanup: effect() }
      }
    },
  }
  return {
    React,
    uiPrimitives: { Menu: 'Menu', RiskConfirmation: 'RiskConfirmation', IconChevronDownOutline14: 'IconChevronDownOutline14' },
  }
}
export function permissionObject(entries) { return Object.fromEntries(entries) }
export function permissionRender(component, props) { cursor = 0; return component(props) }
export function permissionCommand() {
  return line => {
    lines.push(line)
    if (commandMode === 'resolve') return Promise.resolve(true)
    if (commandMode === 'reject') return Promise.reject(new Error('rejected'))
    return new Promise((resolve, reject) => { pending.push({ resolve, reject }) })
  }
}
export function permissionSetCommandMode(mode) { commandMode = mode }
export function permissionResolve(ok) { pending.shift()?.resolve(ok) }
export function permissionLines() { return lines }
export function makePermissionTranslate() {
  const copy = {
    'access.confirm.title': 'Confirm Full access?', 'access.confirm.description': 'This can modify anything.',
    'access.confirm.acknowledge': 'I understand', 'access.confirm.cancel': 'Cancel', 'access.confirm.enable': 'Enable Full access',
  }
  return (key, vars) => key === 'input.accessMode' ? `Access mode ${vars.name}` : copy[key] ?? key
}
export function permissionFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = permissionFindKind(child, kind); if (found) return found }
  return undefined
}
export function permissionText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(permissionText).join('')
  return permissionText(value.children)
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installPermissionBench)]
    fn install_permission_bench() -> JsValue;
    #[wasm_bindgen(js_name = permissionObject)]
    fn permission_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = permissionRender)]
    fn permission_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = permissionCommand)]
    fn permission_command() -> Function;
    #[wasm_bindgen(js_name = permissionSetCommandMode)]
    fn permission_set_command_mode(mode: &str);
    #[wasm_bindgen(js_name = permissionResolve)]
    fn permission_resolve(ok: bool);
    #[wasm_bindgen(js_name = permissionLines)]
    fn permission_lines() -> Array;
    #[wasm_bindgen(js_name = makePermissionTranslate)]
    fn make_permission_translate() -> Function;
    #[wasm_bindgen(js_name = permissionFindKind)]
    fn permission_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = permissionText)]
    fn permission_text(value: &JsValue) -> String;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn child(value: &JsValue, index: u32) -> JsValue {
    property(value, "children")
        .unchecked_into::<Array>()
        .get(index)
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    permission_object(&array).unchecked_into()
}

fn option(value: &str, name: &str, description: Option<&str>) -> Object {
    let mut entries = vec![
        ("value", JsValue::from_str(value)),
        ("name", JsValue::from_str(name)),
    ];
    if let Some(description) = description {
        entries.push(("description", JsValue::from_str(description)));
    }
    object(&entries)
}

fn permissions(current: &str) -> Object {
    object(&[
        (
            "options",
            Array::of4(
                option("read-only", "read-only", Some("Read only")).as_ref(),
                option(
                    "workspace-write",
                    "workspace-write",
                    Some("Workspace write"),
                )
                .as_ref(),
                option(FULL_ACCESS, FULL_ACCESS, Some("Full access")).as_ref(),
                option("custom", "custom", None).as_ref(),
            )
            .into(),
        ),
        ("currentValue", JsValue::from_str(current)),
    ])
}

const FULL_ACCESS: &str = "danger-full-access";

fn setup(value: JsValue, locked: bool) -> (JsValue, Object) {
    let bench = install_permission_bench();
    configure_client_ui_conversation_permission_select(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    let props = object(&[
        ("value", value),
        ("locked", JsValue::from_bool(locked)),
        ("command", permission_command().into()),
        ("t", make_permission_translate().into()),
    ]);
    (permission_select_component().unwrap(), props)
}

async fn flush_microtasks() {
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
}

#[wasm_bindgen_test]
fn absent_projection_renders_null_after_running_all_hooks() {
    let (component, props) = setup(JsValue::UNDEFINED, false);
    assert!(permission_render(&component, props.as_ref()).is_null());
}

#[wasm_bindgen_test]
fn menu_filters_custom_title_cases_names_and_reuses_stable_glyphs() {
    let (component, props) = setup(permissions("read-only").into(), false);
    let tree = permission_render(&component, props.as_ref());
    let menu = permission_find_kind(&tree, "Menu");
    let menu_props = property(&menu, "props");
    let items = property(&menu_props, "items").unchecked_into::<Array>();
    assert_eq!(items.length(), 3);
    assert_eq!(
        property(&items.get(0), "label").as_string().as_deref(),
        Some("Read Only")
    );
    assert_eq!(
        property(&items.get(1), "label").as_string().as_deref(),
        Some("Workspace Write")
    );
    assert_eq!(
        property(&items.get(2), "label").as_string().as_deref(),
        Some("Full access")
    );
    assert_eq!(
        property(&menu_props, "selectedId").as_string().as_deref(),
        Some("read-only")
    );
    let anchor = property(&menu_props, "anchor");
    assert!(permission_text(&anchor).contains("Read Only"));
    assert_eq!(
        property(&property(&anchor, "props"), "title")
            .as_string()
            .as_deref(),
        Some("Read only")
    );
    assert!(Object::is(
        &property(&items.get(0), "icon"),
        &child(&child(&anchor, 0), 0)
    ));
    let risk = permission_find_kind(&tree, "RiskConfirmation");
    assert_eq!(
        property(&property(&risk, "props"), "open").as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&property(&risk, "props"), "confirmLabel")
            .as_string()
            .as_deref(),
        Some("Enable Full access")
    );
}

#[wasm_bindgen_test(async)]
async fn ordinary_pick_is_optimistic_disabled_and_clears_after_resolve_or_reject() {
    let (component, props) = setup(permissions("read-only").into(), false);
    let tree = permission_render(&component, props.as_ref());
    let menu = permission_find_kind(&tree, "Menu");
    property(&property(&menu, "props"), "onSelect")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("workspace-write"))
        .unwrap();
    let busy = permission_render(&component, props.as_ref());
    let menu = permission_find_kind(&busy, "Menu");
    assert_eq!(
        property(&property(&menu, "props"), "selectedId")
            .as_string()
            .as_deref(),
        Some("workspace-write")
    );
    let anchor = property(&property(&menu, "props"), "anchor");
    assert_eq!(
        property(&property(&anchor, "props"), "disabled").as_bool(),
        Some(true)
    );
    assert_eq!(
        permission_lines().get(0).as_string().as_deref(),
        Some("/permission workspace-write")
    );
    permission_resolve(true);
    flush_microtasks().await;
    let settled = permission_render(&component, props.as_ref());
    let menu = permission_find_kind(&settled, "Menu");
    assert_eq!(
        property(&property(&menu, "props"), "selectedId")
            .as_string()
            .as_deref(),
        Some("read-only")
    );

    permission_set_command_mode("reject");
    property(&property(&menu, "props"), "onSelect")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("workspace-write"))
        .unwrap();
    flush_microtasks().await;
    let rearmed = permission_render(&component, props.as_ref());
    let menu = permission_find_kind(&rearmed, "Menu");
    assert_eq!(
        property(&property(&menu, "props"), "selectedId")
            .as_string()
            .as_deref(),
        Some("read-only")
    );
}

#[wasm_bindgen_test(async)]
async fn full_access_requires_acknowledgement_then_closes_and_submits() {
    let (component, props) = setup(permissions("workspace-write").into(), false);
    let tree = permission_render(&component, props.as_ref());
    let menu = permission_find_kind(&tree, "Menu");
    property(&property(&menu, "props"), "onSelect")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(FULL_ACCESS))
        .unwrap();
    let confirmation = permission_render(&component, props.as_ref());
    let risk = permission_find_kind(&confirmation, "RiskConfirmation");
    assert_eq!(
        property(&property(&risk, "props"), "open").as_bool(),
        Some(true)
    );
    property(&property(&risk, "props"), "onConfirm")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(permission_lines().length(), 0);
    property(&property(&risk, "props"), "onAcknowledgedChange")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::TRUE)
        .unwrap();
    let acknowledged = permission_render(&component, props.as_ref());
    let risk = permission_find_kind(&acknowledged, "RiskConfirmation");
    assert_eq!(
        property(&property(&risk, "props"), "acknowledged").as_bool(),
        Some(true)
    );
    property(&property(&risk, "props"), "onConfirm")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let busy = permission_render(&component, props.as_ref());
    let risk = permission_find_kind(&busy, "RiskConfirmation");
    assert_eq!(
        property(&property(&risk, "props"), "open").as_bool(),
        Some(false)
    );
    assert_eq!(
        permission_lines().get(0).as_string().as_deref(),
        Some("/permission danger-full-access")
    );
    permission_resolve(true);
    flush_microtasks().await;
}

#[wasm_bindgen_test]
fn cancel_and_lock_reset_confirmation_acknowledgement_and_menu_state() {
    let (component, props) = setup(permissions("workspace-write").into(), false);
    let tree = permission_render(&component, props.as_ref());
    let menu = permission_find_kind(&tree, "Menu");
    property(&property(&menu, "props"), "onSelect")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(FULL_ACCESS))
        .unwrap();
    let confirmation = permission_render(&component, props.as_ref());
    let risk = permission_find_kind(&confirmation, "RiskConfirmation");
    property(&property(&risk, "props"), "onAcknowledgedChange")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::TRUE)
        .unwrap();
    let acknowledged = permission_render(&component, props.as_ref());
    let risk = permission_find_kind(&acknowledged, "RiskConfirmation");
    property(&property(&risk, "props"), "onCancel")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let cancelled = permission_render(&component, props.as_ref());
    let risk = permission_find_kind(&cancelled, "RiskConfirmation");
    assert_eq!(
        property(&property(&risk, "props"), "open").as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&property(&risk, "props"), "acknowledged").as_bool(),
        Some(false)
    );

    let menu = permission_find_kind(&cancelled, "Menu");
    property(&property(&menu, "props"), "onSelect")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(FULL_ACCESS))
        .unwrap();
    let confirmation = permission_render(&component, props.as_ref());
    let risk = permission_find_kind(&confirmation, "RiskConfirmation");
    property(&property(&risk, "props"), "onAcknowledgedChange")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::TRUE)
        .unwrap();
    let _ = permission_render(&component, props.as_ref());
    let locked_props = object(&[
        ("value", permissions("workspace-write").into()),
        ("locked", JsValue::TRUE),
        ("command", permission_command().into()),
        ("t", make_permission_translate().into()),
    ]);
    let _ = permission_render(&component, locked_props.as_ref());
    let locked = permission_render(&component, locked_props.as_ref());
    let menu = permission_find_kind(&locked, "Menu");
    let risk = permission_find_kind(&locked, "RiskConfirmation");
    assert_eq!(
        property(&property(&menu, "props"), "open").as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&property(&risk, "props"), "open").as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&property(&risk, "props"), "acknowledged").as_bool(),
        Some(false)
    );
    let anchor = property(&property(&menu, "props"), "anchor");
    assert_eq!(
        property(&property(&anchor, "props"), "disabled").as_bool(),
        Some(true)
    );
}
