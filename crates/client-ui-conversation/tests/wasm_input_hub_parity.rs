//! Live WASM coverage for the session-addressed input hub.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::BrowserInputHub;
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let bench

function deferred() {
  let resolve
  let reject
  const promise = new Promise((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

export function hubSetup() {
  bench = {
    listeners: new Map(), cleanups: [], effectLabels: [], offCount: 0,
    rootGets: [], sessionCalls: [], sends: [], sendMode: 'resolve', sendPending: [],
    releasedImages: [], updateCalls: [], updateResults: [], queueListeners: [],
    tracks: [], popupDismisses: 0,
  }
  const actx = {
    id: 's1',
    on(name, listener) {
      const rows = bench.listeners.get(name) ?? []
      const row = { listener, live: true }
      rows.push(row)
      bench.listeners.set(name, rows)
      return () => { if (row.live) { row.live = false; bench.offCount += 1 } }
    },
    effect(setup, label) {
      bench.effectLabels.push(label)
      const cleanup = setup()
      bench.cleanups.push(cleanup)
      return cleanup
    },
  }
  const orphan = { id: 'root' }
  const session = {
    sessionId: 's1',
    snapshot: { queue: [] },
    getSnapshot() { bench.sessionCalls.push({ method: 'getSnapshot', receiver: this }); return this.snapshot },
    subscribe(listener) {
      bench.sessionCalls.push({ method: 'subscribe', receiver: this })
      bench.queueListeners.push(listener)
      return () => {
        const at = bench.queueListeners.indexOf(listener)
        if (at >= 0) bench.queueListeners.splice(at, 1)
      }
    },
    updateQueue(id, update) {
      bench.updateCalls.push({ id, update, receiver: this })
      const result = bench.updateResults.shift() ?? { ok: true }
      return Promise.resolve(result)
    },
  }
  const binding = { sessionId: 's1', session, ctx: actx }
  const sessions = {
    scopeOf(ctx) { bench.sessionsReceiver = this; return ctx === actx ? 's1' : undefined },
    binding(id) { bench.sessionsReceiver = this; return id === 's1' ? binding : undefined },
    scope(id) { bench.sessionsReceiver = this; return id === 's1' ? actx : undefined },
  }
  const controller = {
    lexicon: { getSnapshot() { return new Map() }, subscribe() { return () => {} } },
    track(draft, caret, guard, draftRev) { bench.tracks.push({ draft, caret, guard, draftRev }) },
    arbitrate() { return 'pass' }, onSpace() { return false },
    adjudicate() { return Promise.resolve(undefined) },
    serializeReference(source, ref) { return Promise.resolve(`<${source}:${ref}>`) },
  }
  const inputTriggers = {
    sessionOf(ctx) { bench.controllerReceiver = this; return ctx === actx ? controller : undefined },
  }
  const popup = { dismiss() { bench.popupDismisses += 1 } }
  const commandUi = { popupFor(ctx) { bench.popupReceiver = this; return ctx === actx ? popup : undefined } }
  const conversation = {
    sendSession(sentSession, text, imageIds, mode) {
      bench.sends.push({ session: sentSession, text, imageIds, mode, receiver: this })
      if (bench.sendMode === 'resolve') return Promise.resolve()
      if (bench.sendMode === 'reject') return Promise.reject(new Error('prompt failed'))
      const pending = deferred()
      bench.sendPending.push(pending)
      return pending.promise
    },
    releaseDraftImage(id) { bench.releasedImages.push({ id, receiver: this }) },
  }
  const services = new Map([
    ['sessions', sessions], ['inputTriggers', inputTriggers],
    ['commandUi', commandUi], ['conversation', conversation],
  ])
  const root = {
    get(name) { bench.rootGets.push({ name, receiver: this }); return services.get(name) },
  }
  const translate = key => `translated ${key}`
  Object.assign(bench, {
    actx, orphan, session, binding, sessions, controller, inputTriggers,
    popup, commandUi, conversation, services, root, translate,
  })
  return bench
}

export function hubBench() { return bench }
export function hubProduce() {
  return (base, recipe) => {
    const draft = Array.isArray(base) ? [...base] : { ...base }
    recipe(draft)
    return draft
  }
}
export function hubObject(entries) { return Object.fromEntries(entries) }
export function hubClaim(token) {
  return { token, submit() { return Promise.resolve({ kind: 'success' }) } }
}
export function hubEmit(name, request) {
  const row = (bench.listeners.get(name) ?? []).find(candidate => candidate.live)
  return row?.listener(request)
}
export function hubCleanup(index) { return bench.cleanups[index]() }
export function hubSetService(name, value) {
  if (value === undefined) bench.services.delete(name)
  else bench.services.set(name, value)
}
export function hubSetSendMode(mode) { bench.sendMode = mode }
export function hubRejectSend(index) { bench.sendPending[index].reject(new Error('prompt failed')) }
export function hubResolveSend(index) { bench.sendPending[index].resolve() }
export function hubSetQueue(rows, results) {
  bench.session.snapshot = { ...bench.session.snapshot, queue: rows }
  bench.updateResults = [...results]
  for (const listener of [...bench.queueListeners]) listener()
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = hubSetup)]
    fn hub_setup() -> JsValue;
    #[wasm_bindgen(js_name = hubBench)]
    fn hub_bench() -> JsValue;
    #[wasm_bindgen(js_name = hubProduce)]
    fn hub_produce() -> Function;
    #[wasm_bindgen(js_name = hubObject)]
    fn hub_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = hubClaim)]
    fn hub_claim(token: &str) -> JsValue;
    #[wasm_bindgen(js_name = hubEmit)]
    fn hub_emit(name: &str, request: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = hubCleanup)]
    fn hub_cleanup(index: u32) -> JsValue;
    #[wasm_bindgen(js_name = hubSetService)]
    fn hub_set_service(name: &str, value: JsValue);
    #[wasm_bindgen(js_name = hubSetSendMode)]
    fn hub_set_send_mode(mode: &str);
    #[wasm_bindgen(js_name = hubRejectSend)]
    fn hub_reject_send(index: u32);
    #[wasm_bindgen(js_name = hubResolveSend)]
    fn hub_resolve_send(index: u32);
    #[wasm_bindgen(js_name = hubSetQueue)]
    fn hub_set_queue(rows: &Array, results: &Array);
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key))
        .unwrap_or_else(|error| panic!("property {key:?} on {value:?} failed: {error:?}"))
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    hub_object(&array).unchecked_into()
}

fn strings(values: &[&str]) -> Array {
    values
        .iter()
        .map(|value| JsValue::from_str(value))
        .collect()
}

fn call(target: &JsValue, method: &str, arguments: &[JsValue]) -> JsValue {
    let function = property(target, method).dyn_into::<Function>().unwrap();
    let arguments: Array = arguments.iter().collect();
    function.apply(target, &arguments).unwrap()
}

fn snapshot(shell: &JsValue) -> JsValue {
    property(shell, "snapshot")
}

fn store_snapshot(store: &JsValue) -> JsValue {
    call(store, "getSnapshot", &[])
}

fn entries(key: &str) -> Array {
    property(&hub_bench(), key).unchecked_into()
}

fn array_strings(value: &JsValue) -> Vec<String> {
    value
        .clone()
        .unchecked_into::<Array>()
        .iter()
        .map(|value| value.as_string().unwrap())
        .collect()
}

fn span(start: u32, end: u32, draft_rev: f64) -> Object {
    object(&[
        ("start", JsValue::from_f64(f64::from(start))),
        ("end", JsValue::from_f64(f64::from(end))),
        ("draftRev", JsValue::from_f64(draft_rev)),
    ])
}

fn error_message(error: &JsValue) -> String {
    property(error, "message").as_string().unwrap()
}

fn setup() -> (BrowserInputHub, JsValue) {
    seekdeep_client_runtime::install_store_produce(hub_produce());
    let bench = hub_setup();
    let hub = BrowserInputHub::new(
        property(&bench, "root"),
        property(&bench, "translate")
            .dyn_into::<Function>()
            .unwrap(),
    );
    (hub, bench)
}

async fn flush_microtasks() {
    for _ in 0..10 {
        JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
            .await
            .unwrap();
    }
}

#[allow(clippy::too_many_lines)] // Scope resolution, four listeners, and teardown are one lifecycle.
fn scoped_resolution_listener_bails_and_teardown_preserve_one_shell_identity() {
    let (hub, bench) = setup();
    let binding = property(&bench, "binding");
    let actx = property(&bench, "actx");
    let shell = hub.shell_for(binding.clone()).unwrap();
    assert!(Object::is(&shell, &hub.shell_for(binding.clone()).unwrap()));
    assert!(Object::is(&shell, &hub.for_context(actx.clone()).unwrap()));
    assert!(Object::is(&shell, &hub.shell("s1".to_owned()).unwrap()));
    assert!(Object::is(&shell, &hub.keyboard("s1".to_owned()).unwrap()));
    assert_eq!(entries("effectLabels").length(), 1);
    assert_eq!(
        entries("effectLabels").get(0).as_string().as_deref(),
        Some("conversation.input: session shell")
    );
    let listeners = property(&bench, "listeners");
    assert_eq!(property(&listeners, "size").as_f64(), Some(4.0));
    for event in [
        "slash/input-begin-command",
        "slash/input-insert-reference",
        "slash/input-consume-token",
        "slash/input-insert-text",
    ] {
        assert_eq!(
            call(&listeners, "has", &[JsValue::from_str(event)]).as_bool(),
            Some(true),
            "{event}"
        );
    }

    call(&shell, "setDraft", &[JsValue::from_str("/go")]);
    let revision = property(&snapshot(&shell), "draftRev").as_f64().unwrap();
    let accepted = hub_emit(
        "slash/input-begin-command",
        object(&[
            ("claim", hub_claim("/goal ")),
            ("span", span(0, 3, revision).into()),
        ])
        .as_ref(),
    );
    assert_eq!(accepted.as_bool(), Some(true));

    call(&shell, "setDraft", &[JsValue::from_str("@item")]);
    let revision = property(&snapshot(&shell), "draftRev").as_f64().unwrap();
    let accepted = hub_emit(
        "slash/input-insert-reference",
        object(&[
            (
                "reference",
                object(&[
                    ("source", JsValue::from_str("skills")),
                    ("ref", JsValue::from_str("item")),
                    ("label", JsValue::from_str("/item")),
                    ("clipboardText", JsValue::from_str("/item")),
                ])
                .into(),
            ),
            ("span", span(0, 5, revision).into()),
        ])
        .as_ref(),
    );
    assert_eq!(accepted.as_bool(), Some(true));

    call(&shell, "setDraft", &[JsValue::from_str("/token")]);
    let accepted = hub_emit(
        "slash/input-consume-token",
        object(&[(
            "guard",
            object(&[
                ("kind", JsValue::from_str("bare-token")),
                ("token", JsValue::from_str("/token")),
            ])
            .into(),
        )])
        .as_ref(),
    );
    assert_eq!(accepted.as_bool(), Some(true));

    call(&shell, "setDraft", &[JsValue::from_str("@x")]);
    let revision = property(&snapshot(&shell), "draftRev").as_f64().unwrap();
    let accepted = hub_emit(
        "slash/input-insert-text",
        object(&[
            ("text", JsValue::from_str("/literal ")),
            ("span", span(0, 2, revision).into()),
        ])
        .as_ref(),
    );
    assert_eq!(accepted.as_bool(), Some(true));
    assert_eq!(
        property(&snapshot(&shell), "draft").as_string().as_deref(),
        Some("/literal ")
    );

    assert!(Object::is(
        &hub.input_triggers("s1".to_owned()).unwrap(),
        &property(&bench, "controller")
    ));
    assert!(
        hub.input_triggers("missing".to_owned())
            .unwrap()
            .is_undefined()
    );
    call(
        &shell,
        "track",
        &[JsValue::from_str("draft"), JsValue::from_f64(2.0)],
    );
    call(&shell, "dismissPopup", &[]);
    assert_eq!(entries("tracks").length(), 1);
    assert_eq!(property(&bench, "popupDismisses").as_f64(), Some(1.0));
    assert!(Object::is(
        &property(&bench, "controllerReceiver"),
        &property(&bench, "inputTriggers")
    ));
    assert!(Object::is(
        &property(&bench, "popupReceiver"),
        &property(&bench, "commandUi")
    ));
    call(&shell, "addImages", &[strings(&["draft-image"]).into()]);
    hub_cleanup(0);
    assert_eq!(property(&bench, "offCount").as_f64(), Some(4.0));
    assert_eq!(
        property(&entries("releasedImages").get(0), "id")
            .as_string()
            .as_deref(),
        Some("draft-image")
    );
    let reborn = hub.shell_for(binding).unwrap();
    assert!(!Object::is(&shell, &reborn));
    assert_eq!(entries("effectLabels").length(), 2);
}

fn missing_scope_binding_and_sessions_fail_at_the_source_boundaries() {
    let (hub, bench) = setup();
    let error = hub.for_context(property(&bench, "orphan")).unwrap_err();
    assert_eq!(
        error_message(&error),
        "conversation.input.for requires a session scope"
    );
    let error = hub.shell("missing".to_owned()).unwrap_err();
    assert_eq!(
        error_message(&error),
        "conversation.input: session \"missing\" resolved no binding"
    );
    hub_set_service("sessions", JsValue::UNDEFINED);
    let error = hub.shell("s1".to_owned()).unwrap_err();
    assert_eq!(
        error_message(&error),
        "conversation.input: sessions service unavailable"
    );
}

#[allow(clippy::too_many_lines)] // Commit, rollback, drift, and stale-shell ownership stay one send matrix.
async fn sink_commits_then_rolls_back_only_the_resident_untouched_shell() {
    let (hub, bench) = setup();
    let shell = hub.shell_for(property(&bench, "binding")).unwrap();
    hub_set_send_mode("pending");
    call(&shell, "setDraft", &[JsValue::from_str(" hello ")]);
    call(&shell, "addImages", &[strings(&["i1"]).into()]);
    call(&shell, "submit", &[JsValue::from_str("steer")]);
    assert_eq!(entries("sends").length(), 1);
    let send = entries("sends").get(0);
    assert_eq!(
        property(&send, "text").as_string().as_deref(),
        Some("hello")
    );
    assert_eq!(
        property(&send, "mode").as_string().as_deref(),
        Some("steer")
    );
    assert!(Object::is(
        &property(&send, "receiver"),
        &property(&bench, "conversation")
    ));
    assert_eq!(
        property(&snapshot(&shell), "draft").as_string().as_deref(),
        Some("")
    );
    assert_eq!(
        array_strings(&property(&snapshot(&shell), "imageIds")),
        Vec::<String>::new()
    );
    call(&shell, "setDraft", &[JsValue::from_str("newer")]);
    hub_reject_send(0);
    flush_microtasks().await;
    assert_eq!(
        property(&snapshot(&shell), "draft").as_string().as_deref(),
        Some("newer")
    );
    assert_eq!(
        array_strings(&property(&snapshot(&shell), "imageIds")),
        ["i1"]
    );

    let (hub, bench) = setup();
    let shell = hub.shell_for(property(&bench, "binding")).unwrap();
    hub_set_send_mode("pending");
    call(&shell, "setDraft", &[JsValue::from_str("restore me")]);
    call(&shell, "addImages", &[strings(&["i2"]).into()]);
    call(&shell, "submit", &[]);
    hub_reject_send(0);
    flush_microtasks().await;
    assert_eq!(
        property(&snapshot(&shell), "draft").as_string().as_deref(),
        Some("restore me")
    );
    assert_eq!(
        array_strings(&property(&snapshot(&shell), "imageIds")),
        ["i2"]
    );

    let (hub, bench) = setup();
    let binding = property(&bench, "binding");
    let old = hub.shell_for(binding.clone()).unwrap();
    hub_set_send_mode("pending");
    call(&old, "setDraft", &[JsValue::from_str("stale")]);
    call(&old, "addImages", &[strings(&["old-image"]).into()]);
    call(&old, "submit", &[]);
    hub_cleanup(0);
    let current = hub.shell_for(binding).unwrap();
    call(&current, "setDraft", &[JsValue::from_str("current")]);
    hub_reject_send(0);
    flush_microtasks().await;
    assert_eq!(
        property(&snapshot(&current), "draft")
            .as_string()
            .as_deref(),
        Some("current")
    );
    assert_eq!(
        property(&entries("releasedImages").get(0), "id")
            .as_string()
            .as_deref(),
        Some("old-image")
    );
}

#[allow(clippy::too_many_lines)] // FIFO, both convergence codes, and genuine failure form one matrix.
async fn queue_steering_is_fifo_queued_only_immediate_and_convergent() {
    let (hub, bench) = setup();
    let shell = hub.shell_for(property(&bench, "binding")).unwrap();
    let rows = Array::new();
    rows.push(&object(&[
        ("id", JsValue::from_str("q1")),
        ("placement", JsValue::from_str("queued")),
    ]));
    rows.push(&object(&[
        ("id", JsValue::from_str("skip")),
        ("placement", JsValue::from_str("steering")),
    ]));
    rows.push(&object(&[
        ("id", JsValue::from_str("q2")),
        ("placement", JsValue::from_str("queued")),
    ]));
    hub_set_queue(
        &rows,
        &Array::of2(
            &object(&[("ok", JsValue::TRUE)]),
            &object(&[("ok", JsValue::TRUE)]),
        ),
    );
    call(&shell, "steerQueue", &[]);
    assert_eq!(entries("updateCalls").length(), 1);
    assert_eq!(
        property(&entries("updateCalls").get(0), "id")
            .as_string()
            .as_deref(),
        Some("q1")
    );
    flush_microtasks().await;
    assert_eq!(entries("updateCalls").length(), 2);
    assert_eq!(
        property(&entries("updateCalls").get(1), "id")
            .as_string()
            .as_deref(),
        Some("q2")
    );
    assert_eq!(
        property(&property(&entries("updateCalls").get(0), "update"), "kind")
            .as_string()
            .as_deref(),
        Some("steer")
    );

    let stop_rows = Array::of2(
        &object(&[
            ("id", JsValue::from_str("stop")),
            ("placement", JsValue::from_str("queued")),
        ]),
        &object(&[
            ("id", JsValue::from_str("never")),
            ("placement", JsValue::from_str("queued")),
        ]),
    );
    let failure = object(&[
        ("ok", JsValue::FALSE),
        (
            "error",
            object(&[("code", JsValue::from_str("steer-unavailable"))]).into(),
        ),
    ]);
    hub_set_queue(&stop_rows, &Array::of1(&failure));
    call(&shell, "steerQueue", &[]);
    flush_microtasks().await;
    assert_eq!(entries("updateCalls").length(), 3);
    assert!(store_snapshot(&property(&shell, "notices")).is_null());

    let claimed_rows = Array::of1(&object(&[
        ("id", JsValue::from_str("claimed")),
        ("placement", JsValue::from_str("queued")),
    ]));
    let failure = object(&[
        ("ok", JsValue::FALSE),
        (
            "error",
            object(&[("code", JsValue::from_str("queue-item-not-found"))]).into(),
        ),
    ]);
    hub_set_queue(&claimed_rows, &Array::of1(&failure));
    call(&shell, "steerQueue", &[]);
    flush_microtasks().await;
    assert_eq!(entries("updateCalls").length(), 4);
    assert!(store_snapshot(&property(&shell, "notices")).is_null());

    let genuine_rows = Array::of1(&object(&[
        ("id", JsValue::from_str("bad")),
        ("placement", JsValue::from_str("queued")),
    ]));
    let failure = object(&[
        ("ok", JsValue::FALSE),
        (
            "error",
            object(&[("code", JsValue::from_str("host-error"))]).into(),
        ),
    ]);
    hub_set_queue(&genuine_rows, &Array::of1(&failure));
    call(&shell, "steerQueue", &[]);
    flush_microtasks().await;
    let notice = store_snapshot(&property(&shell, "notices"));
    assert_eq!(
        property(&notice, "level").as_string().as_deref(),
        Some("error")
    );
    assert_eq!(
        property(&notice, "text").as_string().as_deref(),
        Some("translated queue.steerFailed")
    );
}

fn resolved_send_helper_is_callable() {
    let (hub, bench) = setup();
    let shell = hub.shell_for(property(&bench, "binding")).unwrap();
    hub_set_send_mode("pending");
    call(&shell, "setDraft", &[JsValue::from_str("ok")]);
    call(&shell, "submit", &[]);
    hub_resolve_send(0);
}

#[wasm_bindgen_test]
async fn compiled_input_hub_runs_the_full_scoped_lifecycle_matrix() {
    scoped_resolution_listener_bails_and_teardown_preserve_one_shell_identity();
    missing_scope_binding_and_sessions_fail_at_the_source_boundaries();
    sink_commits_then_rolls_back_only_the_resident_untouched_shell().await;
    queue_steering_is_fifo_queued_only_immediate_and_convergent().await;
    resolved_send_helper_is_callable();
}
