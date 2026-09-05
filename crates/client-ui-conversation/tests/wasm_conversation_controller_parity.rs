//! Live WASM coverage for the scope-bound conversation and image controller.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::{
    BrowserConversationController, configure_client_ui_conversation_controller,
};
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
export function controllerSetup() {
  bench = {
    uuidSeq: 0, effects: [], prompts: [], promptResults: [], updates: [], updateResults: [],
    cancels: 0, cancelResults: [], older: 0, reads: [], readPending: [],
    created: [], revoked: [], blobs: [],
  }
  const root = {
    get(name) { return name === 'sessions' ? sessions : undefined },
    effect(setup, label) { const cleanup = setup(); bench.effects.push({ label, cleanup }); return cleanup },
  }
  const actx = {
    id: 's1', get(name) { return name === 'sessions' ? sessions : undefined },
    effect(setup, label) { const cleanup = setup(); bench.effects.push({ label, cleanup }); return cleanup },
  }
  const session = {
    prompt(content, mode) { bench.prompts.push({ content, mode, receiver: this }); return Promise.resolve(bench.promptResults.shift() ?? { ok: true, value: { accepted: true } }) },
    updateQueue(id, action) { bench.updates.push({ id, action, receiver: this }); return Promise.resolve(bench.updateResults.shift() ?? { ok: true, value: { accepted: true } }) },
    cancel() { bench.cancels += 1; return Promise.resolve(bench.cancelResults.shift() ?? { ok: true, value: { accepted: true } }) },
    loadOlder() { bench.older += 1; return Promise.resolve() },
    readAttachment(id) { bench.reads.push({ id, receiver: this }); const pending = deferred(); bench.readPending.push(pending); return pending.promise },
  }
  const binding = { sessionId: 's1', session, ctx: actx }
  const sessions = {
    scopeOf(ctx) { return ctx === actx ? 's1' : undefined },
    binding(id) { return id === 's1' ? binding : undefined },
  }
  const input = { kind: 'input' }
  const blocks = { kind: 'blocks' }
  const config = { input, blocks }
  const uuid = () => `draft-${++bench.uuidSeq}`
  globalThis.URL = {
    createObjectURL(value) { const url = `blob:${bench.created.length + 1}`; bench.created.push({ value, url, receiver: this }); return url },
    revokeObjectURL(url) { bench.revoked.push({ url, receiver: this }) },
  }
  globalThis.Blob = class { constructor(parts, options) { this.parts = parts; this.type = options.type; bench.blobs.push(this) } }
  Object.assign(bench, { root, actx, session, binding, sessions, input, blocks, config, uuid })
  return bench
}
export function controllerBench() { return bench }
export function controllerObject(entries) { return Object.fromEntries(entries) }
export function controllerFile(name, type, bytes) {
  const data = Uint8Array.from(bytes)
  return { name, type, size: data.byteLength, arrayBuffer() { return Promise.resolve(data.buffer) } }
}
export function controllerPushPrompt(value) { bench.promptResults.push(value) }
export function controllerPushUpdate(value) { bench.updateResults.push(value) }
export function controllerPushCancel(value) { bench.cancelResults.push(value) }
export function controllerResolveRead(index, value) { bench.readPending[index].resolve(value) }
export function controllerRejectRead(index, message) { bench.readPending[index].reject(new Error(message)) }
export function controllerCleanup(index) { return bench.effects[index].cleanup() }
export function controllerDisableObjectUrl() { globalThis.URL.createObjectURL = undefined }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = controllerSetup)]
    fn controller_setup() -> JsValue;
    #[wasm_bindgen(js_name = controllerBench)]
    fn controller_bench() -> JsValue;
    #[wasm_bindgen(js_name = controllerObject)]
    fn controller_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = controllerFile)]
    fn controller_file(name: &str, media_type: &str, bytes: &Array) -> JsValue;
    #[wasm_bindgen(js_name = controllerPushPrompt)]
    fn controller_push_prompt(value: &JsValue);
    #[wasm_bindgen(js_name = controllerPushUpdate)]
    fn controller_push_update(value: &JsValue);
    #[wasm_bindgen(js_name = controllerPushCancel)]
    fn controller_push_cancel(value: &JsValue);
    #[wasm_bindgen(js_name = controllerResolveRead)]
    fn controller_resolve_read(index: u32, value: &JsValue);
    #[wasm_bindgen(js_name = controllerRejectRead)]
    fn controller_reject_read(index: u32, message: &str);
    #[wasm_bindgen(js_name = controllerCleanup)]
    fn controller_cleanup(index: u32) -> JsValue;
    #[wasm_bindgen(js_name = controllerDisableObjectUrl)]
    fn controller_disable_object_url();
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key))
        .unwrap_or_else(|error| panic!("property {key:?} on {value:?} failed: {error:?}"))
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let values = Array::new();
    for (key, value) in entries {
        values.push(&Array::of2(&JsValue::from_str(key), value));
    }
    controller_object(&values).unchecked_into()
}

fn strings(values: &[&str]) -> Array {
    values
        .iter()
        .map(|value| JsValue::from_str(value))
        .collect()
}

fn bytes(values: &[u8]) -> Array {
    values
        .iter()
        .map(|value| JsValue::from_f64(f64::from(*value)))
        .collect()
}

fn failure(code: &str, message: &str) -> Object {
    object(&[
        ("ok", JsValue::FALSE),
        (
            "error",
            object(&[
                ("code", JsValue::from_str(code)),
                ("message", JsValue::from_str(message)),
                ("details", Object::new().into()),
            ])
            .into(),
        ),
    ])
}

fn read_success(id: &str, media_type: &str, data: &[u8]) -> Object {
    object(&[
        ("ok", JsValue::TRUE),
        (
            "value",
            object(&[
                (
                    "attachment",
                    object(&[
                        ("attachmentId", JsValue::from_str(id)),
                        ("mediaType", JsValue::from_str(media_type)),
                    ])
                    .into(),
                ),
                ("data", js_sys::Uint8Array::from(data).into()),
            ])
            .into(),
        ),
    ])
}

fn setup() -> (BrowserConversationController, JsValue) {
    let bench = controller_setup();
    configure_client_ui_conversation_controller(
        property(&bench, "uuid").dyn_into::<Function>().unwrap(),
    );
    let controller =
        BrowserConversationController::new(property(&bench, "root"), property(&bench, "config"))
            .unwrap();
    (controller, bench)
}

async fn resolve(promise: Promise) -> JsValue {
    JsFuture::from(promise).await.unwrap()
}

async fn reject(promise: Promise) -> String {
    let error = JsFuture::from(promise).await.unwrap_err();
    property(&error, "message").as_string().unwrap()
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // Scope verbs and both attachment lifecycles share one controller state.
async fn compiled_controller_runs_scope_prompt_draft_history_and_teardown_matrix() {
    let (root, bench) = setup();
    assert!(Object::is(&root.input(), &property(&bench, "input")));
    assert!(Object::is(&root.blocks(), &property(&bench, "blocks")));
    assert_eq!(
        reject(root.send("x".to_owned())).await,
        "conversation.send requires a session scope — address one via ctx.sessions.scope(id).conversation"
    );
    let scoped = root.for_context(property(&bench, "actx"));
    resolve(scoped.send("hello".to_owned())).await;
    let prompt = property(&bench, "prompts").unchecked_into::<Array>().get(0);
    assert_eq!(
        property(&prompt, "mode").as_string().as_deref(),
        Some("queue")
    );
    let content = property(&prompt, "content").unchecked_into::<Array>();
    assert_eq!(
        property(&content.get(0), "type").as_string().as_deref(),
        Some("text")
    );
    assert_eq!(
        property(&content.get(0), "text").as_string().as_deref(),
        Some("hello")
    );
    assert!(Object::is(
        &property(&prompt, "receiver"),
        &property(&bench, "session")
    ));
    controller_push_prompt(failure("agent-busy", "busy").as_ref());
    assert_eq!(
        reject(scoped.send("x".to_owned())).await,
        "conversation.send failed: agent-busy: busy"
    );

    controller_push_update(failure("queue-item-not-found", "claimed").as_ref());
    resolve(scoped.update_queue(
        JsValue::from_str("q1"),
        object(&[("kind", JsValue::from_str("steer"))]).into(),
    ))
    .await;
    controller_push_update(failure("internal", "broken").as_ref());
    assert_eq!(
        reject(scoped.update_queue(
            JsValue::from_str("q2"),
            object(&[("kind", JsValue::from_str("remove"))]).into(),
        ))
        .await,
        "conversation.updateQueue failed: internal: broken"
    );
    controller_push_cancel(failure("internal", "nope").as_ref());
    assert_eq!(
        reject(scoped.cancel()).await,
        "conversation.cancel failed: internal: nope"
    );
    resolve(scoped.load_older()).await;
    assert_eq!(property(&bench, "older").as_f64(), Some(1.0));

    let valid = controller_file("a.png", "image/png", &bytes(&[1, 2, 3]));
    let invalid = controller_file("bad.svg", "image/svg+xml", &bytes(&[4]));
    let error = root
        .create_draft_images(Array::of2(&valid, &invalid).into())
        .unwrap_err();
    assert_eq!(
        property(&error, "name").as_string().as_deref(),
        Some("UnsupportedImageMediaTypeError")
    );
    assert_eq!(
        property(&error, "mediaType").as_string().as_deref(),
        Some("image/svg+xml")
    );
    assert_eq!(
        property(&bench, "created")
            .unchecked_into::<Array>()
            .length(),
        0
    );
    let drafts = root
        .create_draft_images(
            Array::of2(
                &valid,
                &controller_file("b.jpg", "image/jpeg", &bytes(&[4, 5])),
            )
            .into(),
        )
        .unwrap();
    assert_eq!(drafts.length(), 2);
    assert_eq!(
        property(&drafts.get(0), "id").as_string().as_deref(),
        Some("draft-1")
    );
    assert_eq!(
        root.draft_images(strings(&["missing", "draft-2"]).into())
            .unwrap()
            .length(),
        1
    );
    root.release_draft_image("draft-2".to_owned()).unwrap();
    assert_eq!(
        property(&bench, "revoked")
            .unchecked_into::<Array>()
            .length(),
        1
    );

    let uploaded = root
        .create_draft_images(
            Array::of1(&controller_file(
                "upload.png",
                "image/png",
                &bytes(&[1, 2, 3]),
            ))
            .into(),
        )
        .unwrap();
    resolve(root.send_session(
        property(&bench, "session"),
        "caption".to_owned(),
        Array::of1(&property(&uploaded.get(0), "id")).into(),
        "steer".to_owned(),
    ))
    .await;
    let prompts = property(&bench, "prompts").unchecked_into::<Array>();
    let sent = prompts.get(prompts.length() - 1);
    assert_eq!(
        property(&sent, "mode").as_string().as_deref(),
        Some("steer")
    );
    let content = property(&sent, "content").unchecked_into::<Array>();
    assert_eq!(
        property(&content.get(0), "type").as_string().as_deref(),
        Some("image")
    );
    assert_eq!(
        property(&content.get(0), "data").as_string().as_deref(),
        Some("AQID")
    );
    assert_eq!(
        property(&content.get(0), "name").as_string().as_deref(),
        Some("upload.png")
    );
    assert_eq!(
        property(&content.get(1), "text").as_string().as_deref(),
        Some("caption")
    );
    assert_eq!(
        reject(root.send_session(
            property(&bench, "session"),
            String::new(),
            strings(&["missing"]).into(),
            "queue".to_owned(),
        ))
        .await,
        "conversation.sendSession: one or more draft images are no longer available"
    );

    let historical = object(&[("attachmentId", JsValue::from_str("history-1"))]);
    let first = root
        .resolve_image("s1".to_owned(), historical.clone().into())
        .unwrap();
    let duplicate = root
        .resolve_image("s1".to_owned(), historical.clone().into())
        .unwrap();
    assert!(Object::is(first.as_ref(), duplicate.as_ref()));
    assert_eq!(
        property(&bench, "reads").unchecked_into::<Array>().length(),
        1
    );
    root.release_session_images("s1".to_owned());
    controller_resolve_read(0, read_success("history-1", "image/png", &[9]).as_ref());
    assert_eq!(
        reject(first).await,
        "historical image scope was released before loading completed"
    );
    let second = root
        .resolve_image("s1".to_owned(), historical.clone().into())
        .unwrap();
    controller_resolve_read(1, read_success("history-1", "image/png", &[7, 8]).as_ref());
    assert_eq!(
        resolve(second.clone()).await.as_string().as_deref(),
        Some("blob:4")
    );
    assert!(Object::is(
        second.as_ref(),
        root.resolve_image("s1".to_owned(), historical.clone().into())
            .unwrap()
            .as_ref()
    ));
    root.release_session_images("s1".to_owned());
    for _ in 0..4 {
        JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
            .await
            .unwrap();
    }
    assert!(
        property(&bench, "revoked")
            .unchecked_into::<Array>()
            .iter()
            .any(|entry| property(&entry, "url").as_string().as_deref() == Some("blob:4"))
    );
    assert_eq!(
        reject(
            root.resolve_image("missing".to_owned(), historical.clone().into())
                .unwrap(),
        )
        .await,
        "conversation.resolveImage: unknown session \"missing\""
    );

    let fallback = object(&[("attachmentId", JsValue::from_str("history-2"))]);
    controller_disable_object_url();
    let data_url = root
        .resolve_image("s1".to_owned(), fallback.into())
        .unwrap();
    controller_resolve_read(2, read_success("history-2", "image/png", &[1, 2]).as_ref());
    assert_eq!(
        resolve(data_url).await.as_string().as_deref(),
        Some("data:image/png;base64,AQI=")
    );

    controller_cleanup(0);
    assert_eq!(
        reject(
            root.resolve_image(
                "s1".to_owned(),
                object(&[("attachmentId", JsValue::from_str("disposed"))]).into(),
            )
            .unwrap(),
        )
        .await,
        "conversation.resolveImage: service is disposed"
    );
    assert_eq!(
        root.draft_images(strings(&["draft-1"]).into())
            .unwrap()
            .length(),
        0
    );
}
