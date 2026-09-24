//! Live browser RPC and observable parity for Agent preset stores.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_agent_preset::{
    agent_preset_draft_blocker_js, create_agent_preset_seat_controller,
    create_agent_preset_section_controller, create_agent_preset_settings_controller,
    write_agent_preset_default_js,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const ok = value => ({ result: { ok: true, value } })
const fail = message => ({ result: { ok: false, error: { message } } })

export function makeAgentPresetApi() {
  const calls = []
  const contents = new Map([
    ['standard', { name: 'Standard', content: '- id: tool-bash\n' }],
    ['minimal', { content: '[]\n' }],
  ])
  const state = {
    presets: [
      { id: 'standard', trust: 'system', isDefault: true, name: 'Standard' },
      { id: 'minimal', trust: 'system', isDefault: false },
      { id: 'damaged', trust: 'user', isDefault: false, broken: 'invalid composition' },
    ],
    writable: true,
    updateFailure: '',
    selectFailure: '',
    current: undefined,
  }
  const api = {
    agentPresets: {
      list(request) { calls.push(['list', structuredClone(request)]); return Promise.resolve(ok({ presets: structuredClone(state.presets), authorable: true, hasDocument: false })) },
      select(request) { calls.push(['select', structuredClone(request)]); return Promise.resolve(state.selectFailure ? fail(state.selectFailure) : ok({ agentPreset: request.agentPreset })) },
      read(request) { calls.push(['read', structuredClone(request)]); const value=contents.get(request.agentPreset); return Promise.resolve(value ? ok({ agentPreset: request.agentPreset, trust: 'system', ...structuredClone(value) }) : fail('unknown preset')) },
      copy(request) { calls.push(['copy', structuredClone(request)]); const source=contents.get(request.from); contents.set(request.agentPreset, { content: source.content, ...(request.name === undefined ? {} : { name: request.name }) }); state.presets.push({ id: request.agentPreset, trust: 'user', isDefault: false, ...(request.name === undefined ? {} : { name: request.name }) }); return Promise.resolve(ok({ agentPreset: request.agentPreset })) },
      openDocument(request) { calls.push(['openDocument', structuredClone(request)]); return Promise.resolve(ok({ opened: false, path: `/presets/${request.agentPreset}` })) },
      remove(request) { calls.push(['remove', structuredClone(request)]); contents.delete(request.agentPreset); state.presets = state.presets.filter(row => row.id !== request.agentPreset); return Promise.resolve(ok({})) },
    },
    settings: {
      describe(request) { calls.push(['describe', structuredClone(request)]); return Promise.resolve(ok({ writable: state.writable })) },
      update(request) { calls.push(['update', structuredClone(request)]); if (state.updateFailure) return Promise.resolve(fail(state.updateFailure)); for (const row of state.presets) row.isDefault = row.id === request.patch.default; return Promise.resolve(ok({})) },
    },
  }
  return { api, calls, state }
}

export function tick() { return new Promise(resolve => setTimeout(resolve, 0)) }
export function call(face, method, args) { return face[method](...args) }
export function snapshot(face, hook) { return face.hooks[hook].getSnapshot() }
export function calls(bench) { return bench.calls }
export function setCurrent(bench, value) { bench.state.current = value }
export function currentReader(bench) { return () => bench.state.current }
export function setFailure(bench, key, value) { bench.state[key] = value }
"#)]
extern "C" {
    fn makeAgentPresetApi() -> JsValue;
    fn tick() -> Promise;
    fn call(face: &JsValue, method: &str, arguments: &Array) -> JsValue;
    fn snapshot(face: &JsValue, hook: &str) -> JsValue;
    fn calls(bench: &JsValue) -> Array;
    fn setCurrent(bench: &JsValue, value: &JsValue);
    fn currentReader(bench: &JsValue) -> Function;
    fn setFailure(bench: &JsValue, key: &str, value: &str);
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn invoke(face: &JsValue, method: &str, arguments: &[JsValue]) {
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    call(face, method, &args);
}

async fn settle() {
    for _ in 0..5 {
        JsFuture::from(tick()).await.unwrap();
    }
}

#[wasm_bindgen_test(async)]
async fn settings_face_filters_roster_and_restores_refused_default() {
    let bench = makeAgentPresetApi();
    let face = create_agent_preset_settings_controller(property(&bench, "api")).unwrap();
    invoke(&face, "load", &[]);
    settle().await;
    let ready = snapshot(&face, "agentPresetSettings");
    assert_eq!(
        property(&ready, "status").as_string().as_deref(),
        Some("ready")
    );
    assert_eq!(
        property(&ready, "currentValue").as_string().as_deref(),
        Some("standard")
    );
    assert_eq!(Array::from(&property(&ready, "options")).length(), 2);

    invoke(&face, "select", &[JsValue::from_str("minimal")]);
    settle().await;
    assert_eq!(
        property(&snapshot(&face, "agentPresetSettings"), "currentValue")
            .as_string()
            .as_deref(),
        Some("minimal")
    );
    setFailure(&bench, "updateFailure", "read-only settings");
    invoke(&face, "select", &[JsValue::from_str("standard")]);
    settle().await;
    let failed = snapshot(&face, "agentPresetSettings");
    assert_eq!(
        property(&failed, "currentValue").as_string().as_deref(),
        Some("minimal")
    );
    assert_eq!(
        property(&failed, "error").as_string().as_deref(),
        Some("read-only settings")
    );
}

#[wasm_bindgen_test(async)]
async fn seat_face_stages_then_spends_one_blank_session_switch() {
    let bench = makeAgentPresetApi();
    let applied = Array::new();
    let applied_log = applied.clone();
    let on_applied = Closure::wrap(Box::new(move |session: String, preset: String| {
        applied_log.push(&Array::of2(
            &JsValue::from_str(&session),
            &JsValue::from_str(&preset),
        ));
    }) as Box<dyn FnMut(String, String)>);
    let face = create_agent_preset_seat_controller(
        property(&bench, "api"),
        currentReader(&bench),
        Some(on_applied.into_js_value().unchecked_into()),
    )
    .unwrap();
    invoke(&face, "load", &[]);
    settle().await;
    assert_eq!(
        property(&snapshot(&face, "agentPresetSeat"), "current")
            .as_string()
            .as_deref(),
        Some("standard")
    );
    invoke(&face, "select", &[JsValue::from_str("minimal")]);
    settle().await;
    assert_eq!(applied.length(), 0);
    setCurrent(
        &bench,
        &Object::from_entries(&Array::of3(
            &Array::of2(&JsValue::from_str("id"), &JsValue::from_str("session-1")),
            &Array::of2(&JsValue::from_str("blank"), &JsValue::TRUE),
            &Array::of2(
                &JsValue::from_str("agentPreset"),
                &JsValue::from_str("standard"),
            ),
        ))
        .unwrap()
        .into(),
    );
    invoke(&face, "apply", &[]);
    settle().await;
    invoke(&face, "apply", &[]);
    settle().await;
    assert_eq!(applied.length(), 1);
    assert_eq!(
        Array::from(&applied.get(0)).get(0).as_string().as_deref(),
        Some("session-1")
    );
    invoke(
        &face,
        "stage",
        &[JsValue::from_str("standard"), JsValue::TRUE],
    );
    assert_eq!(
        property(&snapshot(&face, "agentPresetSeat"), "introduce"),
        JsValue::TRUE
    );
    invoke(&face, "introduced", &[]);
    assert_eq!(
        property(&snapshot(&face, "agentPresetSeat"), "introduce"),
        JsValue::FALSE
    );
}

#[wasm_bindgen_test(async)]
async fn section_face_views_copies_reveals_deletes_and_writes_default() {
    let bench = makeAgentPresetApi();
    let changed = Array::new();
    let changed_log = changed.clone();
    let callback = Closure::wrap(Box::new(move || {
        changed_log.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let face = create_agent_preset_section_controller(
        property(&bench, "api"),
        Some(callback.into_js_value().unchecked_into()),
    )
    .unwrap();
    invoke(&face, "load", &[]);
    settle().await;
    let ready = snapshot(&face, "agentPresetSection");
    assert_eq!(
        property(&ready, "status").as_string().as_deref(),
        Some("ready")
    );
    assert_eq!(Array::from(&property(&ready, "rows")).length(), 3);

    invoke(&face, "view", &[JsValue::from_str("standard")]);
    settle().await;
    assert_eq!(
        property(
            &property(&snapshot(&face, "agentPresetSection"), "view"),
            "content"
        )
        .as_string()
        .as_deref(),
        Some("- id: tool-bash\n")
    );
    invoke(&face, "beginCopy", &[JsValue::from_str("standard")]);
    invoke(&face, "setCopyId", &[JsValue::from_str("my-copy")]);
    invoke(&face, "setCopyName", &[JsValue::from_str("  My copy  ")]);
    invoke(&face, "confirmCopy", &[]);
    settle().await;
    let copied = snapshot(&face, "agentPresetSection");
    assert_eq!(changed.length(), 1);
    assert_eq!(
        property(&property(&copied, "revealedPaths"), "my-copy")
            .as_string()
            .as_deref(),
        Some("/presets/my-copy")
    );
    invoke(&face, "confirmDelete", &[JsValue::from_str("my-copy")]);
    invoke(&face, "remove", &[]);
    settle().await;
    assert_eq!(changed.length(), 2);
    invoke(&face, "makeDefault", &[JsValue::from_str("minimal")]);
    settle().await;
    assert!(calls(&bench).iter().any(|entry| {
        let entry = Array::from(&entry);
        entry.get(0).as_string().as_deref() == Some("update")
            && property(&entry.get(1), "ns").as_string().as_deref() == Some("agent-presets")
    }));
}

#[wasm_bindgen_test(async)]
async fn public_helpers_preserve_draft_blockers_and_settings_only_writer_boundary() {
    let draft = js_sys::JSON::parse(
        r#"{"from":"standard","fromTitle":"Standard","id":"UPPER","name":"","saving":false,"error":null}"#,
    )
    .unwrap();
    assert_eq!(
        agent_preset_draft_blocker_js(draft, Array::new().into())
            .unwrap()
            .as_string()
            .as_deref(),
        Some("idInvalid")
    );
    let api = js_sys::JSON::parse(r#"{"settings":{}}"#).unwrap();
    let settings = property(&api, "settings");
    let update = Closure::wrap(Box::new(move |_request: JsValue| {
        Promise::resolve(&js_sys::JSON::parse(r#"{"result":{"ok":true,"value":{}}}"#).unwrap())
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    Reflect::set(
        &settings,
        &JsValue::from_str("update"),
        &update.into_js_value(),
    )
    .unwrap();
    assert!(
        JsFuture::from(write_agent_preset_default_js(api, "minimal".to_owned()))
            .await
            .unwrap()
            .is_undefined()
    );
}
