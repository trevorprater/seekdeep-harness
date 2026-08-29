//! Live browser registry and assembler parity for compiled conversation definitions.

#![cfg(target_arch = "wasm32")]

use std::rc::Rc;

use indexmap::IndexMap;
use js_sys::{Array, Reflect};
use seekdeep_client_runtime::{
    AssemblerViewBuilder, AssemblerViewDefinition, ConversationAssemblerError,
    ConversationTimelineSnapshot, ConversationViewNode, ConversationVisibility,
    WasmConversationEventRegistry, WasmConversationNodeAssembler, WasmConversationViewRegistry,
    native_conversation_view_definition_to_js,
};
use seekdeep_client_ui_conversation::register_conversation_simple_nodes_browser;
use serde_json::{Value, json};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function makeConversationNodesBench() {
  const effects = []
  const context = {
    effect(setup, label) {
      const dispose = setup()
      effects.push({ label, dispose })
      return dispose
    },
  }
  return { context, effects }
}

export function disposeConversationNodeEffects(bench) {
  for (const effect of [...bench.effects].reverse()) effect.dispose()
}
"#)]
extern "C" {
    fn makeConversationNodesBench() -> JsValue;
    fn disposeConversationNodeEffects(bench: &JsValue);
}

#[derive(Default)]
struct ChatBuilder {
    nodes: IndexMap<String, Rc<ConversationViewNode>>,
}

impl ChatBuilder {
    fn snapshot(&self) -> Rc<Value> {
        Rc::new(Value::Array(
            self.nodes
                .values()
                .map(|node| {
                    let chat = node.chat.as_ref().expect("chat metadata");
                    json!({
                        "key": node.key,
                        "kind": node.kind,
                        "anchorSeq": chat.anchor_seq,
                        "visibility": match chat.visibility {
                            ConversationVisibility::Visible => "visible",
                            ConversationVisibility::Hidden => "hidden",
                        },
                        "data": node.data.as_ref().clone(),
                    })
                })
                .collect(),
        ))
    }
}

impl AssemblerViewBuilder for ChatBuilder {
    fn empty(&self) -> Rc<Value> {
        self.snapshot()
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        self.nodes = nodes
            .iter()
            .map(|node| (node.key.clone(), node.clone()))
            .collect();
        Ok(self.snapshot())
    }

    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        for node in upserts {
            self.nodes.insert(node.key.clone(), node.clone());
        }
        Ok(self.snapshot())
    }
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn find_node(snapshot: &JsValue, kind: &str) -> JsValue {
    Array::from(snapshot)
        .iter()
        .find(|node| property(node, "kind").as_string().as_deref() == Some(kind))
        .unwrap_or(JsValue::UNDEFINED)
}

fn live_entries() -> Array {
    Array::from(
        &js_sys::JSON::parse(
            r#"[
              {"event":{"seq":1,"time":1001,"type":"turn/start","data":{"turn":1}}},
              {"event":{"seq":2,"time":1002,"type":"step/start","data":{"turn":1,"step":1}}},
              {"event":{"seq":3,"time":1003,"type":"agent/inbox/spliced","data":{"target":"next-step","start":0,"inserted":[{"id":"steer"}]}}},
              {"event":{"seq":4,"time":1004,"type":"agent/inbox/spliced","data":{"target":"next-step","start":0,"removedCount":1,"inserted":[]}}},
              {"event":{"seq":5,"time":1005,"type":"user/message","surfaceOp":"append","data":{"id":"steer","role":"user","source":{"kind":"user"},"content":[{"type":"text","text":"change"}]}}},
              {"event":{"seq":6,"time":1006,"type":"llm/retry","data":{"retryId":"retry-1","turn":1,"step":1,"retry":1,"maxRetries":2,"delayMs":10,"failure":{"message":"first"}}}},
              {"event":{"seq":7,"time":1007,"type":"llm/retry-started","data":{"retryId":"retry-1","turn":1,"step":1,"retry":1}}},
              {"event":{"seq":8,"time":1008,"type":"llm/retry","data":{"retryId":"retry-1","turn":1,"step":1,"retry":2,"maxRetries":2,"delayMs":20,"failure":{"message":"second"}}}},
              {"event":{"seq":9,"time":1009,"type":"step/end","data":{"turn":1,"step":1}}},
              {"event":{"seq":10,"time":1010,"type":"turn/end","data":{"turn":1,"reason":{"kind":"error","error":{"message":"failed"}}}}},
              {"event":{"seq":11,"time":1011,"type":"turn/start","data":{"turn":2}}},
              {"event":{"seq":12,"time":1012,"type":"step/start","data":{"turn":2,"step":1}}},
              {"event":{"seq":13,"time":1013,"type":"step/end","data":{"turn":2,"step":1}}},
              {"event":{"seq":14,"time":1014,"type":"turn/end","data":{"turn":2,"reason":{"kind":"max-tokens"}}}},
              {"event":{"seq":15,"time":1015,"type":"tool/result","surfaceOp":"append","data":{"message":"unclaimed"}}}
            ]"#,
        )
        .unwrap(),
    )
}

#[wasm_bindgen_test]
fn compiled_definitions_cross_the_live_registry_and_assembler_boundary() {
    let bench = makeConversationNodesBench();
    let context = property(&bench, "context");
    let events = WasmConversationEventRegistry::new();
    let views = WasmConversationViewRegistry::new();
    Reflect::set(
        &context,
        &JsValue::from_str("conversationEvents"),
        &events.face_for(context.clone()).unwrap(),
    )
    .unwrap();
    register_conversation_simple_nodes_browser(context).unwrap();
    views
        .register(
            native_conversation_view_definition_to_js(AssemblerViewDefinition {
                target: "chat".to_owned(),
                create: Rc::new(|| Box::new(ChatBuilder::default())),
            })
            .unwrap(),
        )
        .unwrap();

    assert_eq!(events.entries().length(), 6);
    assert_eq!(
        property(&events.fallback_entry(), "kind")
            .as_string()
            .as_deref(),
        Some("unknown-surface")
    );
    let mut assembler = WasmConversationNodeAssembler::new(&events, &views);
    assembler.replace_window(live_entries(), false).unwrap();
    assert!(assembler.flush().unwrap());
    let snapshot = assembler.get("chat").unwrap();

    let steering = find_node(&snapshot, "steering");
    assert_eq!(
        property(&property(&steering, "data"), "messageId")
            .as_string()
            .as_deref(),
        Some("steer")
    );
    let retry = property(&find_node(&snapshot, "model-retry"), "data");
    let attempts = Array::from(&property(&retry, "attempts"));
    assert_eq!(
        property(&attempts.get(0), "retryState")
            .as_string()
            .as_deref(),
        Some("started")
    );
    assert_eq!(
        property(&attempts.get(1), "retryState")
            .as_string()
            .as_deref(),
        Some("cancelled")
    );
    assert!(find_node(&snapshot, "turn-error").is_undefined());
    assert_eq!(
        property(
            &property(&find_node(&snapshot, "turn-max-tokens"), "data"),
            "step",
        )
        .as_f64(),
        Some(1.0)
    );
    assert_eq!(
        property(&property(&find_node(&snapshot, "unknown"), "data"), "type")
            .as_string()
            .as_deref(),
        Some("tool/result")
    );

    disposeConversationNodeEffects(&bench);
    assert_eq!(events.entries().length(), 0);
    assert!(events.fallback_entry().is_undefined());
}
