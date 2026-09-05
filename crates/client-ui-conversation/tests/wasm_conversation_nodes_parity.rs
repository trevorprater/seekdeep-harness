//! Live browser registry, assembler, and normalized Chat snapshot parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Map, Object, Reflect};
use seekdeep_client_runtime::{
    WasmConversationEventRegistry, WasmConversationNodeAssembler, WasmConversationViewRegistry,
};
use seekdeep_client_ui_conversation::register_conversation_nodes_browser;
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
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
export function conversationNodeOf(snapshot, kind, turn) {
  return snapshot.nodes.values().find(node =>
    node.kind === kind && (turn === undefined || node.data?.turn === turn))
}
export function conversationCall(value, method, ...args) { return value[method](...args) }
"#)]
extern "C" {
    fn makeConversationNodesBench() -> JsValue;
    fn disposeConversationNodeEffects(bench: &JsValue);
    fn conversationNodeOf(snapshot: &JsValue, kind: &str, turn: &JsValue) -> JsValue;
    fn conversationCall(
        value: &JsValue,
        method: &str,
        first: &JsValue,
        second: &JsValue,
    ) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
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
              {"event":{"seq":15,"time":1015,"type":"command/run","data":{"commandId":"manual","name":"compact"}}},
              {"event":{"seq":16,"time":1016,"type":"compaction/start","data":{"compactionId":"manual-compaction","sourceCommandId":"manual","turn":null}}},
              {"event":{"seq":17,"time":1017,"type":"compaction/summary","data":{"compactionId":"manual-compaction","sourceCommandId":"manual","summary":[{"type":"text","text":"manual summary"}],"shadowedSeqs":[1,2],"shadowedTokenCount":50}}},
              {"event":{"seq":18,"time":1018,"type":"user/message","surfaceOp":{"op":"replace","start":1,"end":2},"data":{"id":"manual-checkpoint","source":{"kind":"plugin","plugin":"compact","compactionId":"manual-compaction","sourceCommandId":"manual"},"content":[]}}},
              {"event":{"seq":19,"time":1019,"type":"command/done","data":{"commandId":"manual","kind":"success","sourceEventSeq":17}}},
              {"event":{"seq":20,"time":1020,"type":"compaction/summary","data":{"compactionId":"automatic","summary":[{"type":"text","text":"automatic summary"}],"shadowedSeqs":[3],"shadowedTokenCount":25}}},
              {"event":{"seq":21,"time":1021,"type":"user/message","surfaceOp":{"op":"replace","start":3,"end":3},"data":{"id":"automatic-checkpoint","source":{"kind":"plugin","plugin":"compact","compactionId":"automatic"},"content":[]}}},
              {"event":{"seq":22,"time":1022,"type":"turn/start","data":{"turn":3}}},
              {"event":{"seq":23,"time":1023,"type":"step/start","data":{"turn":3,"step":1}}},
              {"event":{"seq":24,"time":1024,"type":"assistant/chunk","data":{"turn":3,"step":1,"chunk":{"type":"text-delta","index":0,"text":"stream"}}}},
              {"event":{"seq":25,"time":1025,"type":"assistant/message","surfaceOp":"append","data":{"turn":3,"step":1,"message":{"id":"assistant","content":[{"type":"text","text":"answer"}],"source":{"kind":"model","provider":"fake","model":"fake"}},"usage":{"inputTokens":5,"outputTokens":2}}}},
              {"event":{"seq":26,"time":1026,"type":"tool/call","data":{"turn":3,"step":1,"callId":"root","name":"read","arguments":"{}"}}},
              {"event":{"seq":27,"time":1027,"type":"tool/result","surfaceOp":"append","data":{"turn":3,"step":1,"message":{"source":{"kind":"tool","callId":"root"},"content":[{"type":"tool-result","toolCallId":"root","isError":false,"content":[{"type":"text","text":"done"}]}]},"meta":null}}},
              {"event":{"seq":28,"time":1028,"type":"step/end","data":{"turn":3,"step":1}}},
              {"event":{"seq":29,"time":1029,"type":"turn/end","data":{"turn":3,"reason":{"kind":"completed"}}}}
            ]"#,
        )
        .unwrap(),
    )
}

fn input(value: &str) -> JsValue {
    js_sys::JSON::parse(value).unwrap()
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One live matrix crosses registry, assembler, and snapshot faces.
fn compiled_definitions_and_chat_builder_cross_the_live_runtime_boundary() {
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
    Reflect::set(
        &context,
        &JsValue::from_str("conversationViews"),
        &views.face_for(context.clone()).unwrap(),
    )
    .unwrap();
    register_conversation_nodes_browser(context).unwrap();

    assert_eq!(events.entries().length(), 11);
    assert_eq!(views.entries().length(), 1);
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

    let nodes = property(&snapshot, "nodes");
    assert!(property(&nodes, "get").is_function());
    let values = property(&nodes, "values")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&nodes)
        .unwrap();
    assert!(Array::from(&values).length() >= 9);
    assert!(Array::is_array(&property(&snapshot, "order")));

    let steering = conversationNodeOf(&snapshot, "steering", &JsValue::UNDEFINED);
    assert_eq!(
        property(&property(&steering, "data"), "messageId")
            .as_string()
            .as_deref(),
        Some("steer")
    );
    let retry = property(
        &conversationNodeOf(&snapshot, "model-retry", &JsValue::UNDEFINED),
        "data",
    );
    let attempts = Array::from(&property(&retry, "attempts"));
    assert_eq!(
        property(&attempts.get(1), "retryState")
            .as_string()
            .as_deref(),
        Some("cancelled")
    );
    assert!(conversationNodeOf(&snapshot, "turn-error", &JsValue::UNDEFINED).is_undefined());

    let manual = property(
        &conversationNodeOf(&snapshot, "manual-compaction", &JsValue::UNDEFINED),
        "data",
    );
    assert_eq!(
        property(&property(&manual, "compaction"), "summary")
            .as_string()
            .as_deref(),
        Some("manual summary")
    );
    assert_eq!(
        property(
            &property(
                &conversationNodeOf(&snapshot, "compaction", &JsValue::UNDEFINED),
                "data",
            ),
            "summary",
        )
        .as_string()
        .as_deref(),
        Some("automatic summary")
    );

    let assistant = conversationNodeOf(&snapshot, "assistant-step", &JsValue::from_f64(3.0));
    assert_eq!(
        property(&property(&assistant, "data"), "status")
            .as_string()
            .as_deref(),
        Some("settled")
    );
    let tool = property(
        &conversationNodeOf(&snapshot, "tool-call", &JsValue::UNDEFINED),
        "data",
    );
    assert_eq!(
        property(&property(&tool, "root"), "kind")
            .as_string()
            .as_deref(),
        Some("tool-result")
    );
    let tail = property(
        &conversationNodeOf(&snapshot, "turn-tail", &JsValue::from_f64(3.0)),
        "data",
    );
    assert_eq!(property(&tail, "branchUnavailable").as_bool(), Some(true));

    let locations = property(&snapshot, "locations");
    let step_keys = Array::from(&conversationCall(
        &locations,
        "getStep",
        &JsValue::from_f64(3.0),
        &JsValue::from_f64(1.0),
    ));
    assert!(step_keys.length() >= 2);
    let timeline = property(&snapshot, "timeline");
    let turns = property(&timeline, "turns").dyn_into::<Map>().unwrap();
    let turn = turns.get(&JsValue::from_f64(3.0));
    assert!(property(&turn, "data").is_object());
    assert!(property(&property(&snapshot, "legacy"), "turnTimings").is_instance_of::<Map>());

    assembler
        .append(input(
            r#"{"event":{"seq":30,"time":1030,"type":"tool/code-dispatch-start","data":{"rootCallId":"root","parentCallId":"root","subCallId":"child-1","name":"read","arguments":{"path":"one"}}}}"#,
        ))
        .unwrap();
    assembler.flush().unwrap();
    let with_first_child = assembler.get("chat").unwrap();
    let first_tool = property(
        &conversationNodeOf(&with_first_child, "tool-call", &JsValue::UNDEFINED),
        "data",
    );
    let first_child = Array::from(&property(&property(&first_tool, "root"), "subCalls")).get(0);
    let stable_store = property(&with_first_child, "nodes");
    let stable_order = property(&with_first_child, "order");
    let stable_locations = property(&with_first_child, "locations");
    let stable_step_keys = conversationCall(
        &stable_locations,
        "getStep",
        &JsValue::from_f64(3.0),
        &JsValue::from_f64(1.0),
    );
    let stable_timeline = property(&with_first_child, "timeline");
    let stable_assistant =
        conversationNodeOf(&with_first_child, "assistant-step", &JsValue::from_f64(3.0));

    assembler
        .append(input(
            r#"{"event":{"seq":31,"time":1031,"type":"tool/code-dispatch-start","data":{"rootCallId":"root","parentCallId":"root","subCallId":"child-2","name":"read","arguments":{"path":"two"}}}}"#,
        ))
        .unwrap();
    assembler.flush().unwrap();
    let with_second_child = assembler.get("chat").unwrap();
    assert!(Object::is(
        &stable_store,
        &property(&with_second_child, "nodes")
    ));
    assert!(Object::is(
        &stable_order,
        &property(&with_second_child, "order")
    ));
    let second_locations = property(&with_second_child, "locations");
    assert!(Object::is(&stable_locations, &second_locations));
    assert!(!Object::is(
        &stable_step_keys,
        &conversationCall(
            &second_locations,
            "getStep",
            &JsValue::from_f64(3.0),
            &JsValue::from_f64(1.0)
        )
    ));
    assert!(Object::is(
        &stable_timeline,
        &property(&with_second_child, "timeline")
    ));
    assert!(Object::is(
        &stable_assistant,
        &conversationNodeOf(
            &with_second_child,
            "assistant-step",
            &JsValue::from_f64(3.0)
        )
    ));
    let second_tool = property(
        &conversationNodeOf(&with_second_child, "tool-call", &JsValue::UNDEFINED),
        "data",
    );
    let retained_child = Array::from(&property(&property(&second_tool, "root"), "subCalls")).get(0);
    assert!(Object::is(&first_child, &retained_child));

    disposeConversationNodeEffects(&bench);
    assert_eq!(events.entries().length(), 0);
    assert_eq!(views.entries().length(), 0);
    assert!(events.fallback_entry().is_undefined());
}

#[wasm_bindgen_test]
fn assistant_settlement_preserves_store_order_and_business_key() {
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
    Reflect::set(
        &context,
        &JsValue::from_str("conversationViews"),
        &views.face_for(context.clone()).unwrap(),
    )
    .unwrap();
    register_conversation_nodes_browser(context).unwrap();
    let mut assembler = WasmConversationNodeAssembler::new(&events, &views);
    assembler
        .replace_window(
            Array::from(
                &js_sys::JSON::parse(
                    r#"[
                      {"event":{"seq":1,"time":1001,"type":"turn/start","data":{"turn":1}}},
                      {"event":{"seq":2,"time":1002,"type":"step/start","data":{"turn":1,"step":1}}},
                      {"event":{"seq":3,"time":1003,"type":"assistant/chunk","data":{"turn":1,"step":1,"chunk":{"type":"text-delta","index":0,"text":"partial"}}}}
                    ]"#,
                )
                .unwrap(),
            ),
            false,
        )
        .unwrap();
    assembler.flush().unwrap();
    let running = assembler.get("chat").unwrap();
    let store = property(&running, "nodes");
    let order = property(&running, "order");
    let running_node = conversationNodeOf(&running, "assistant-step", &JsValue::from_f64(1.0));
    let key = property(&running_node, "key");

    assembler
        .append(input(
            r#"{"event":{"seq":4,"time":1004,"type":"assistant/message","surfaceOp":"append","data":{"turn":1,"step":1,"message":{"id":"assistant","content":[{"type":"text","text":"settled"}]}}}}"#,
        ))
        .unwrap();
    assembler.flush().unwrap();
    let settled = assembler.get("chat").unwrap();
    let settled_node = conversationNodeOf(&settled, "assistant-step", &JsValue::from_f64(1.0));
    assert!(Object::is(&store, &property(&settled, "nodes")));
    assert!(Object::is(&order, &property(&settled, "order")));
    assert_eq!(property(&settled_node, "key"), key);
    assert_eq!(
        property(&property(&settled_node, "data"), "status")
            .as_string()
            .as_deref(),
        Some("settled")
    );
    disposeConversationNodeEffects(&bench);
}
