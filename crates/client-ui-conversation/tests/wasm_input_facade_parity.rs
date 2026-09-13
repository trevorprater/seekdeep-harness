//! Live WASM coverage for the per-session input facade and effect executor.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::BrowserSessionInputShell;
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

function reference(name = 'item') {
  return { source: 'skills', ref: name, label: `/${name}`, clipboardText: `/${name}` }
}

function claim(token = '/goal ', hint = 'describe goal') {
  const value = {
    token,
    ...(hint === undefined ? {} : { hint }),
    submit(args, actx) {
      bench.submitCalls.push({ args, actx, receiver: this })
      switch (bench.submitMode) {
        case 'success': return Promise.resolve({ kind: 'success', text: 'done' })
        case 'success-empty': return Promise.resolve({ kind: 'success' })
        case 'error': return Promise.resolve({ kind: 'error', text: 'business failed' })
        case 'reject': return Promise.reject(new Error('submit rejected'))
        case 'throw': throw new Error('submit exploded')
        case 'pending': {
          bench.submitPending = deferred()
          return bench.submitPending.promise
        }
        default: throw new Error(`unknown submit mode ${bench.submitMode}`)
      }
    },
  }
  bench.lastClaim = value
  return value
}

function adjudicationOutcome(mode) {
  switch (mode) {
    case 'miss': return undefined
    case 'handled': return 'handled'
    case 'claim': return { claim: claim() }
    case 'insert': return { insert: reference('enter-ref') }
    case 'text': return { text: '/literal ' }
    default: throw new Error(`unknown adjudication outcome ${mode}`)
  }
}

export function inputSetup(controllerEnabled) {
  bench = {
    controllerEnabled,
    adjudicateMode: 'miss',
    submitMode: 'success',
    serializeMode: 'success',
    arbitration: 'consumed',
    spaceConsumed: false,
    queue: [{ id: 'q1', placement: 'queued' }],
    queueListeners: [],
    queueReceivers: [],
    stateNotifications: 0,
    sinks: [],
    tracks: [],
    adjudications: [],
    serializations: [],
    serializerPending: [],
    submitCalls: [],
    mirrorCalls: [],
    lexiconListeners: [],
    lexiconNotifications: 0,
    popupDismisses: 0,
    steerCalls: 0,
    thunkReceivers: [],
  }
  bench.lexiconValue = new Map([['/', ['/goal']], ['@', ['item']]])
  bench.lexiconDisposer = () => { bench.lexiconNotifications += 100 }
  const queue = {
    getSnapshot() { bench.queueReceivers.push(this); return bench.queue },
    subscribe(listener) {
      bench.queueReceivers.push(this)
      bench.queueListeners.push(listener)
      return () => {
        const at = bench.queueListeners.indexOf(listener)
        if (at >= 0) bench.queueListeners.splice(at, 1)
      }
    },
  }
  const controller = {
    lexicon: {
      getSnapshot() { return bench.lexiconValue },
      subscribe(listener) {
        bench.lexiconListeners.push(listener)
        return bench.lexiconDisposer
      },
    },
    track(draft, caret, guard, draftRev) {
      bench.tracks.push({ draft, caret, guard, draftRev, receiver: this })
    },
    arbitrate(key, composing) {
      bench.arbitrations ??= []
      bench.arbitrations.push({ key, composing, receiver: this })
      return bench.arbitration
    },
    onSpace() {
      bench.spaceCalls = (bench.spaceCalls ?? 0) + 1
      return bench.spaceConsumed
    },
    adjudicate(line, signal) {
      bench.adjudications.push({ line, signal, receiver: this })
      switch (bench.adjudicateMode) {
        case 'reject': return Promise.reject(new Error('adjudication failed'))
        case 'throw': throw new Error('adjudication exploded')
        case 'pending': {
          bench.adjudicationPending = deferred()
          return bench.adjudicationPending.promise
        }
        default: return Promise.resolve(adjudicationOutcome(bench.adjudicateMode))
      }
    },
    serializeReference(source, ref, signal) {
      const record = { source, ref, signal, receiver: this }
      bench.serializations.push(record)
      switch (bench.serializeMode) {
        case 'success': return Promise.resolve(`<${source}:${ref}>`)
        case 'reject': return Promise.reject(new Error('serialization failed'))
        case 'throw': throw new Error('serialization exploded')
        case 'pending': {
          const slot = deferred()
          record.pending = slot
          bench.serializerPending.push(slot)
          return slot.promise
        }
        default: throw new Error(`unknown serialize mode ${bench.serializeMode}`)
      }
    },
  }
  const popup = {
    dismiss() {
      bench.popupDismisses += 1
      bench.popupReceiver = this
    },
  }
  const deps = {
    actx: { session: 's1' },
    queue,
    defaultSink(text, imageIds, mode) {
      bench.sinks.push({ text, imageIds, mode, receiver: this })
      bench.sinkAction?.()
    },
    inputTriggers() {
      bench.thunkReceivers.push(this)
      return bench.controllerEnabled ? controller : undefined
    },
    popup() {
      bench.popupThunkReceiver = this
      return popup
    },
    steerQueue() {
      bench.steerCalls += 1
      bench.steerReceiver = this
    },
  }
  bench.deps = deps
  bench.queueFace = queue
  bench.controller = controller
  bench.popup = popup
  return deps
}

export function inputBench() { return bench }
export function inputProduce() {
  return (base, recipe) => {
    const draft = Array.isArray(base) ? [...base] : { ...base }
    recipe(draft)
    return draft
  }
}
export function inputObject(entries) { return Object.fromEntries(entries) }
export function inputClaim(token, hint) { return claim(token, hint === null ? undefined : hint) }
export function inputReference(name) { return reference(name) }
export function inputMirror(name) {
  return function write(text) { bench.mirrorCalls.push({ name, text, receiver: this }) }
}
export function inputStateListener() { return () => { bench.stateNotifications += 1 } }
export function inputLexiconListener() { return () => { bench.lexiconNotifications += 1 } }
export function inputSetQueue(queue) {
  bench.queue = queue
  for (const listener of [...bench.queueListeners]) listener()
}
export function inputSetController(enabled) { bench.controllerEnabled = enabled }
export function inputSetAdjudicateMode(mode) { bench.adjudicateMode = mode }
export function inputSetSubmitMode(mode) { bench.submitMode = mode }
export function inputSetSerializeMode(mode) { bench.serializeMode = mode }
export function inputSetArbitration(value) { bench.arbitration = value }
export function inputSetSpace(value) { bench.spaceConsumed = value }
export function inputSetSinkReentry(actions) {
  bench.sinkAction = () => { actions.addImages(['reentered']) }
}
export function inputResolveAdjudication(mode) { bench.adjudicationPending.resolve(adjudicationOutcome(mode)) }
export function inputResolveSubmit(kind, text) {
  bench.submitPending.resolve({ kind, ...(text === null ? {} : { text }) })
}
export function inputResolveSerializer(index, text) { bench.serializerPending[index].resolve(text) }
export function inputFireLexicon() {
  bench.lexiconNotifications += 1
  for (const listener of [...bench.lexiconListeners]) listener()
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = inputSetup)]
    fn input_setup(controller_enabled: bool) -> JsValue;
    #[wasm_bindgen(js_name = inputBench)]
    fn input_bench() -> JsValue;
    #[wasm_bindgen(js_name = inputProduce)]
    fn input_produce() -> Function;
    #[wasm_bindgen(js_name = inputObject)]
    fn input_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = inputClaim)]
    fn input_claim(token: &str, hint: JsValue) -> JsValue;
    #[wasm_bindgen(js_name = inputReference)]
    fn input_reference(name: &str) -> JsValue;
    #[wasm_bindgen(js_name = inputMirror)]
    fn input_mirror(name: &str) -> Function;
    #[wasm_bindgen(js_name = inputStateListener)]
    fn input_state_listener() -> Function;
    #[wasm_bindgen(js_name = inputLexiconListener)]
    fn input_lexicon_listener() -> Function;
    #[wasm_bindgen(js_name = inputSetQueue)]
    fn input_set_queue(queue: &Array);
    #[wasm_bindgen(js_name = inputSetController)]
    fn input_set_controller(enabled: bool);
    #[wasm_bindgen(js_name = inputSetAdjudicateMode)]
    fn input_set_adjudicate_mode(mode: &str);
    #[wasm_bindgen(js_name = inputSetSubmitMode)]
    fn input_set_submit_mode(mode: &str);
    #[wasm_bindgen(js_name = inputSetSerializeMode)]
    fn input_set_serialize_mode(mode: &str);
    #[wasm_bindgen(js_name = inputSetArbitration)]
    fn input_set_arbitration(value: JsValue);
    #[wasm_bindgen(js_name = inputSetSpace)]
    fn input_set_space(value: bool);
    #[wasm_bindgen(js_name = inputSetSinkReentry)]
    fn input_set_sink_reentry(actions: &JsValue);
    #[wasm_bindgen(js_name = inputResolveAdjudication)]
    fn input_resolve_adjudication(mode: &str);
    #[wasm_bindgen(js_name = inputResolveSubmit)]
    fn input_resolve_submit(kind: &str, text: JsValue);
    #[wasm_bindgen(js_name = inputResolveSerializer)]
    fn input_resolve_serializer(index: u32, text: &str);
    #[wasm_bindgen(js_name = inputFireLexicon)]
    fn input_fire_lexicon();
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn has(value: &JsValue, key: &str) -> bool {
    Reflect::has(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    input_object(&array).unchecked_into()
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

fn store_snapshot(store: &JsValue) -> JsValue {
    call(store, "getSnapshot", &[])
}

fn entries(value: &JsValue, key: &str) -> Array {
    property(value, key).unchecked_into()
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

fn selection(start: u32, end: u32) -> Object {
    object(&[
        ("start", JsValue::from_f64(f64::from(start))),
        ("end", JsValue::from_f64(f64::from(end))),
    ])
}

fn setup(controller_enabled: bool) -> BrowserSessionInputShell {
    seekdeep_client_runtime::install_store_produce(input_produce());
    BrowserSessionInputShell::new(input_setup(controller_enabled)).unwrap()
}

async fn flush_microtasks() {
    for _ in 0..10 {
        JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
            .await
            .unwrap();
    }
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One facade snapshot/identity contract stays legible together.
fn stores_faces_queue_and_mirror_preserve_source_identity_and_lifecycle() {
    let shell = setup(false);
    let bench = input_bench();
    let state = shell.state();
    let notices = shell.notices();
    let actions = shell.actions().unwrap();
    let lexicon = shell.lexicon().unwrap();
    assert!(Object::is(&state, &shell.state()));
    assert!(Object::is(&notices, &shell.notices()));
    assert!(Object::is(&actions, &shell.actions().unwrap()));
    assert!(Object::is(&lexicon, &shell.lexicon().unwrap()));

    let initial = shell.snapshot().unwrap();
    assert_eq!(property(&initial, "draft").as_string().as_deref(), Some(""));
    assert_eq!(property(&initial, "draftRev").as_f64(), Some(0.0));
    assert_eq!(
        property(&initial, "phase").as_string().as_deref(),
        Some("plain")
    );
    assert!(!has(&initial, "claim"));
    assert!(!has(&initial, "paste"));
    assert!(Object::is(
        &property(&initial, "queue"),
        &property(&bench, "queue")
    ));
    let initial_images = property(&initial, "imageIds");

    let deps = property(&input_bench(), "deps");
    let no_queue = object(&[
        ("actx", property(&deps, "actx")),
        ("defaultSink", property(&deps, "defaultSink")),
    ]);
    let second = BrowserSessionInputShell::new(no_queue.clone().into()).unwrap();
    let third = BrowserSessionInputShell::new(no_queue.into()).unwrap();
    let empty_one = call(&lexicon, "getSnapshot", &[]);
    let empty_two = call(&second.lexicon().unwrap(), "getSnapshot", &[]);
    assert!(Object::is(&empty_one, &empty_two));
    assert!(Object::is(
        &property(&second.snapshot().unwrap(), "queue"),
        &property(&third.snapshot().unwrap(), "queue")
    ));

    let listener = input_state_listener();
    let _off = call(&state, "subscribe", &[listener.into()]);
    shell
        .set_draft("seed".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(
        property(&input_bench(), "stateNotifications").as_f64(),
        Some(1.0)
    );
    assert!(Object::is(
        &initial_images,
        &property(&shell.snapshot().unwrap(), "imageIds")
    ));

    let first_mirror = input_mirror("first");
    let off_first = shell.bind_mirror(first_mirror.into()).unwrap();
    shell
        .set_draft("one".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    let second_mirror = input_mirror("second");
    let off_second = shell.bind_mirror(second_mirror.into()).unwrap();
    off_first
        .unchecked_into::<Function>()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    shell
        .set_draft("two".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    off_second
        .unchecked_into::<Function>()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    shell
        .set_draft("three".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    let mirrors = entries(&input_bench(), "mirrorCalls");
    assert_eq!(mirrors.length(), 2);
    assert_eq!(
        property(&mirrors.get(0), "name").as_string().as_deref(),
        Some("first")
    );
    assert_eq!(
        property(&mirrors.get(1), "name").as_string().as_deref(),
        Some("second")
    );
    assert_eq!(
        property(&mirrors.get(1), "text").as_string().as_deref(),
        Some("two")
    );

    let next_queue = Array::of1(&object(&[("id", JsValue::from_str("q2"))]));
    input_set_queue(&next_queue);
    assert!(Object::is(
        &property(&shell.snapshot().unwrap(), "queue"),
        next_queue.as_ref()
    ));
    let before_dispose_update = property(&input_bench(), "stateNotifications")
        .as_f64()
        .unwrap();
    shell.dispose().unwrap();
    let after_dispose = Array::of1(&object(&[("id", JsValue::from_str("q3"))]));
    input_set_queue(&after_dispose);
    assert_eq!(
        property(&input_bench(), "stateNotifications").as_f64(),
        Some(before_dispose_update + 2.0)
    );
    assert!(Object::is(
        &property(&shell.snapshot().unwrap(), "queue"),
        after_dispose.as_ref()
    ));

    let queue_receivers = entries(&input_bench(), "queueReceivers");
    assert!(
        queue_receivers
            .iter()
            .all(|receiver| Object::is(&receiver, &property(&input_bench(), "queueFace")))
    );
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // Public verbs map one-to-one onto the frozen shell face.
fn public_actions_images_spans_paste_and_undo_map_into_the_portable_machine() {
    let shell = setup(false);
    let actions = shell.actions().unwrap();
    call(&actions, "setDraft", &[JsValue::from_str("draft")]);
    assert_eq!(
        property(&shell.snapshot().unwrap(), "draft")
            .as_string()
            .as_deref(),
        Some("draft")
    );

    let first_images = property(&shell.snapshot().unwrap(), "imageIds");
    assert!(
        call(&actions, "addImages", &[Array::new().into()])
            .as_bool()
            .unwrap()
    );
    assert!(Object::is(
        &first_images,
        &property(&shell.snapshot().unwrap(), "imageIds")
    ));
    assert!(
        call(&actions, "addImages", &[strings(&["i1", "i2"]).into()])
            .as_bool()
            .unwrap()
    );
    let added = property(&shell.snapshot().unwrap(), "imageIds");
    assert_eq!(array_strings(&added), ["i1", "i2"]);
    call(&actions, "removeImage", &[JsValue::from_str("missing")]);
    assert!(Object::is(
        &added,
        &property(&shell.snapshot().unwrap(), "imageIds")
    ));
    call(&actions, "pruneImages", &[strings(&["i2", "i1"]).into()]);
    assert!(Object::is(
        &added,
        &property(&shell.snapshot().unwrap(), "imageIds")
    ));
    shell.restore_images(strings(&["i0", "i1"]).into()).unwrap();
    assert_eq!(
        array_strings(&property(&shell.snapshot().unwrap(), "imageIds")),
        ["i0", "i1", "i2"]
    );

    shell
        .set_draft("/go".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    let revision = property(&shell.snapshot().unwrap(), "draftRev")
        .as_f64()
        .unwrap();
    assert!(
        shell
            .begin_command(
                input_claim("/goal ", JsValue::from_str("describe goal")),
                span(0, 3, revision).into(),
            )
            .unwrap()
    );
    let claimed = shell.snapshot().unwrap();
    assert_eq!(
        property(&claimed, "phase").as_string().as_deref(),
        Some("claimed")
    );
    assert_eq!(
        property(&claimed, "draft").as_string().as_deref(),
        Some("/goal ")
    );
    assert_eq!(
        property(&property(&claimed, "claim"), "hint")
            .as_string()
            .as_deref(),
        Some("describe goal")
    );
    assert!(
        !shell
            .insert_text("stale".to_owned(), span(0, 1, revision).into())
            .unwrap()
    );
    assert!(
        shell
            .consume_token(
                object(&[
                    ("kind", JsValue::from_str("bare-token")),
                    ("token", JsValue::from_str("/goal")),
                ])
                .into(),
            )
            .unwrap()
    );
    assert_eq!(
        property(&shell.snapshot().unwrap(), "phase")
            .as_string()
            .as_deref(),
        Some("plain")
    );

    shell
        .set_draft("@item".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    let revision = property(&shell.snapshot().unwrap(), "draftRev")
        .as_f64()
        .unwrap();
    assert!(
        shell
            .insert_reference(input_reference("item"), span(0, 5, revision).into())
            .unwrap()
    );
    let inserted = shell.snapshot().unwrap();
    assert_eq!(
        property(&inserted, "draft").as_string().as_deref(),
        Some("\u{fffc} ")
    );
    let occurrences = entries(&inserted, "occurrences");
    assert_eq!(occurrences.length(), 1);
    assert_eq!(
        property(&occurrences.get(0), "source")
            .as_string()
            .as_deref(),
        Some("skills")
    );
    assert_eq!(
        property(&occurrences.get(0), "ref").as_string().as_deref(),
        Some("item")
    );
    shell.undo().unwrap();
    assert_eq!(
        property(&shell.snapshot().unwrap(), "draft")
            .as_string()
            .as_deref(),
        Some("@item")
    );
    shell.redo().unwrap();
    assert_eq!(
        property(&shell.snapshot().unwrap(), "draft")
            .as_string()
            .as_deref(),
        Some("\u{fffc} ")
    );

    shell.commit_send(strings(&["i1"]).into()).unwrap();
    let committed = shell.snapshot().unwrap();
    assert_eq!(
        property(&committed, "draft").as_string().as_deref(),
        Some("")
    );
    assert_eq!(
        array_strings(&property(&committed, "imageIds")),
        ["i0", "i2"]
    );
    shell.undo().unwrap();
    assert_eq!(
        property(&shell.snapshot().unwrap(), "draft")
            .as_string()
            .as_deref(),
        Some("")
    );

    let component = object(&[
        ("start", JsValue::from_f64(0.0)),
        ("end", JsValue::from_f64(4.0)),
        ("reference", input_reference("goal")),
    ]);
    shell
        .paste_begin(
            "/goal tail".to_owned(),
            selection(0, 0).into(),
            Array::of1(&component).into(),
            JsValue::from_f64(7.0),
        )
        .unwrap();
    let pasted = shell.snapshot().unwrap();
    assert_eq!(
        property(&property(&pasted, "paste"), "generation").as_f64(),
        Some(7.0)
    );
    assert_eq!(entries(&pasted, "occurrences").length(), 1);
    shell.invalidate_paste().unwrap();
    assert!(!has(&shell.snapshot().unwrap(), "paste"));
}

#[wasm_bindgen_test]
async fn trigger_forwarding_lock_dismissal_and_disposal_abort_match_source() {
    let shell = setup(true);
    shell
        .set_draft("hello😀".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    let revision = property(&shell.snapshot().unwrap(), "draftRev")
        .as_f64()
        .unwrap();
    shell.track("hello😀".to_owned(), 2.0).unwrap();
    let first_track = entries(&input_bench(), "tracks").get(0);
    assert_eq!(property(&first_track, "caret").as_f64(), Some(2.0));
    assert_eq!(property(&first_track, "draftRev").as_f64(), Some(revision));
    assert_eq!(
        property(&property(&first_track, "guard"), "tier")
            .as_string()
            .as_deref(),
        Some("plain")
    );
    input_set_arbitration(JsValue::NULL);
    assert_eq!(shell.arbitrate("enter".to_owned(), false).unwrap(), "pass");
    input_set_arbitration(JsValue::from_str("consumed"));
    assert_eq!(shell.arbitrate("up".to_owned(), true).unwrap(), "consumed");
    input_set_space(true);
    assert!(shell.space().unwrap());
    let tracks = entries(&input_bench(), "tracks");
    assert_eq!(
        property(&tracks.get(tracks.length() - 1), "caret").as_f64(),
        Some(7.0)
    );

    let lexicon = shell.lexicon().unwrap();
    assert!(Object::is(
        &call(&lexicon, "getSnapshot", &[]),
        &property(&input_bench(), "lexiconValue")
    ));
    let disposer = call(&lexicon, "subscribe", &[input_lexicon_listener().into()]);
    assert!(Object::is(
        &disposer,
        &property(&input_bench(), "lexiconDisposer")
    ));
    input_fire_lexicon();
    assert_eq!(
        property(&input_bench(), "lexiconNotifications").as_f64(),
        Some(2.0)
    );

    shell.steer_queue().unwrap();
    shell.dismiss_popup().unwrap();
    assert_eq!(property(&input_bench(), "steerCalls").as_f64(), Some(1.0));
    assert_eq!(
        property(&input_bench(), "popupDismisses").as_f64(),
        Some(1.0)
    );
    assert!(Object::is(
        &property(&input_bench(), "steerReceiver"),
        &property(&input_bench(), "deps")
    ));
    assert!(Object::is(
        &property(&input_bench(), "popupThunkReceiver"),
        &property(&input_bench(), "deps")
    ));

    input_set_adjudicate_mode("pending");
    shell
        .set_draft("  /wait  ".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    shell.submit(Some("queue".to_owned())).unwrap();
    let locked = shell.snapshot().unwrap();
    assert_eq!(
        property(&locked, "phase").as_string().as_deref(),
        Some("adjudicating")
    );
    assert_eq!(
        property(&input_bench(), "popupDismisses").as_f64(),
        Some(2.0)
    );
    let adjudication = entries(&input_bench(), "adjudications").get(0);
    assert_eq!(
        property(&adjudication, "line").as_string().as_deref(),
        Some("/wait")
    );
    let signal = property(&adjudication, "signal");
    assert_eq!(property(&signal, "aborted").as_bool(), Some(false));
    assert!(!shell.add_images(strings(&["locked"]).into()).unwrap());
    shell.dispose().unwrap();
    assert_eq!(property(&signal, "aborted").as_bool(), Some(true));
    assert_eq!(
        property(&shell.snapshot().unwrap(), "phase")
            .as_string()
            .as_deref(),
        Some("plain")
    );
    input_resolve_adjudication("miss");
    flush_microtasks().await;
    assert_eq!(entries(&input_bench(), "sinks").length(), 0);
    assert!(store_snapshot(&shell.notices()).is_null());
    let thunk_receivers = entries(&input_bench(), "thunkReceivers");
    assert!(
        thunk_receivers
            .iter()
            .all(|receiver| Object::is(&receiver, &property(&input_bench(), "deps")))
    );
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // Promise phase, rollback, drift, and failure timing form one transaction matrix.
async fn adjudication_and_command_transactions_preserve_timing_rollback_and_notices() {
    let shell = setup(false);
    shell
        .set_draft(" /ordinary ".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    shell.submit(Some("steer".to_owned())).unwrap();
    let sinks = entries(&input_bench(), "sinks");
    assert_eq!(sinks.length(), 1);
    assert_eq!(
        property(&sinks.get(0), "text").as_string().as_deref(),
        Some("/ordinary")
    );
    assert_eq!(
        property(&sinks.get(0), "mode").as_string().as_deref(),
        Some("steer")
    );

    let shell = setup(true);
    input_set_adjudicate_mode("miss");
    shell
        .set_draft(" /miss ".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    shell.submit(Some("queue".to_owned())).unwrap();
    assert_eq!(
        property(&shell.snapshot().unwrap(), "phase")
            .as_string()
            .as_deref(),
        Some("adjudicating")
    );
    flush_microtasks().await;
    assert_eq!(
        property(&shell.snapshot().unwrap(), "phase")
            .as_string()
            .as_deref(),
        Some("plain")
    );
    assert_eq!(entries(&input_bench(), "sinks").length(), 1);

    let shell = setup(true);
    input_set_adjudicate_mode("reject");
    shell
        .set_draft("/broken".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    shell.submit(None).unwrap();
    flush_microtasks().await;
    let notice = store_snapshot(&shell.notices());
    assert_eq!(
        property(&notice, "level").as_string().as_deref(),
        Some("error")
    );
    assert_eq!(
        property(&notice, "text").as_string().as_deref(),
        Some("adjudication failed")
    );
    assert_eq!(
        property(&shell.snapshot().unwrap(), "draft")
            .as_string()
            .as_deref(),
        Some("/broken")
    );

    let shell = setup(true);
    input_set_adjudicate_mode("claim");
    input_set_submit_mode("success");
    shell
        .set_draft("/goal ship it".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    shell.submit(None).unwrap();
    assert_eq!(entries(&input_bench(), "submitCalls").length(), 0);
    flush_microtasks().await;
    let calls = entries(&input_bench(), "submitCalls");
    assert_eq!(calls.length(), 1);
    assert_eq!(
        property(&calls.get(0), "args").as_string().as_deref(),
        Some("ship it")
    );
    assert!(Object::is(
        &property(&calls.get(0), "actx"),
        &property(&property(&input_bench(), "deps"), "actx")
    ));
    assert!(Object::is(
        &property(&calls.get(0), "receiver"),
        &property(&input_bench(), "lastClaim")
    ));
    assert_eq!(
        property(&shell.snapshot().unwrap(), "phase")
            .as_string()
            .as_deref(),
        Some("plain")
    );
    assert_eq!(
        property(&shell.snapshot().unwrap(), "draft")
            .as_string()
            .as_deref(),
        Some("")
    );
    assert_eq!(
        property(&store_snapshot(&shell.notices()), "text")
            .as_string()
            .as_deref(),
        Some("done")
    );

    let shell = setup(true);
    input_set_adjudicate_mode("claim");
    input_set_submit_mode("error");
    shell
        .set_draft("/goal retry".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    shell.submit(None).unwrap();
    flush_microtasks().await;
    assert_eq!(
        property(&shell.snapshot().unwrap(), "phase")
            .as_string()
            .as_deref(),
        Some("claimed")
    );
    assert_eq!(
        property(&store_snapshot(&shell.notices()), "text")
            .as_string()
            .as_deref(),
        Some("business failed")
    );
    input_set_submit_mode("success-empty");
    shell.submit(None).unwrap();
    assert_eq!(entries(&input_bench(), "submitCalls").length(), 1);
    flush_microtasks().await;
    assert_eq!(entries(&input_bench(), "submitCalls").length(), 2);
    assert_eq!(
        property(&shell.snapshot().unwrap(), "phase")
            .as_string()
            .as_deref(),
        Some("plain")
    );

    let shell = setup(true);
    input_set_adjudicate_mode("claim");
    input_set_submit_mode("pending");
    shell
        .set_draft("/goal original".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    shell.submit(None).unwrap();
    flush_microtasks().await;
    assert_eq!(
        property(&shell.snapshot().unwrap(), "phase")
            .as_string()
            .as_deref(),
        Some("submitting")
    );
    shell
        .set_draft("/goal drifted".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    input_resolve_submit("error", JsValue::from_str("late failure"));
    flush_microtasks().await;
    let drifted = shell.snapshot().unwrap();
    assert_eq!(
        property(&drifted, "phase").as_string().as_deref(),
        Some("plain")
    );
    assert!(!has(&drifted, "claim"));
    assert_eq!(
        property(&drifted, "draft").as_string().as_deref(),
        Some("/goal drifted")
    );
    assert_eq!(
        property(&store_snapshot(&shell.notices()), "text")
            .as_string()
            .as_deref(),
        Some("late failure")
    );

    let shell = setup(true);
    input_set_adjudicate_mode("claim");
    input_set_submit_mode("throw");
    shell
        .set_draft("/goal throw".to_owned(), JsValue::UNDEFINED)
        .unwrap();
    shell.submit(None).unwrap();
    assert_eq!(entries(&input_bench(), "submitCalls").length(), 0);
    flush_microtasks().await;
    assert_eq!(
        property(&shell.snapshot().unwrap(), "phase")
            .as_string()
            .as_deref(),
        Some("claimed")
    );
    assert_eq!(
        property(&store_snapshot(&shell.notices()), "text")
            .as_string()
            .as_deref(),
        Some("submit exploded")
    );
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // Serialization ordering, error, absence, and disposal are one atomic sink boundary.
async fn reference_serialization_is_ordered_abortable_and_never_silently_downgrades() {
    let shell = setup(true);
    input_set_serialize_mode("pending");
    let components = Array::new();
    components.push(&object(&[
        ("start", JsValue::from_f64(2.0)),
        ("end", JsValue::from_f64(4.0)),
        ("reference", input_reference("x")),
    ]));
    components.push(&object(&[
        ("start", JsValue::from_f64(7.0)),
        ("end", JsValue::from_f64(9.0)),
        ("reference", input_reference("y")),
    ]));
    shell
        .paste_begin(
            "A @x B @y C".to_owned(),
            selection(0, 0).into(),
            components.into(),
            JsValue::from_f64(1.0),
        )
        .unwrap();
    shell.add_images(strings(&["i1"]).into()).unwrap();
    shell.submit(Some("steer".to_owned())).unwrap();
    let serializations = entries(&input_bench(), "serializations");
    assert_eq!(serializations.length(), 2);
    assert!(Object::is(
        &property(&serializations.get(0), "signal"),
        &property(&serializations.get(1), "signal")
    ));
    assert_eq!(
        property(&serializations.get(0), "ref")
            .as_string()
            .as_deref(),
        Some("x")
    );
    assert_eq!(
        property(&serializations.get(1), "ref")
            .as_string()
            .as_deref(),
        Some("y")
    );
    shell.add_images(strings(&["i2"]).into()).unwrap();
    input_resolve_serializer(1, "<Y>");
    input_resolve_serializer(0, "<X>");
    flush_microtasks().await;
    let sinks = entries(&input_bench(), "sinks");
    assert_eq!(sinks.length(), 1);
    let sink = sinks.get(0);
    assert_eq!(
        property(&sink, "text").as_string().as_deref(),
        Some("A <X> B <Y> C")
    );
    assert_eq!(array_strings(&property(&sink, "imageIds")), ["i1"]);
    assert_eq!(
        property(&sink, "mode").as_string().as_deref(),
        Some("steer")
    );
    assert!(Object::is(
        &property(&sink, "receiver"),
        &property(&input_bench(), "deps")
    ));

    let shell = setup(true);
    input_set_serialize_mode("reject");
    shell
        .paste_begin(
            "@x".to_owned(),
            selection(0, 0).into(),
            Array::of1(&object(&[
                ("start", JsValue::from_f64(0.0)),
                ("end", JsValue::from_f64(2.0)),
                ("reference", input_reference("x")),
            ]))
            .into(),
            JsValue::UNDEFINED,
        )
        .unwrap();
    shell.submit(None).unwrap();
    assert!(store_snapshot(&shell.notices()).is_null());
    let signal = property(&entries(&input_bench(), "serializations").get(0), "signal");
    flush_microtasks().await;
    assert_eq!(property(&signal, "aborted").as_bool(), Some(true));
    assert_eq!(entries(&input_bench(), "sinks").length(), 0);
    assert_eq!(
        property(&store_snapshot(&shell.notices()), "text")
            .as_string()
            .as_deref(),
        Some("serialization failed")
    );
    assert_eq!(
        entries(&shell.snapshot().unwrap(), "occurrences").length(),
        1
    );

    let shell = setup(false);
    shell
        .paste_begin(
            "@x".to_owned(),
            selection(0, 0).into(),
            Array::of1(&object(&[
                ("start", JsValue::from_f64(0.0)),
                ("end", JsValue::from_f64(2.0)),
                ("reference", input_reference("x")),
            ]))
            .into(),
            JsValue::UNDEFINED,
        )
        .unwrap();
    shell.submit(None).unwrap();
    assert!(store_snapshot(&shell.notices()).is_null());
    flush_microtasks().await;
    assert_eq!(
        property(&store_snapshot(&shell.notices()), "text")
            .as_string()
            .as_deref(),
        Some("no serializer for reference source \"skills\"")
    );
    assert_eq!(entries(&input_bench(), "sinks").length(), 0);

    let shell = setup(true);
    input_set_serialize_mode("pending");
    shell
        .paste_begin(
            "@x".to_owned(),
            selection(0, 0).into(),
            Array::of1(&object(&[
                ("start", JsValue::from_f64(0.0)),
                ("end", JsValue::from_f64(2.0)),
                ("reference", input_reference("x")),
            ]))
            .into(),
            JsValue::UNDEFINED,
        )
        .unwrap();
    shell.submit(None).unwrap();
    shell.dispose().unwrap();
    input_resolve_serializer(0, "<X>");
    flush_microtasks().await;
    assert_eq!(entries(&input_bench(), "sinks").length(), 0);
    assert!(store_snapshot(&shell.notices()).is_null());
}

#[wasm_bindgen_test]
fn image_only_submission_uses_the_requested_mode_without_mutating_the_draft() {
    let shell = setup(false);
    shell.add_images(strings(&["image-only"]).into()).unwrap();
    input_set_sink_reentry(&shell.actions().unwrap());
    shell.submit(Some("steer".to_owned())).unwrap();
    let sinks = entries(&input_bench(), "sinks");
    assert_eq!(sinks.length(), 1);
    assert_eq!(
        property(&sinks.get(0), "text").as_string().as_deref(),
        Some("")
    );
    assert_eq!(
        array_strings(&property(&sinks.get(0), "imageIds")),
        ["image-only"]
    );
    assert_eq!(
        property(&sinks.get(0), "mode").as_string().as_deref(),
        Some("steer")
    );
    assert_eq!(
        array_strings(&property(&shell.snapshot().unwrap(), "imageIds")),
        ["image-only", "reentered"]
    );
}
