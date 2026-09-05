//! Live browser-boundary coverage for the compiled plugin and React controls.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Function, Map, Object, Promise, Reflect};
use seekdeep_client_ui_message_feedback::{
    WasmMessageFeedbackController, apply_client_ui_message_feedback,
    configure_client_ui_message_feedback, message_feedback_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const copy = {
  'action.like': 'Good response',
  'action.likeActive': 'Remove rating',
  'action.dislike': 'Bad response',
  'action.dislikeActive': 'Remove rating',
  'note.open': 'Add a note',
  'note.placeholder': 'What was good, or what went wrong? (optional)',
  'note.save': 'Save',
  'note.cancel': 'Cancel',
  'note.aria': 'Feedback note',
  'error.conflict': 'This feedback changed elsewhere; the latest state is shown',
  'error.load': 'Could not load feedback',
  'error.generic': 'Could not save feedback',
}

function makeReactHarness() {
  const slots = []
  const cleanups = []
  let cursor = 0
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useState(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = initial
      return [slots[index], update => {
        slots[index] = typeof update === 'function' ? update(slots[index]) : update
      }]
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = { current: initial }
      return slots[index]
    },
    useCallback(callback) { cursor += 1; return callback },
    useEffect(effect) {
      const index = cursor++
      if (!(index in slots)) {
        slots[index] = true
        const cleanup = effect()
        if (typeof cleanup === 'function') cleanups.push(cleanup)
      }
    },
  }
  return {
    React,
    reset() { cursor = 0 },
    unmount() { for (const cleanup of cleanups.splice(0).reverse()) cleanup() },
  }
}

export function makeFeedbackBench() {
  const react = makeReactHarness()
  const registrations = []
  const effects = []
  const listeners = new Map()
  const calls = []
  const dictionaries = []
  const styles = []
  globalThis.document = {
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(k, v) { this.attributes[k] = v } } },
    querySelector(selector) {
      const match = /^style\[data-plugin-css=(.+)\]$/.exec(selector)
      if (match === null) return null
      const tagId = JSON.parse(match[1])
      return styles.find(node => node.attributes['data-plugin-css'] === tagId) ?? null
    },
  }
  const carried = value => Promise.resolve({ ok: true, value })
  const seeded = {
    messageId: 'm-1', rating: 'positive', version: 'v1', createdAt: 1, updatedAt: 1,
  }
  const messageFeedback = {
    list(request) {
      calls.push({ method: 'list', request })
      return carried({ ok: true, value: { items: [seeded] } })
    },
    put(request) {
      calls.push({ method: 'put', request })
      return carried({ ok: true, value: { ...seeded, ...request, version: 'v2', createdAt: 1, updatedAt: 2 } })
    },
    delete(request) {
      calls.push({ method: 'delete', request })
      return carried({ ok: true, value: { absent: true } })
    },
  }
  const slots = {
    inject(name, install) {
      const dispose = install()
      effects.push(dispose)
      return dispose
    },
    register(options, component) {
      const entry = { options, component }
      registrations.push(entry)
      return () => {
        const index = registrations.indexOf(entry)
        if (index >= 0) registrations.splice(index, 1)
      }
    },
  }
  const locale = {
    register(namespace, values) {
      const entry = { namespace, values }
      dictionaries.push(entry)
      return () => {
        const index = dictionaries.indexOf(entry)
        if (index >= 0) dictionaries.splice(index, 1)
      }
    },
  }
  const services = { slots, remote: { messageFeedback }, locale }
  const ctx = {
    get(name) { return services[name] },
    effect(install) { const dispose = install(); effects.push(dispose); return dispose },
    on(name, listener) {
      let bucket = listeners.get(name)
      if (bucket === undefined) listeners.set(name, bucket = new Set())
      bucket.add(listener)
      const dispose = () => bucket.delete(listener)
      effects.push(dispose)
      return dispose
    },
  }
  const primitives = Object.fromEntries([
    'Tooltip', 'IconLikeOutline16', 'IconDislikeOutline16',
  ].map(name => [name, name]))
  return {
    ctx, React: react.React, react, primitives, registrations, calls, dictionaries, styles,
    emit(name) { for (const listener of listeners.get(name) ?? []) listener() },
    dispose() {
      for (const dispose of effects.splice(0).reverse()) dispose()
      react.unmount()
    },
  }
}

export function feedbackRegistration(bench) { return bench.registrations[0] }
export function feedbackInject(bench, sessionId) {
  return feedbackRegistration(bench).options.inject(sessionId)
}
export function feedbackCalls(bench, method) { return bench.calls.filter(call => call.method === method) }
export function feedbackEmitReset(bench) { bench.emit('connection/reset') }
export function feedbackDispose(bench) { bench.dispose() }
export function feedbackTick() { return Promise.resolve().then(() => Promise.resolve()) }

export function makeControllerRemote(mode) {
  const state = { mode }
  const seeded = {
    messageId: 'm-1', rating: 'positive', version: 'v1', createdAt: 1, updatedAt: 1,
  }
  const carried = value => Promise.resolve({ ok: true, value })
  const remote = {
    list() {
      if (state.mode === 'non-error-rejection') return Promise.reject('socket string')
      if (state.mode === 'carrier') {
        return Promise.resolve({ ok: false, error: { code: 'host-offline', message: 'Host offline' } })
      }
      if (state.mode === 'unknown') {
        return carried({ ok: false, error: {
          code: 'brand-new-code', futureDetail: { retained: true },
        } })
      }
      return carried({ ok: true, value: { items: [seeded] } })
    },
    put() {
      if (state.mode === 'mutation-non-error-rejection') return Promise.reject('wire string')
      if (state.mode === 'conflict') {
        return carried({ ok: false, error: {
          code: 'version-conflict',
          current: { ...seeded, rating: 'negative', version: 'v9', updatedAt: 9 },
        } })
      }
      return carried({ ok: true, value: { ...seeded, version: 'v2' } })
    },
    delete() { return carried({ ok: true, value: { absent: true } }) },
  }
  return { remote, state }
}
export function feedbackSetMode(bench, mode) { bench.state.mode = mode }

export function makeActionBench(options = {}) {
  const calls = []
  const item = options.current
  const view = {
    status: options.status ?? 'ready',
    items: new Map(item === undefined ? [] : [['m-1', item]]),
    error: null,
  }
  const result = options.result ?? { ok: true }
  const props = {
    messageId: 'm-1',
    ensure() { calls.push({ method: 'ensure', args: [] }); return Promise.resolve({ ok: true }) },
    rate(...args) { calls.push({ method: 'rate', args }); return Promise.resolve(result) },
    toggle(...args) { calls.push({ method: 'toggle', args }); return Promise.resolve(result) },
    clearNote(...args) { calls.push({ method: 'clearNote', args }); return Promise.resolve(result) },
    clear(...args) { calls.push({ method: 'clear', args }); return Promise.resolve(result) },
    useFeedback(selector) { return selector(view) },
    t(key) { return copy[key] ?? key },
  }
  return { props, calls, view }
}

export function feedbackRender(bench, actions) {
  bench.react.reset()
  return feedbackRegistration(bench).component(actions.props)
}

export function feedbackText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return (node.children ?? []).map(feedbackText).join('')
}

export function feedbackFind(node, property, value) {
  if (node === null || node === undefined || node === false) return undefined
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) {
    const found = feedbackFind(child, property, value)
    if (found !== undefined) return found
  }
  return undefined
}

export function feedbackFindText(node, text) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  for (const child of node.children ?? []) {
    if ((typeof child === 'string' || typeof child === 'number') && String(child) === text) return node
    const found = feedbackFindText(child, text)
    if (found !== undefined) return found
  }
  if (feedbackText(node) === text) return node
  return undefined
}

export function feedbackInvoke(node, property, argument) {
  return argument === undefined ? node.props[property]() : node.props[property](argument)
}
export function feedbackActionCalls(actions, method) { return actions.calls.filter(call => call.method === method) }
export function feedbackProperty(value, key) { return value?.[key] }
export function feedbackCurrent(overrides = {}) {
  return { messageId: 'm-1', rating: 'positive', version: 'v1', createdAt: 1, updatedAt: 1, ...overrides }
}
"#)]
extern "C" {
    fn makeFeedbackBench() -> JsValue;
    fn feedbackRegistration(bench: &JsValue) -> JsValue;
    fn feedbackInject(bench: &JsValue, session_id: &str) -> JsValue;
    fn feedbackCalls(bench: &JsValue, method: &str) -> Array;
    fn feedbackEmitReset(bench: &JsValue);
    fn feedbackDispose(bench: &JsValue);
    fn feedbackTick() -> Promise;
    fn makeControllerRemote(mode: &str) -> JsValue;
    fn feedbackSetMode(bench: &JsValue, mode: &str);
    fn makeActionBench(options: &JsValue) -> JsValue;
    fn feedbackRender(bench: &JsValue, actions: &JsValue) -> JsValue;
    fn feedbackFind(node: &JsValue, property: &str, value: &JsValue) -> JsValue;
    fn feedbackFindText(node: &JsValue, text: &str) -> JsValue;
    fn feedbackInvoke(node: &JsValue, property: &str, argument: &JsValue) -> JsValue;
    fn feedbackActionCalls(actions: &JsValue, method: &str) -> Array;
    fn feedbackProperty(value: &JsValue, key: &str) -> JsValue;
    fn feedbackCurrent(overrides: &JsValue) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).expect("property")
}

fn options(entries: &[(&str, JsValue)]) -> JsValue {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry).unwrap();
    }
    value.into()
}

async fn await_value(value: JsValue) -> JsValue {
    JsFuture::from(Promise::resolve(&value)).await.unwrap()
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn controller_boundary_preserves_frozen_snapshots_conflicts_unknown_codes_and_rejections() {
    let bench = makeControllerRemote("success");
    let controller = WasmMessageFeedbackController::new(property(&bench, "remote"), "s1".into());
    let cold = controller.get_snapshot().unwrap();
    assert_eq!(
        property(&cold, "status").as_string().as_deref(),
        Some("cold")
    );
    assert!(Object::is_frozen(&Object::from(cold.clone())));
    assert!(Object::is(&cold, &controller.get_snapshot().unwrap()));
    let loaded = await_value(controller.ensure().into()).await;
    assert_eq!(property(&loaded, "ok").as_bool(), Some(true));
    let loaded_again = await_value(controller.ensure().into()).await;
    assert!(Object::is(&loaded, &loaded_again));
    assert!(Object::is_frozen(&Object::from(loaded.clone())));
    let ready = controller.get_snapshot().unwrap();
    assert!(!Object::is(&cold, &ready));
    assert!(Object::is(&ready, &controller.get_snapshot().unwrap()));
    let items = property(&ready, "items").unchecked_into::<Map>();
    let item = items.get(&JsValue::from_str("m-1"));
    assert_eq!(property(&item, "createdAt").as_f64(), Some(1.0));
    assert!(Object::is_frozen(&Object::from(item)));

    let notifications = Rc::new(Cell::new(0_u32));
    let count = notifications.clone();
    let listener = Closure::wrap(Box::new(move || count.set(count.get() + 1)) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into::<Function>();
    let dispose_first = controller.subscribe(listener.clone());
    let _dispose_duplicate = controller.subscribe(listener);
    await_value(controller.refresh().into()).await;
    assert_eq!(notifications.get(), 2);
    dispose_first.call0(&JsValue::UNDEFINED).unwrap();
    await_value(controller.refresh().into()).await;
    assert_eq!(notifications.get(), 2);

    feedbackSetMode(&bench, "mutation-non-error-rejection");
    let rejected = await_value(
        controller
            .rate("m-1".into(), "negative".into(), JsValue::UNDEFINED)
            .into(),
    )
    .await;
    assert_eq!(
        property(&property(&rejected, "error"), "message")
            .as_string()
            .as_deref(),
        Some("message feedback mutation failed")
    );
    let items = property(&controller.get_snapshot().unwrap(), "items").unchecked_into::<Map>();
    assert_eq!(
        property(&items.get(&JsValue::from_str("m-1")), "version")
            .as_string()
            .as_deref(),
        Some("v1")
    );

    feedbackSetMode(&bench, "conflict");
    let conflict = await_value(
        controller
            .rate("m-1".into(), "negative".into(), JsValue::UNDEFINED)
            .into(),
    )
    .await;
    assert_eq!(property(&conflict, "ok").as_bool(), Some(false));
    assert_eq!(
        property(&property(&conflict, "error"), "code")
            .as_string()
            .as_deref(),
        Some("version-conflict")
    );
    let items = property(&controller.get_snapshot().unwrap(), "items").unchecked_into::<Map>();
    assert_eq!(
        property(&items.get(&JsValue::from_str("m-1")), "version")
            .as_string()
            .as_deref(),
        Some("v9")
    );

    controller.dispose();
    let disposed = await_value(
        controller
            .rate("m-1".into(), "positive".into(), JsValue::UNDEFINED)
            .into(),
    )
    .await;
    let disposed_again = await_value(
        controller
            .rate("m-1".into(), "positive".into(), JsValue::UNDEFINED)
            .into(),
    )
    .await;
    assert!(Object::is(&disposed, &disposed_again));
    assert!(Object::is_frozen(&Object::from(disposed.clone())));
    assert!(Object::is_frozen(&Object::from(property(
        &disposed, "error"
    ))));

    for (mode, code, message) in [
        ("unknown", "brand-new-code", "brand-new-code"),
        ("carrier", "host-offline", "Host offline"),
        (
            "non-error-rejection",
            "transport",
            "message feedback list failed",
        ),
    ] {
        let bench = makeControllerRemote(mode);
        let controller =
            WasmMessageFeedbackController::new(property(&bench, "remote"), "s2".into());
        let result = await_value(controller.ensure().into()).await;
        let error = property(&result, "error");
        assert_eq!(property(&error, "code").as_string().as_deref(), Some(code));
        assert_eq!(
            property(&error, "message").as_string().as_deref(),
            Some(message)
        );
        assert_eq!(
            property(&controller.get_snapshot().unwrap(), "error")
                .as_string()
                .as_deref(),
            Some(message)
        );
    }
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn plugin_owns_exact_registration_shared_session_faces_resync_and_disposal() {
    let bench = makeFeedbackBench();
    configure_client_ui_message_feedback(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    configure_client_ui_message_feedback(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    apply_client_ui_message_feedback(property(&bench, "ctx")).unwrap();
    assert_eq!(
        message_feedback_inject().to_vec(),
        ["slots", "remote", "remote.messageFeedback", "locale"]
            .into_iter()
            .map(JsValue::from_str)
            .collect::<Vec<_>>()
    );
    let registration = feedbackRegistration(&bench);
    let registration_options = property(&registration, "options");
    assert_eq!(
        property(&registration_options, "name")
            .as_string()
            .as_deref(),
        Some("conversation.chat.assistant-actions")
    );
    assert_eq!(
        property(&registration_options, "id").as_string().as_deref(),
        Some("feedback")
    );
    assert_eq!(
        property(&registration_options, "order").as_f64(),
        Some(10.0)
    );
    assert_eq!(
        property(&registration_options, "locale")
            .as_string()
            .as_deref(),
        Some("feedback")
    );
    assert_eq!(Array::from(&property(&bench, "dictionaries")).length(), 1);
    assert_eq!(Array::from(&property(&bench, "styles")).length(), 1);
    let style = Array::from(&property(&bench, "styles")).get(0);
    let attributes = property(&style, "attributes");
    assert_eq!(
        property(&attributes, "data-plugin").as_string().as_deref(),
        Some("@seekdeep-ai/seekdeep-client-ui-message-feedback")
    );
    assert_eq!(
        property(&attributes, "data-plugin-css")
            .as_string()
            .as_deref(),
        Some("@seekdeep-ai/seekdeep-client-ui-message-feedback/MessageFeedbackActions.module.css")
    );

    let first = feedbackInject(&bench, "s1");
    let second = feedbackInject(&bench, "s1");
    let other = feedbackInject(&bench, "s2");
    let first_controller = property(&property(&first, "hooks"), "feedback");
    assert!(Object::is(
        &first_controller,
        &property(&property(&second, "hooks"), "feedback")
    ));
    assert!(!Object::is(
        &first_controller,
        &property(&property(&other, "hooks"), "feedback")
    ));
    await_value(
        property(&first, "ensure")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&JsValue::UNDEFINED)
            .unwrap(),
    )
    .await;
    await_value(
        property(&second, "ensure")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&JsValue::UNDEFINED)
            .unwrap(),
    )
    .await;
    assert_eq!(feedbackCalls(&bench, "list").length(), 1);
    await_value(
        property(&other, "ensure")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&JsValue::UNDEFINED)
            .unwrap(),
    )
    .await;
    assert_eq!(feedbackCalls(&bench, "list").length(), 2);
    let cold = feedbackInject(&bench, "cold");
    assert!(!property(&property(&cold, "hooks"), "feedback").is_undefined());
    feedbackEmitReset(&bench);
    JsFuture::from(feedbackTick()).await.unwrap();
    assert_eq!(feedbackCalls(&bench, "list").length(), 4);
    let last = property(&feedbackCalls(&bench, "list").get(3), "request");
    assert_eq!(
        property(&last, "sessionId").as_string().as_deref(),
        Some("s2")
    );

    let rate = property(&first, "rate").dyn_into::<Function>().unwrap();
    await_value(
        rate.call3(
            &JsValue::UNDEFINED,
            &JsValue::from_str("m-1"),
            &JsValue::from_str("negative"),
            &JsValue::from_str("wrong answer"),
        )
        .unwrap(),
    )
    .await;
    assert_eq!(feedbackCalls(&bench, "put").length(), 1);
    let request = property(&feedbackCalls(&bench, "put").get(0), "request");
    assert_eq!(
        property(&request, "sessionId").as_string().as_deref(),
        Some("s1")
    );
    assert_eq!(
        property(&request, "note").as_string().as_deref(),
        Some("wrong answer")
    );

    feedbackDispose(&bench);
    assert!(feedbackRegistration(&bench).is_undefined());
    let result = await_value(
        rate.call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str("m-1"),
            &JsValue::from_str("positive"),
        )
        .unwrap(),
    )
    .await;
    assert!(!property(&result, "ok").as_bool().unwrap());
    assert_eq!(
        property(&property(&result, "error"), "code")
            .as_string()
            .as_deref(),
        Some("disposed")
    );
    assert_eq!(feedbackCalls(&bench, "put").length(), 1);

    apply_client_ui_message_feedback(property(&bench, "ctx")).unwrap();
    assert!(!feedbackRegistration(&bench).is_undefined());
    assert_eq!(Array::from(&property(&bench, "styles")).length(), 1);
    feedbackDispose(&bench);
}

#[wasm_bindgen_test]
fn controls_render_rating_seed_once_and_surface_load_failure() {
    let bench = makeFeedbackBench();
    configure_client_ui_message_feedback(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    apply_client_ui_message_feedback(property(&bench, "ctx")).unwrap();
    let actions = makeActionBench(&options(&[("status", JsValue::from_str("error"))]));
    let tree = feedbackRender(&bench, &actions);
    let like = feedbackFind(&tree, "aria-label", &JsValue::from_str("Good response"));
    let dislike = feedbackFind(&tree, "aria-label", &JsValue::from_str("Bad response"));
    assert_eq!(
        property(&like, "kind").as_string().as_deref(),
        Some("button")
    );
    assert_eq!(
        property(&property(&like, "props"), "aria-pressed").as_bool(),
        Some(false)
    );
    feedbackInvoke(&like, "onPointerEnter", &JsValue::UNDEFINED);
    feedbackInvoke(&like, "onPointerEnter", &JsValue::UNDEFINED);
    feedbackInvoke(&dislike, "onFocus", &JsValue::UNDEFINED);
    assert_eq!(feedbackActionCalls(&actions, "ensure").length(), 1);
    assert!(!feedbackFindText(&tree, "Could not load feedback").is_undefined());
    feedbackInvoke(&like, "onClick", &JsValue::UNDEFINED);
    assert_eq!(feedbackActionCalls(&actions, "toggle").length(), 1);
    let toggle = feedbackActionCalls(&actions, "toggle").get(0);
    let args = Array::from(&feedbackProperty(&toggle, "args"));
    assert_eq!(args.get(0).as_string().as_deref(), Some("m-1"));
    assert_eq!(args.get(1).as_string().as_deref(), Some("positive"));

    let current = feedbackCurrent(&options(&[("rating", JsValue::from_str("negative"))]));
    let actions = makeActionBench(&options(&[("current", current)]));
    let tree = feedbackRender(&bench, &actions);
    let dislike = feedbackFind(&tree, "aria-label", &JsValue::from_str("Remove rating"));
    assert_eq!(
        property(&property(&dislike, "props"), "aria-pressed").as_bool(),
        Some(true)
    );
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn controls_trim_save_clear_notes_keep_failed_editor_and_prefer_action_error() {
    let bench = makeFeedbackBench();
    configure_client_ui_message_feedback(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    apply_client_ui_message_feedback(property(&bench, "ctx")).unwrap();
    let current = feedbackCurrent(&options(&[("note", JsValue::from_str("old note"))]));
    let actions = makeActionBench(&options(&[("current", current)]));
    let tree = feedbackRender(&bench, &actions);
    let open = feedbackFindText(&tree, "old note");
    feedbackInvoke(&open, "onClick", &JsValue::UNDEFINED);
    let tree = feedbackRender(&bench, &actions);
    let textarea = feedbackFind(&tree, "aria-label", &JsValue::from_str("Feedback note"));
    assert_eq!(
        property(&property(&textarea, "props"), "value")
            .as_string()
            .as_deref(),
        Some("old note")
    );
    let target = options(&[("value", JsValue::from_str("  precise and short  "))]);
    let event = options(&[("target", target)]);
    feedbackInvoke(&textarea, "onChange", &event);
    let tree = feedbackRender(&bench, &actions);
    let save = feedbackFindText(&tree, "Save");
    feedbackInvoke(&save, "onClick", &JsValue::UNDEFINED);
    JsFuture::from(feedbackTick()).await.unwrap();
    let rates = feedbackActionCalls(&actions, "rate");
    assert_eq!(rates.length(), 1);
    let args = Array::from(&feedbackProperty(&rates.get(0), "args"));
    assert_eq!(args.get(0).as_string().as_deref(), Some("m-1"));
    assert_eq!(args.get(1).as_string().as_deref(), Some("positive"));
    assert_eq!(
        args.get(2).as_string().as_deref(),
        Some("precise and short")
    );
    let closed = feedbackRender(&bench, &actions);
    assert!(
        feedbackFind(&closed, "aria-label", &JsValue::from_str("Feedback note")).is_undefined()
    );

    let failure = options(&[
        ("ok", JsValue::FALSE),
        (
            "error",
            options(&[
                ("code", JsValue::from_str("note-too-large")),
                ("message", JsValue::from_str("too long")),
            ]),
        ),
    ]);
    let current = feedbackCurrent(&Object::new().into());
    let actions = makeActionBench(&options(&[
        ("current", current),
        ("status", JsValue::from_str("error")),
        ("result", failure),
    ]));
    let tree = feedbackRender(&bench, &actions);
    feedbackInvoke(
        &feedbackFindText(&tree, "Add a note"),
        "onClick",
        &JsValue::UNDEFINED,
    );
    let tree = feedbackRender(&bench, &actions);
    feedbackInvoke(
        &feedbackFindText(&tree, "Save"),
        "onClick",
        &JsValue::UNDEFINED,
    );
    JsFuture::from(feedbackTick()).await.unwrap();
    assert_eq!(feedbackActionCalls(&actions, "clearNote").length(), 1);
    let tree = feedbackRender(&bench, &actions);
    assert!(!feedbackFind(&tree, "aria-label", &JsValue::from_str("Feedback note")).is_undefined());
    assert!(!feedbackFindText(&tree, "Could not save feedback").is_undefined());
    assert!(feedbackFindText(&tree, "Could not load feedback").is_undefined());

    let conflict = options(&[
        ("ok", JsValue::FALSE),
        (
            "error",
            options(&[
                ("code", JsValue::from_str("version-conflict")),
                ("message", JsValue::from_str("feedback changed elsewhere")),
            ]),
        ),
    ]);
    let actions = makeActionBench(&options(&[("result", conflict)]));
    let tree = feedbackRender(&bench, &actions);
    feedbackInvoke(
        &feedbackFind(&tree, "aria-label", &JsValue::from_str("Good response")),
        "onClick",
        &JsValue::UNDEFINED,
    );
    JsFuture::from(feedbackTick()).await.unwrap();
    let tree = feedbackRender(&bench, &actions);
    assert!(
        !feedbackFindText(
            &tree,
            "This feedback changed elsewhere; the latest state is shown"
        )
        .is_undefined()
    );

    let current = feedbackCurrent(&options(&[("note", JsValue::from_str("old note"))]));
    let actions = makeActionBench(&options(&[("current", current)]));
    let tree = feedbackRender(&bench, &actions);
    feedbackInvoke(
        &feedbackFindText(&tree, "old note"),
        "onClick",
        &JsValue::UNDEFINED,
    );
    let tree = feedbackRender(&bench, &actions);
    feedbackInvoke(
        &feedbackFindText(&tree, "Cancel"),
        "onClick",
        &JsValue::UNDEFINED,
    );
    let tree = feedbackRender(&bench, &actions);
    assert!(feedbackFind(&tree, "aria-label", &JsValue::from_str("Feedback note")).is_undefined());
    assert_eq!(feedbackActionCalls(&actions, "rate").length(), 0);
}
