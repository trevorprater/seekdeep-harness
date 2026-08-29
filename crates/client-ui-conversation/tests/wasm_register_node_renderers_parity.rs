//! Live WASM coverage for keyed chat renderer registration.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Reflect};
use seekdeep_client_ui_conversation::register_chat_node_renderers_browser;
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let injectCalls = []
let registerCalls = []
export function installRendererBench() {
  injectCalls = []; registerCalls = []
  const slots = {
    inject(name, callback) {
      const call = { name, receiver: this === slots }
      injectCalls.push(call)
      call.result = callback()
      return call.result
    },
    register(options, component) {
      const disposer = { dispose: registerCalls.length }
      registerCalls.push({ options, component, disposer, receiver: this === slots })
      return disposer
    },
  }
  return { context: { slots }, slots }
}
export function rendererComponents() {
  const names = [
    'UserMessageNodeView', 'ContextMessageNodeView', 'AssistantNodeView', 'CommandNodeView',
    'ManualCompactionNodeView', 'CompactionNodeView', 'RetryNodeView', 'TurnErrorNodeView',
    'TurnMaxTokensNodeView', 'TurnTailNodeView', 'UnknownNodeView',
  ]
  return Object.fromEntries(names.map(name => [name, { name }]))
}
export function rendererInjectCalls() { return injectCalls }
export function rendererRegisterCalls() { return registerCalls }
export function rendererDelete(value, key) { delete value[key]; return value }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installRendererBench)]
    fn install_renderer_bench() -> JsValue;
    #[wasm_bindgen(js_name = rendererComponents)]
    fn renderer_components() -> JsValue;
    #[wasm_bindgen(js_name = rendererInjectCalls)]
    fn renderer_inject_calls() -> Array;
    #[wasm_bindgen(js_name = rendererRegisterCalls)]
    fn renderer_register_calls() -> Array;
    #[wasm_bindgen(js_name = rendererDelete)]
    fn renderer_delete(value: &JsValue, key: &str) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
fn registrations_preserve_order_components_children_receivers_and_disposers() {
    let bench = install_renderer_bench();
    let components = renderer_components();
    register_chat_node_renderers_browser(property(&bench, "context"), components.clone()).unwrap();
    let injects = renderer_inject_calls();
    let registers = renderer_register_calls();
    assert_eq!(injects.length(), 12);
    assert_eq!(registers.length(), 12);
    let expected = [
        ("user", "UserMessageNodeView"),
        ("steering", "UserMessageNodeView"),
        ("context", "ContextMessageNodeView"),
        ("assistant-step", "AssistantNodeView"),
        ("command", "CommandNodeView"),
        ("manual-compaction", "ManualCompactionNodeView"),
        ("compaction", "CompactionNodeView"),
        ("model-retry", "RetryNodeView"),
        ("turn-error", "TurnErrorNodeView"),
        ("turn-max-tokens", "TurnMaxTokensNodeView"),
        ("turn-tail", "TurnTailNodeView"),
        ("unknown", "UnknownNodeView"),
    ];
    for (index, (key, component_name)) in expected.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        let inject = injects.get(index);
        let register = registers.get(index);
        let options = property(&register, "options");
        assert_eq!(
            property(&inject, "name").as_string().as_deref(),
            Some("conversation.chat.node")
        );
        assert_eq!(property(&inject, "receiver").as_bool(), Some(true));
        assert_eq!(property(&register, "receiver").as_bool(), Some(true));
        assert_eq!(property(&options, "key").as_string().as_deref(), Some(*key));
        assert_eq!(
            property(&options, "name").as_string().as_deref(),
            Some("conversation.chat.node")
        );
        assert_eq!(
            property(&options, "locale").as_string().as_deref(),
            Some("conversation")
        );
        assert!(Object::is(
            &property(&register, "component"),
            &property(&components, component_name)
        ));
        assert!(Object::is(
            &property(&inject, "result"),
            &property(&register, "disposer")
        ));
        if !matches!(*key, "command" | "turn-tail") {
            assert!(property(&options, "children").is_undefined());
        }
    }
    assert!(!expected.iter().any(|(key, _)| *key == "tool-call"));

    let command_children = property(&property(&registers.get(4), "options"), "children");
    let command = property(&command_children, "conversation.chat.commandview");
    assert_eq!(
        property(&command, "kind").as_string().as_deref(),
        Some("keyed")
    );
    assert_eq!(
        property(&command, "scope").as_string().as_deref(),
        Some("session")
    );

    let tail_children = property(&property(&registers.get(10), "options"), "children");
    let tail = property(&tail_children, "conversation.chat.turnTail");
    let actions = property(&tail_children, "conversation.chat.assistant-actions");
    assert_eq!(
        property(&tail, "kind").as_string().as_deref(),
        Some("chain")
    );
    assert_eq!(
        property(&actions, "kind").as_string().as_deref(),
        Some("list")
    );
    assert_eq!(
        property(&tail, "scope").as_string().as_deref(),
        Some("session")
    );
    assert_eq!(
        property(&actions, "scope").as_string().as_deref(),
        Some("session")
    );
}

#[wasm_bindgen_test]
fn missing_component_fails_before_any_registration() {
    let bench = install_renderer_bench();
    let components = renderer_delete(&renderer_components(), "TurnTailNodeView");
    let error =
        register_chat_node_renderers_browser(property(&bench, "context"), components).unwrap_err();
    assert_eq!(
        property(&error, "message").as_string().as_deref(),
        Some("renderer components omitted TurnTailNodeView")
    );
    assert_eq!(renderer_inject_calls().length(), 0);
    assert_eq!(renderer_register_calls().length(), 0);
}
