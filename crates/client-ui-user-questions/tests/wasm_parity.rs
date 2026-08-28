//! Live Rust/WASM generic question, plan review, carrier, and plugin parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_user_questions::{
    apply_client_ui_user_questions, configure_client_ui_user_questions,
    exported_question_composer_component, user_questions_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
function reactHooks() {
  const slots = []
  let cursor = 0
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useRef(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = { current: initial }
      return slots[index]
    },
    useState(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = initial
      return [slots[index], value => { slots[index] = typeof value === 'function' ? value(slots[index]) : value }]
    },
  }
  return { React, reset() { cursor = 0 } }
}

function translations(key) {
  return ({
    'error.incomplete': 'Please complete this question first.',
    'error.unanswered': 'Please select an option or enter a custom answer.',
    'nav.prev': 'Previous question', 'nav.next': 'Next question',
    'nav.cancel': 'Dismiss all questions', 'option.recommended': 'Recommended',
    'custom.placeholder': 'Type your answer', 'action.skip': 'Skip this question',
    'action.next': 'Next', 'plan.header': 'Plan review', 'plan.approve': 'Approve',
    'plan.decline': 'Refuse', 'plan.discuss': 'Chat about it',
    'submit': 'Submit', 'submitting': 'Submitting',
  })[key] ?? key
}

export function makeQuestionBench(kind = 'generic') {
  const hooks = reactHooks()
  const responses = []
  const matched = {
    kind: 'question', key: 'question-1', sessionId: 'session-1',
    payload: { questions: kind === 'plan' ? [{
      id: 'plan', question: 'Review plan?', detail: '# Plan\nDo it',
      options: [
        { label: 'Ship it', description: 'Execute the plan' },
        { label: 'No thanks', description: 'Reject the plan' },
      ],
      intent: { kind: 'plan-review', approve: 'Ship it' },
    }] : [
      { id: 'one', question: 'Choose one', options: [{ label: 'Alpha (Recommended)', description: 'A' }, { label: 'Beta' }] },
      { id: 'many', question: 'Choose many', options: [{ label: 'Red' }, { label: 'Blue' }], multiSelect: true },
    ] },
    respond(result) { responses.push(result); return Promise.resolve({ accepted: true }) },
  }
  return {
    hooks, React: hooks.React,
    primitives: {
      Button: 'Button', MarkdownText: 'MarkdownText', IconCheckOutline14: 'IconCheckOutline14',
      IconChevronLeftOutline14: 'IconChevronLeftOutline14', IconChevronRightOutline14: 'IconChevronRightOutline14',
      IconCloseOutline16: 'IconCloseOutline16', IconEditOutline16: 'IconEditOutline16',
    },
    props: { matched, t: translations }, matched, responses,
  }
}

export function questionRender(bench, component) { bench.hooks.reset(); return component(bench.props) }
export function questionText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(questionText).join('')
  if (node.kind === 'MarkdownText') return node.props?.text ?? ''
  return (node.children ?? []).map(questionText).join('')
}
export function questionFind(node, property, value) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (Array.isArray(node)) {
    for (const child of node) { const found = questionFind(child, property, value); if (found !== undefined) return found }
    return undefined
  }
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) { const found = questionFind(child, property, value); if (found !== undefined) return found }
  return undefined
}
export function questionFindText(node, value) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (!Array.isArray(node) && questionText(node) === value) return node
  for (const child of Array.isArray(node) ? node : node.children ?? []) {
    const found = questionFindText(child, value); if (found !== undefined) return found
  }
  return undefined
}
export function questionInvoke(node, property, event) { return event === undefined ? node.props[property]() : node.props[property](event) }
export function questionChange(value) { return { target: { value } } }
export function questionKey(key, options = {}) {
  return { key, shiftKey: options.shiftKey ?? false, nativeEvent: { isComposing: options.isComposing ?? false, keyCode: options.keyCode ?? 0 }, preventDefault() {} }
}
export function questionResponses(bench) { return bench.responses }
export function questionReplaceCarrier(bench, key) { bench.props.matched = { ...bench.matched, key } }
export function questionTick() { return Promise.resolve().then(() => Promise.resolve()) }

export function makeQuestionPluginBench() {
  const effects = []
  const entries = []
  const ctx = {
    effect(setup) { const dispose = setup(); effects.push(dispose); return dispose },
    locale: { register() { return () => {} } },
  }
  ctx.slots = {
    inject(name, install) { const dispose = install(); effects.push(dispose); return dispose },
    register(options, component) {
      const entry = { options, component }; entries.push(entry)
      return () => entries.splice(entries.indexOf(entry), 1)
    },
  }
  return { ctx, effects, entries }
}
export function questionPluginEntries(bench) { return bench.entries }
export function questionPluginDispose(bench) { [...bench.effects].reverse().forEach(dispose => dispose()) }
export function questionSelect(entry, owner) { return entry.options.select(owner) }
"#)]
extern "C" {
    fn makeQuestionBench(kind: &str) -> JsValue;
    fn questionRender(bench: &JsValue, component: &Function) -> JsValue;
    fn questionText(node: &JsValue) -> String;
    fn questionFind(node: &JsValue, property: &str, value: &JsValue) -> JsValue;
    fn questionFindText(node: &JsValue, value: &str) -> JsValue;
    fn questionInvoke(node: &JsValue, property: &str, event: &JsValue) -> JsValue;
    fn questionChange(value: &str) -> JsValue;
    fn questionKey(key: &str, options: &JsValue) -> JsValue;
    fn questionResponses(bench: &JsValue) -> Array;
    fn questionReplaceCarrier(bench: &JsValue, key: &str);
    fn questionTick() -> Promise;
    fn makeQuestionPluginBench() -> JsValue;
    fn questionPluginEntries(bench: &JsValue) -> Array;
    fn questionPluginDispose(bench: &JsValue);
    fn questionSelect(entry: &JsValue, owner: &JsValue) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn component(bench: &JsValue) -> Function {
    configure_client_ui_user_questions(property(bench, "React"), property(bench, "primitives"))
        .unwrap();
    exported_question_composer_component()
        .unwrap()
        .dyn_into()
        .unwrap()
}

#[wasm_bindgen_test(async)]
async fn generic_flow_submits_one_exact_batch_and_same_key_keeps_drafts() {
    let bench = makeQuestionBench("generic");
    let component = component(&bench);
    let first = questionRender(&bench, &component);
    assert!(questionText(&first).contains("Alpha"));
    assert!(questionText(&first).contains("Recommended"));
    let alpha = questionFind(&first, "aria-label", &JsValue::from_str("Alpha"));
    questionInvoke(&alpha, "onClick", &JsValue::UNDEFINED);

    let second = questionRender(&bench, &component);
    assert!(questionText(&second).contains("2 / 2"));
    let red = questionFind(&second, "aria-label", &JsValue::from_str("Red"));
    questionInvoke(&red, "onClick", &JsValue::UNDEFINED);
    let selected = questionRender(&bench, &component);
    let selected_red = questionFind(&selected, "aria-checked", &JsValue::TRUE);
    assert!(questionText(&selected_red).contains("Red"));
    questionReplaceCarrier(&bench, "question-1");
    let replay = questionRender(&bench, &component);
    let replay_red = questionFind(&replay, "aria-checked", &JsValue::TRUE);
    assert!(questionText(&replay_red).contains("Red"));
    let submit = questionFindText(&replay, "Submit");
    questionInvoke(&submit, "onClick", &JsValue::UNDEFINED);
    JsFuture::from(questionTick()).await.unwrap();
    let responses = questionResponses(&bench);
    assert_eq!(responses.length(), 1);
    let response = responses.get(0);
    assert_eq!(property(&response, "ok").as_bool(), Some(true));
    let answer = property(&property(&response, "value"), "answer");
    let answers = Array::from(&property(&answer, "answers"));
    assert_eq!(answers.length(), 2);
    assert_eq!(
        Array::from(&property(&answers.get(0), "selected"))
            .get(0)
            .as_string()
            .as_deref(),
        Some("Alpha (Recommended)")
    );
    assert_eq!(
        Array::from(&property(&answers.get(1), "selected"))
            .get(0)
            .as_string()
            .as_deref(),
        Some("Red")
    );
}

#[wasm_bindgen_test(async)]
async fn plan_review_uses_markdown_and_asker_owned_labels() {
    let bench = makeQuestionBench("plan");
    let component = component(&bench);
    let first = questionRender(&bench, &component);
    assert!(questionText(&first).contains("# Plan\nDo it"));
    assert!(!questionText(&first).contains("Skip this question"));
    let approve = questionFind(&first, "title", &JsValue::from_str("Execute the plan"));
    assert!(questionText(&approve).contains("Approve"));
    questionInvoke(&approve, "onClick", &JsValue::UNDEFINED);
    JsFuture::from(questionTick()).await.unwrap();
    let response = questionResponses(&bench).get(0);
    let value = property(&response, "value");
    let answer = property(&value, "answer");
    let answers = Array::from(&property(&answer, "answers"));
    assert_eq!(answers.length(), 1);
    assert_eq!(
        Array::from(&property(&answers.get(0), "selected"))
            .get(0)
            .as_string()
            .as_deref(),
        Some("Ship it")
    );
}

#[wasm_bindgen_test]
fn browser_plugin_registers_selector_and_disposes() {
    let bench = makeQuestionPluginBench();
    let ctx = property(&bench, "ctx");
    let ui = makeQuestionBench("generic");
    configure_client_ui_user_questions(property(&ui, "React"), property(&ui, "primitives"))
        .unwrap();
    apply_client_ui_user_questions(ctx).unwrap();
    assert_eq!(user_questions_inject().length(), 2);
    let entries = questionPluginEntries(&bench);
    assert_eq!(entries.length(), 1);
    let entry = entries.get(0);
    assert_eq!(
        property(&property(&entry, "options"), "name")
            .as_string()
            .as_deref(),
        Some("conversation.composer")
    );
    let interactions = Array::new();
    interactions.push(&js_sys::JSON::parse(r#"{"kind":"other"}"#).unwrap());
    let question = js_sys::JSON::parse(r#"{"kind":"question","key":"q"}"#).unwrap();
    interactions.push(&question);
    let owner = js_sys::Object::new();
    Reflect::set(&owner, &JsValue::from_str("interactions"), &interactions).unwrap();
    assert!(js_sys::Object::is(
        &questionSelect(&entry, &owner.into()),
        &question
    ));
    questionPluginDispose(&bench);
    assert_eq!(questionPluginEntries(&bench).length(), 0);
}
