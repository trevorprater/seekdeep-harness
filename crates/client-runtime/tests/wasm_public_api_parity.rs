//! Live browser public helper and `PendingWait` compatibility parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Map, Object, Promise, Reflect};
use seekdeep_client_runtime::{
    WasmConversationEventRegistry, WasmConversationLocationIndex, WasmConversationNodeAssembler,
    WasmConversationViewRegistry, WasmPendingWait, conversation_context_key_js,
    display_failure_message_js, empty_assistant_block_js, empty_chat_snapshot_js,
    empty_conversation_views_js, index_subagent_descendants_js, is_append_surface_event_js,
    is_replacement_surface_event_js, is_token_delta_js, resolve_workspace_path_js,
    workspace_title_of_js,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
fn surface_context_path_block_token_lineage_and_failure_helpers_match_source() {
    let event = Object::new();
    set(&event, "type", &JsValue::from_str("user/message"));
    set(&event, "surfaceOp", &JsValue::from_str("append"));
    assert!(is_append_surface_event_js(event.clone().into()).unwrap());
    assert!(!is_replacement_surface_event_js(event.clone().into()).unwrap());
    set(&event, "surfaceOp", &Object::new());
    assert!(is_replacement_surface_event_js(event.into()).unwrap());
    assert_eq!(conversation_context_key_js("🧠", "id"), "2:🧠id");
    assert_eq!(workspace_title_of_js("C:\\work\\name\\"), "name");
    assert_eq!(
        resolve_workspace_path_js(Some("C:\\work\\".to_owned()), "\\src\\lib.rs"),
        "C:\\work/src\\lib.rs"
    );
    assert_eq!(
        get(&empty_assistant_block_js("tool-call").unwrap(), "kind")
            .as_string()
            .as_deref(),
        Some("tool-call")
    );
    let chunk = Object::new();
    set(&chunk, "type", &JsValue::from_str("tool-call-delta"));
    set(&chunk, "argumentsDelta", &JsValue::from_str(""));
    assert!(!is_token_delta_js(chunk.clone().into()).unwrap());
    set(&chunk, "name", &JsValue::from_str("read"));
    assert!(is_token_delta_js(chunk.into()).unwrap());

    let summaries = Object::new();
    let root = Object::new();
    set(&root, "id", &JsValue::from_str("root"));
    set(&root, "running", &JsValue::FALSE);
    set(&summaries, "root", &root);
    let child = Object::new();
    set(&child, "id", &JsValue::from_str("child"));
    set(&child, "parentId", &JsValue::from_str("root"));
    set(&child, "origin", &JsValue::from_str("subagent"));
    set(&child, "running", &JsValue::TRUE);
    set(&summaries, "child", &child);
    let descendants: Map = index_subagent_descendants_js(summaries.into()).unwrap();
    let aggregate = descendants.get(&JsValue::from_str("root"));
    assert_eq!(get(&aggregate, "count").as_f64(), Some(1.0));
    assert_eq!(get(&aggregate, "runningCount").as_f64(), Some(1.0));

    let failure = Object::new();
    set(&failure, "code", &JsValue::from_str("AUTH"));
    set(
        &failure,
        "message",
        &JsValue::from_str("key sk-secret failed"),
    );
    assert_eq!(
        display_failure_message_js(failure.into()).unwrap(),
        "API key is invalid"
    );
    let chat = empty_chat_snapshot_js().unwrap();
    assert_eq!(
        get(&chat, "order")
            .dyn_into::<js_sys::Array>()
            .unwrap()
            .length(),
        0
    );
    let views = empty_conversation_views_js().unwrap();
    assert!(
        get(&views, "get")
            .dyn_into::<Function>()
            .unwrap()
            .call1(&views, &JsValue::from_str("chat"))
            .unwrap()
            .is_undefined()
    );
}

#[wasm_bindgen_test(async)]
async fn pending_wait_backfills_rpc_id_and_fails_synchronously_after_settlement() {
    let captured = Rc::new(RefCell::new(JsValue::UNDEFINED));
    let observed = captured.clone();
    let responder = Closure::wrap(Box::new(move |message: JsValue| {
        *observed.borrow_mut() = message;
        Promise::resolve(&JsValue::UNDEFINED)
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let payload = Object::new();
    set(&payload, "approvalId", &JsValue::from_str("approval"));
    let wait = WasmPendingWait::new(
        "approval".to_owned(),
        "rpc-1".to_owned(),
        "session-1".to_owned(),
        payload.into(),
        responder.into_js_value().unchecked_into(),
    )
    .unwrap();
    assert_eq!(wait.kind(), "approval");
    assert_eq!(wait.key(), "a:rpc-1");
    assert_eq!(wait.session_id(), "session-1");
    let result = Object::new();
    set(&result, "ok", &JsValue::TRUE);
    JsFuture::from(wait.respond(result.into()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        get(&captured.borrow(), "type").as_string().as_deref(),
        Some("client-response")
    );
    assert_eq!(
        get(&captured.borrow(), "rpcId").as_string().as_deref(),
        Some("rpc-1")
    );
    wait.mark_settled();
    assert!(wait.respond(Object::new().into()).is_err());
}

#[wasm_bindgen_test]
fn public_conversation_assembler_and_location_index_constructors_drive_rust_cores() {
    let events = WasmConversationEventRegistry::new();
    let views = WasmConversationViewRegistry::new();
    let view = Object::new();
    set(&view, "target", &JsValue::from_str("probe"));
    set(
        &view,
        "create",
        &Function::new_no_args(
            "return { empty: { count: 0 }, replace({ nodes }) { return { count: nodes.length } }, apply({ upserts }) { return { count: upserts.length } } }",
        ),
    );
    views.register(view.into()).unwrap();
    let definition = Object::new();
    set(&definition, "kind", &JsValue::from_str("probe"));
    set(&definition, "target", &JsValue::from_str("probe"));
    set(
        &definition,
        "match",
        &Function::new_with_args(
            "event",
            "return event.type === 'probe' ? { id: 'one', role: 'start' } : null",
        ),
    );
    set(
        &definition,
        "start",
        &Function::new_with_args("context, match, reader", "return { ok: true }"),
    );
    set(
        &definition,
        "update",
        &Function::new_with_args("context, match", "return context.state"),
    );
    set(
        &definition,
        "buildViewNode",
        &Function::new_with_args(
            "context",
            "return { key: context.key, kind: context.kind, id: context.id, target: 'probe', data: context.state }",
        ),
    );
    events.register(definition.into()).unwrap();
    let event = Object::new();
    set(&event, "seq", &JsValue::from_f64(1.0));
    set(&event, "time", &JsValue::from_f64(1.0));
    set(&event, "type", &JsValue::from_str("probe"));
    set(&event, "data", &Object::new());
    let entry = Object::new();
    set(&entry, "event", &event);
    let entries = Array::new();
    entries.push(&entry);
    let mut assembler = WasmConversationNodeAssembler::new(&events, &views);
    assert_eq!(
        assembler.replace_window(entries, false).unwrap(),
        "immediate"
    );
    assert!(assembler.flush().unwrap());
    assert_eq!(
        get(&assembler.get("probe").unwrap(), "count").as_f64(),
        Some(1.0)
    );

    let turn = Object::new();
    set(&turn, "seq", &JsValue::from_f64(2.0));
    set(&turn, "time", &JsValue::from_f64(2.0));
    set(&turn, "type", &JsValue::from_str("turn/start"));
    let data = Object::new();
    set(&data, "turn", &JsValue::from_f64(0.0));
    set(&turn, "data", &data);
    let turn_entry = Object::new();
    set(&turn_entry, "event", &turn);
    let turn_entries = Array::new();
    turn_entries.push(&turn_entry);
    let mut locations = WasmConversationLocationIndex::new();
    assert_eq!(locations.rebuild(turn_entries).unwrap().size(), 1);
    assert_eq!(
        get(&locations.location_of(turn.into()).unwrap(), "kind")
            .as_string()
            .as_deref(),
        Some("turn")
    );
}

#[wasm_bindgen_test]
fn public_chat_assembly_retains_lossless_event_views_and_reuses_unchanged_data() {
    let events = WasmConversationEventRegistry::new();
    let views = WasmConversationViewRegistry::new();
    let view = Function::new_no_args(
        r"
        const snapshot = nodes => ({
            encoding: 'seekdeep-chat-v1', order: nodes.map(node => node.key), nodes,
            timeline: { turnOrder: [], turns: [] }, locations: { turns: [], steps: [] },
            legacy: { nodes: [], turnTimings: [], turnEnds: [], partial: null, runningCalls: [] }
        });
        return { target: 'chat', create: () => ({
            empty: snapshot([]), replace: ({nodes}) => snapshot(nodes),
            apply: ({upserts}) => snapshot(upserts)
        }) };
    ",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    let _remove_view = views.register(view).unwrap();
    let definition = Function::new_no_args(r"
        return {
            kind: 'lossless', target: 'chat',
            match: event => ({ id: 'one', role: event.seq === 1 ? 'start' : 'update' }),
            start: (_, match) => ({ event: match.event.data, view: match.view, extra: match.event.extra }),
            update: (context, match) => ({ ...context.state, event: match.event.data, view: match.view }),
            buildViewNode: context => ({
                key: context.key, kind: context.kind, id: context.id, target: 'chat',
                anchorSeq: 1, visibility: 'visible', location: {kind: 'session'}, data: context.state
            })
        };
    ").call0(&JsValue::UNDEFINED).unwrap();
    let _remove_definition = events.register(definition).unwrap();
    let input = js_sys::JSON::parse(
        r#"{"event":{"seq":1,"time":1,"type":"lossless","extra":"\ud800","data":{"text":"\ud800","keep":{"\udfff":"\ud800"}}},"view":{"\ud800":"\udfff"}}"#,
    ).unwrap();
    let entries = Array::new();
    entries.push(&input);
    let mut assembler = WasmConversationNodeAssembler::new(&events, &views);
    assembler.replace_window(entries, false).unwrap();
    assembler.flush().unwrap();
    let first = assembler.get("chat").unwrap();
    let first_nodes = get(&first, "nodes");
    let values = get(&first_nodes, "values").dyn_into::<Function>().unwrap();
    let first_node = Array::from(&values.call0(&first_nodes).unwrap()).get(0);
    let first_data = get(&first_node, "data");
    assert_eq!(
        js_sys::JSON::stringify(&get(&first_data, "event"))
            .unwrap()
            .as_string(),
        js_sys::JSON::stringify(&get(&get(&input, "event"), "data"))
            .unwrap()
            .as_string()
    );
    assert_eq!(
        js_sys::JSON::stringify(&get(&first_data, "view"))
            .unwrap()
            .as_string()
            .unwrap(),
        r#"{"\ud800":"\udfff"}"#
    );
    assert_eq!(
        js_sys::JSON::stringify(&get(&first_data, "extra"))
            .unwrap()
            .as_string()
            .unwrap(),
        r#""\ud800""#
    );
    let next_input = js_sys::JSON::parse(
        r#"{"event":{"seq":2,"time":2,"type":"lossless","data":{"text":"\ud800\udc00","keep":{"\udfff":"\ud800"}}},"view":{"\ud800":"\udfff"}}"#,
    ).unwrap();
    assembler.append(next_input).unwrap();
    assembler.flush().unwrap();
    let next = assembler.get("chat").unwrap();
    let next_nodes = get(&next, "nodes");
    let next_node = Array::from(&values.call0(&next_nodes).unwrap()).get(0);
    let next_data = get(&next_node, "data");
    assert!(Object::is(
        &get(&first_data, "view"),
        &get(&next_data, "view")
    ));
    assert!(Object::is(
        &get(&get(&first_data, "event"), "keep"),
        &get(&get(&next_data, "event"), "keep")
    ));
    assert!(Object::is(&next, &assembler.get("chat").unwrap()));
}

#[wasm_bindgen_test]
fn failure_display_preserves_raw_messages_and_redacts_auth_before_rendering() {
    for (input, expected) in [
        (r#""\ud800""#, r#""\ud800""#),
        (
            r#"{"message":"\udfff","extra":{"\ud800":"\udfff"}}"#,
            r#""\udfff""#,
        ),
        (
            r#"{"code":"AUTH","message":"\ud800"}"#,
            r#""API key is invalid""#,
        ),
        (r#"{"\ud800":"\udfff"}"#, r#""{\"\\ud800\":\"\\udfff\"}""#),
    ] {
        let failure = js_sys::JSON::parse(input).unwrap();
        let message = display_failure_message_js(failure).unwrap();
        assert_eq!(
            js_sys::JSON::stringify(&message)
                .unwrap()
                .as_string()
                .as_deref(),
            Some(expected)
        );
    }
    for (value, expected) in [
        (JsValue::UNDEFINED, "undefined"),
        (JsValue::from_f64(f64::NAN), "NaN"),
        (JsValue::from_f64(f64::INFINITY), "Infinity"),
    ] {
        assert_eq!(display_failure_message_js(value).unwrap(), expected);
    }
    let failure = Object::new();
    set(&failure, "message", &JsValue::UNDEFINED);
    assert_eq!(display_failure_message_js(failure.into()).unwrap(), "{}");
}
