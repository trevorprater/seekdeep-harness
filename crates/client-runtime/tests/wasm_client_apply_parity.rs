//! Live browser assembly and wire-sink parity for `applyClientRuntime`.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_runtime::apply_client_runtime;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[allow(clippy::needless_pass_by_value)]
fn response(value: JsValue) -> JsValue {
    let result = Object::new();
    set(&result, "ok", &JsValue::TRUE);
    set(&result, "value", &value);
    let response = Object::new();
    set(&response, "rpcId", &JsValue::from_str("fake"));
    set(&response, "result", &result);
    response.into()
}

fn api() -> JsValue {
    let api = Object::new();
    let sessions = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "items", &Array::new());
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "list", &list.into_js_value());
    let history = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "events", &Array::new());
        set(&value, "hasMore", &JsValue::FALSE);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "history", &history.into_js_value());
    let create = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "sessionId", &JsValue::from_str("fk-new"));
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "create", &create.into_js_value());
    set(&api, "sessions", &sessions);
    let subagents = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "entries", &Array::new());
        set(&value, "parentAvailable", &JsValue::FALSE);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&subagents, "list", &list.into_js_value());
    set(&api, "subagents", &subagents);
    let workspace = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "items", &Array::new());
        set(&value, "archivedSessionIds", &Array::new());
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&workspace, "list", &list.into_js_value());
    set(&api, "workspace", &workspace);
    api.into()
}

struct Bench {
    root: JsValue,
    services: Object,
    emissions: Rc<RefCell<Vec<Vec<JsValue>>>>,
    dispatches: Rc<RefCell<Vec<Vec<JsValue>>>>,
    sinks: Rc<RefCell<JsValue>>,
    cleanups: Rc<RefCell<Vec<Function>>>,
    stopped: Rc<RefCell<u64>>,
}

#[allow(clippy::too_many_lines)]
fn bench() -> Bench {
    let services = Object::new();
    let root = Object::new();
    let cleanups = Rc::new(RefCell::new(Vec::<Function>::new()));
    let cleanups_for_effect = cleanups.clone();
    let effect = Closure::wrap(Box::new(move |installer: Function, _label: JsValue| {
        let cleanup = installer.call0(&JsValue::UNDEFINED).unwrap();
        if let Ok(cleanup) = cleanup.dyn_into::<Function>() {
            cleanups_for_effect.borrow_mut().push(cleanup);
        }
        Function::new_no_args("")
    }) as Box<dyn FnMut(Function, JsValue) -> Function>);
    set(&root, "effect", &effect.into_js_value());
    let services_for_get = services.clone();
    let get_service = Closure::wrap(Box::new(move |name: String| {
        Reflect::get(&services_for_get, &JsValue::from_str(&name)).unwrap()
    }) as Box<dyn FnMut(String) -> JsValue>);
    set(&root, "get", &get_service.into_js_value());
    let services_for_provide = services.clone();
    let provide = Closure::wrap(
        Box::new(move |name: String, value: JsValue, _meta: JsValue| {
            assert!(
                Reflect::set(&services_for_provide, &JsValue::from_str(&name), &value).unwrap()
            );
        }) as Box<dyn FnMut(String, JsValue, JsValue)>,
    );
    let reflect = Object::new();
    set(&reflect, "provide", &provide.into_js_value());
    set(&root, "reflect", &reflect);

    let emissions = Rc::new(RefCell::new(Vec::new()));
    let emissions_for_emit = emissions.clone();
    let emit = Closure::wrap(Box::new(move |event: String, args: JsValue| {
        emissions_for_emit
            .borrow_mut()
            .push(vec![JsValue::from_str(&event), args]);
    }) as Box<dyn FnMut(String, JsValue)>);
    set(&root, "emit", &emit.into_js_value());

    let symbol = Function::new_with_args("description", "return Symbol(description)")
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("Context.filter"))
        .unwrap();
    let constructor = Object::new();
    set(&constructor, "filter", &symbol);
    let base = Object::new();
    set(&base, "constructor", &constructor);
    set(
        &base,
        "extend",
        &Function::new_with_args(
            "extension",
            "Object.setPrototypeOf(extension, this); return extension",
        ),
    );
    let base_for_plugin = base;
    let plugin = Closure::wrap(Box::new(move |_plugin: JsValue| {
        let fiber = Object::new();
        set(&fiber, "ctx", &base_for_plugin);
        set(
            &fiber,
            "dispose",
            &Function::new_no_args("return Promise.resolve()"),
        );
        fiber
    }) as Box<dyn FnMut(JsValue) -> Object>);
    set(&root, "plugin", &plugin.into_js_value());

    let contexts = Object::new();
    let register_client =
        Closure::wrap(Box::new(move |_name: String, _descriptor: JsValue| {})
            as Box<dyn FnMut(String, JsValue)>);
    set(
        &contexts,
        "registerClient",
        &register_client.into_js_value(),
    );
    let typert = Object::new();
    set(&typert, "contexts", &contexts);
    set(&services, "typert", &typert);

    let dispatches = Rc::new(RefCell::new(Vec::new()));
    let dispatches_for_remote = dispatches.clone();
    let dispatch = Closure::wrap(Box::new(move |event: JsValue, args: JsValue| {
        let mut row = vec![event];
        row.extend(Array::from(&args).iter());
        dispatches_for_remote.borrow_mut().push(row);
    }) as Box<dyn FnMut(JsValue, JsValue)>);
    let remote = Object::new();
    set(&remote, "$dispatch", &dispatch.into_js_value());
    let commands = Object::new();
    set(
        &commands,
        "execute",
        &Function::new_with_args("sessionId, line", "return Promise.resolve({ ok: true })"),
    );
    set(&remote, "commands", &commands);
    set(&services, "remote", &remote);

    let sinks = Rc::new(RefCell::new(JsValue::UNDEFINED));
    let sinks_for_start = sinks.clone();
    let stopped = Rc::new(RefCell::new(0));
    let stopped_for_loop = stopped.clone();
    let start = Closure::wrap(Box::new(move |installed: JsValue| {
        *sinks_for_start.borrow_mut() = installed;
        let loop_handle = Object::new();
        let stopped = stopped_for_loop.clone();
        let stop = Closure::wrap(Box::new(move || {
            *stopped.borrow_mut() += 1;
        }) as Box<dyn FnMut()>);
        set(&loop_handle, "stop", &stop.into_js_value());
        loop_handle
    }) as Box<dyn FnMut(JsValue) -> Object>);
    let connection = Object::new();
    set(&connection, "api", &api());
    set(&connection, "start", &start.into_js_value());
    set(&services, "connection", &connection);
    Bench {
        root: root.into(),
        services,
        emissions,
        dispatches,
        sinks,
        cleanups,
        stopped,
    }
}

async fn flush() {
    for _ in 0..4 {
        JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
            .await
            .unwrap();
    }
}

fn envelope(frame_type: &str) -> (Object, Object) {
    let payload = Object::new();
    set(&payload, "type", &JsValue::from_str(frame_type));
    let envelope = Object::new();
    set(&envelope, "rpcId", &JsValue::from_str("frame"));
    set(&envelope, "payload", &payload);
    (envelope, payload)
}

#[wasm_bindgen_test(async)]
async fn apply_mounts_services_and_fans_host_frames_into_both_rust_managers() {
    let bench = bench();
    let runtime = apply_client_runtime(bench.root.clone()).unwrap();
    for service in [
        "slots",
        "conversationEvents",
        "conversationViews",
        "sessions",
        "workspaces",
    ] {
        assert!(!get(&bench.services, service).is_undefined());
    }
    assert!(!bench.sinks.borrow().is_undefined());
    let sinks = bench.sinks.borrow().clone();
    let (session_envelope, session_payload) = envelope("host/session-added");
    set(&session_payload, "sessionId", &JsValue::from_str("s-new"));
    set(&session_payload, "blank", &JsValue::TRUE);
    get(&sinks, "onHostEnvelope")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&sinks, &session_envelope)
        .unwrap();
    let (workspace_envelope, workspace_payload) = envelope("host/workspace-changed");
    let workspace = Object::new();
    set(&workspace, "workspaceId", &JsValue::from_str("w-new"));
    set(&workspace, "path", &JsValue::from_str("/w/new"));
    set(&workspace, "title", &JsValue::from_str("new"));
    set(&workspace, "sessionIds", &Array::new());
    set(
        &workspace,
        "createdAt",
        &JsValue::from_str("2026-01-01T00:00:00.000Z"),
    );
    set(
        &workspace,
        "updatedAt",
        &JsValue::from_str("2026-01-01T00:00:00.000Z"),
    );
    set(&workspace_payload, "workspace", &workspace);
    get(&sinks, "onHostEnvelope")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&sinks, &workspace_envelope)
        .unwrap();
    flush().await;
    let sessions = get(&runtime, "sessions");
    let session_list = get(&sessions, "list");
    let session_snapshot = get(&session_list, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session_list)
        .unwrap();
    assert_eq!(
        get(&session_snapshot, "ids")
            .dyn_into::<Array>()
            .unwrap()
            .get(0)
            .as_string()
            .as_deref(),
        Some("s-new")
    );
    let workspaces = get(&runtime, "workspaces");
    let workspace_list = get(&workspaces, "list");
    let workspace_snapshot = get(&workspace_list, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&workspace_list)
        .unwrap();
    assert_eq!(
        get(
            &get(&workspace_snapshot, "items")
                .dyn_into::<Array>()
                .unwrap()
                .get(0),
            "workspaceId",
        )
        .as_string()
        .as_deref(),
        Some("w-new")
    );
}

#[wasm_bindgen_test]
fn wire_bridge_forwards_only_remote_events_and_emits_each_connection_reset() {
    let bench = bench();
    let runtime = apply_client_runtime(bench.root.clone()).unwrap();
    let sinks = get(&runtime, "sinks");
    let host = get(&sinks, "onHostEnvelope")
        .dyn_into::<Function>()
        .unwrap();
    let (remote_envelope, remote_payload) = envelope("host/remote-event");
    set(
        &remote_payload,
        "event",
        &JsValue::from_str("settings/document-updated"),
    );
    let args = Array::new();
    args.push(&JsValue::from_str("llm"));
    args.push(&JsValue::from_f64(7.0));
    set(&remote_payload, "args", &args);
    host.call1(&sinks, &remote_envelope).unwrap();
    let (ordinary, ordinary_payload) = envelope("host/session-status");
    set(&ordinary_payload, "sessionId", &JsValue::from_str("ghost"));
    set(&ordinary_payload, "running", &JsValue::FALSE);
    host.call1(&sinks, &ordinary).unwrap();
    assert_eq!(bench.dispatches.borrow().len(), 1);
    assert_eq!(
        bench.dispatches.borrow()[0][0].as_string().as_deref(),
        Some("settings/document-updated")
    );
    let connected = get(&sinks, "onConnected").dyn_into::<Function>().unwrap();
    connected.call1(&sinks, &Object::new()).unwrap();
    connected.call1(&sinks, &Object::new()).unwrap();
    assert_eq!(
        bench
            .emissions
            .borrow()
            .iter()
            .filter(|row| row[0].as_string().as_deref() == Some("connection/reset"))
            .count(),
        2
    );
}

#[wasm_bindgen_test]
fn registry_changes_reach_resident_session_rebuild_and_cleanup_stops_loop_once() {
    let bench = bench();
    let runtime = apply_client_runtime(bench.root.clone()).unwrap();
    let sessions = get(&runtime, "sessions");
    let rebuilds = Rc::new(RefCell::new(0));
    let observed = rebuilds.clone();
    let original = get(&sessions, "rebuildConversationRegistry")
        .dyn_into::<Function>()
        .unwrap();
    let sessions_for_call = sessions.clone();
    let wrapper = Closure::wrap(Box::new(move || {
        *observed.borrow_mut() += 1;
        original.call0(&sessions_for_call).unwrap();
    }) as Box<dyn FnMut()>);
    assert!(
        Reflect::set(
            &sessions,
            &JsValue::from_str("rebuildConversationRegistry"),
            &wrapper.into_js_value()
        )
        .unwrap()
    );
    let definition = Object::new();
    set(&definition, "kind", &JsValue::from_str("probe"));
    set(
        &definition,
        "match",
        &Function::new_with_args("event", "return null"),
    );
    set(
        &definition,
        "start",
        &Function::new_with_args("context, match, reader", "return null"),
    );
    set(
        &definition,
        "update",
        &Function::new_with_args("context, match", "return context.state"),
    );
    get(&runtime, "conversationEvents")
        .dyn_into::<Object>()
        .ok()
        .map(|events| {
            get(&events, "register")
                .dyn_into::<Function>()
                .unwrap()
                .call1(&events, &definition)
                .unwrap()
        });
    assert_eq!(*rebuilds.borrow(), 1);
    for cleanup in bench.cleanups.borrow().iter().rev() {
        cleanup.call0(&JsValue::UNDEFINED).unwrap();
    }
    assert_eq!(*bench.stopped.borrow(), 1);
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn registered_javascript_definition_executes_inside_rust_session_assembler() {
    let bench = bench();
    let runtime = apply_client_runtime(bench.root.clone()).unwrap();
    let view_definition = Object::new();
    set(&view_definition, "target", &JsValue::from_str("chat"));
    set(
        &view_definition,
        "create",
        &Function::new_no_args(
            "const read = (node, timeline) => ({ count: node?.data.count ?? 0, surfaceOp: node?.data.surfaceOp, location: timeline.turns.get(0)?.data.get('turn-probe'), anchorSeq: node?.anchorSeq, nodeLocation: node?.location?.kind, nodeTurn: node?.location?.turn?.turn, visibility: node?.visibility }); return { empty: { count: 0 }, replace({ nodes, timeline }) { return read(nodes[0], timeline) }, apply({ upserts, timeline }) { return read(upserts[0], timeline) } }",
        ),
    );
    let views = get(&runtime, "conversationViews");
    get(&views, "register")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&views, &view_definition)
        .unwrap();

    let trajectory_view_definition = Object::new();
    set(
        &trajectory_view_definition,
        "target",
        &JsValue::from_str("trajectory"),
    );
    set(
        &trajectory_view_definition,
        "create",
        &Function::new_no_args(
            "const read = node => ({ anchorSeq: node?.anchorSeq, location: node?.location?.kind, turn: node?.location?.turn?.turn, value: node?.data.value, ...(node?.visibility === undefined ? {} : { visibility: node.visibility }) }); return { empty: {}, replace({ nodes }) { return read(nodes[0]) }, apply({ upserts }) { return read(upserts[0]) } }",
        ),
    );
    get(&views, "register")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&views, &trajectory_view_definition)
        .unwrap();

    let definition = Object::new();
    set(&definition, "kind", &JsValue::from_str("probe"));
    set(&definition, "target", &JsValue::from_str("chat"));
    let match_calls = Rc::new(RefCell::new(0));
    let observed_matches = match_calls.clone();
    let match_event = Closure::wrap(Box::new(move |event: JsValue| {
        *observed_matches.borrow_mut() += 1;
        let event_type = get(&event, "type").as_string();
        if !matches!(event_type.as_deref(), Some("probe/start" | "probe/update")) {
            return JsValue::NULL;
        }
        let result = Object::new();
        set(&result, "id", &get(&get(&event, "data"), "id"));
        set(
            &result,
            "role",
            &JsValue::from_str(if event_type.as_deref() == Some("probe/start") {
                "start"
            } else {
                "update"
            }),
        );
        result.into()
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set(&definition, "match", &match_event.into_js_value());
    let start_calls = Rc::new(RefCell::new(0));
    let observed_starts = start_calls.clone();
    let start_definition = Closure::wrap(Box::new(
        move |_context: JsValue, accepted: JsValue, reader: JsValue| {
            *observed_starts.borrow_mut() += 1;
            let event = get(&accepted, "event");
            let previous = get(&reader, "previous")
                .dyn_into::<Function>()
                .unwrap()
                .call1(&reader, &JsValue::from_str("probe"))
                .unwrap();
            let base = if previous.is_undefined() {
                0.0
            } else {
                get(&get(&previous, "state"), "count").as_f64().unwrap()
            };
            let state = Object::new();
            set(
                &state,
                "count",
                &JsValue::from_f64(base + get(&get(&event, "data"), "count").as_f64().unwrap()),
            );
            set(&state, "surfaceOp", &get(&event, "surfaceOp"));
            state
        },
    )
        as Box<dyn FnMut(JsValue, JsValue, JsValue) -> Object>);
    set(&definition, "start", &start_definition.into_js_value());
    set(
        &definition,
        "update",
        &Function::new_with_args(
            "context, match",
            "return { count: context.state.count + match.event.data.delta, surfaceOp: context.state.surfaceOp }",
        ),
    );
    set(
        &definition,
        "publication",
        &Function::new_with_args("match", "return 'immediate'"),
    );
    set(
        &definition,
        "buildViewNode",
        &Function::new_with_args(
            "context",
            "return { key: context.key, kind: context.kind, id: context.id, target: 'chat', anchorSeq: context.start.event.seq + 0.25, location: context.start.location, visibility: 'hidden', data: context.state }",
        ),
    );
    let events = get(&runtime, "conversationEvents");
    get(&events, "register")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&events, &definition)
        .unwrap();
    let trajectory_definition = Object::new();
    set(
        &trajectory_definition,
        "kind",
        &JsValue::from_str("trajectory-probe"),
    );
    set(
        &trajectory_definition,
        "target",
        &JsValue::from_str("trajectory"),
    );
    set(
        &trajectory_definition,
        "match",
        &Function::new_with_args(
            "event",
            "return event.type === 'trajectory/start' ? { id: 'one', role: 'start' } : null",
        ),
    );
    set(
        &trajectory_definition,
        "start",
        &Function::new_with_args("context, match, reader", "return match.event.data"),
    );
    set(
        &trajectory_definition,
        "update",
        &Function::new_with_args("context, match", "return context.state"),
    );
    set(
        &trajectory_definition,
        "buildViewNode",
        &Function::new_with_args(
            "context",
            "return { key: context.key, kind: context.kind, id: context.id, target: 'trajectory', anchorSeq: context.start.event.seq + 0.5, location: context.start.location, data: context.state }",
        ),
    );
    get(&events, "register")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&events, &trajectory_definition)
        .unwrap();
    let location_definition = Object::new();
    set(
        &location_definition,
        "kind",
        &JsValue::from_str("turn-probe"),
    );
    set(
        &location_definition,
        "match",
        &Function::new_with_args(
            "event",
            "return event.type === 'turn/start' ? { id: 'turn', role: 'start' } : null",
        ),
    );
    set(
        &location_definition,
        "start",
        &Function::new_with_args("context, match, reader", "return { label: 'located' }"),
    );
    set(
        &location_definition,
        "update",
        &Function::new_with_args("context, match", "return context.state"),
    );
    set(
        &location_definition,
        "buildLocationData",
        &Function::new_with_args(
            "context, scope",
            "return scope === 'turn' ? { kind: 'turn', turn: 0, key: 'turn-probe', value: context.state } : null",
        ),
    );
    get(&events, "register")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&events, &location_definition)
        .unwrap();

    let sinks = get(&runtime, "sinks");
    let host = get(&sinks, "onHostEnvelope")
        .dyn_into::<Function>()
        .unwrap();
    let (added, payload) = envelope("host/session-added");
    set(&payload, "sessionId", &JsValue::from_str("s-probe"));
    set(&payload, "blank", &JsValue::TRUE);
    host.call1(&sinks, &added).unwrap();
    flush().await;
    let sessions = get(&runtime, "sessions");
    get(&sessions, "open")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&sessions, &JsValue::from_str("s-probe"))
        .unwrap();
    let binding = get(&sessions, "binding")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&sessions, &JsValue::from_str("s-probe"))
        .unwrap();
    assert!(!binding.is_undefined(), "Session binding was absent");
    let session = get(&binding, "session");
    assert!(!session.is_undefined(), "Session face was absent");
    let open = get(&session, "open")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session)
        .unwrap()
        .dyn_into::<Promise>()
        .unwrap();
    JsFuture::from(open).await.unwrap();
    let session_snapshot = get(&session, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session)
        .unwrap();
    let views = get(&session_snapshot, "views");
    assert!(!views.is_undefined(), "Session views face was absent");
    let initial_view = get(&views, "get")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&views, &JsValue::from_str("chat"))
        .unwrap();
    assert!(
        !initial_view.is_undefined(),
        "registered view target was absent"
    );
    assert_eq!(get(&initial_view, "count").as_f64(), Some(0.0));

    let mux = get(&sinks, "onMuxEnvelope").dyn_into::<Function>().unwrap();
    let send = |seq: f64, event_type: &str, data: JsValue| {
        let event = Object::new();
        set(&event, "seq", &JsValue::from_f64(seq));
        set(
            &event,
            "time",
            &JsValue::from_f64(1_800_000_000_000.0 + seq),
        );
        set(&event, "type", &JsValue::from_str(event_type));
        set(&event, "surfaceOp", &JsValue::from_str("append"));
        set(&event, "data", &data);
        let payload = Object::new();
        set(&payload, "type", &JsValue::from_str("session/event"));
        set(&payload, "sessionId", &JsValue::from_str("s-probe"));
        set(&payload, "event", &event);
        let envelope = Object::new();
        set(
            &envelope,
            "rpcId",
            &JsValue::from_str(&format!("event-{seq}")),
        );
        set(&envelope, "payload", &payload);
        mux.call1(&sinks, &envelope).unwrap();
    };
    let start = Object::new();
    set(&start, "id", &JsValue::from_str("one"));
    set(&start, "count", &JsValue::from_f64(2.0));
    let console = get(&js_sys::global(), "console");
    let original_console_error = get(&console, "error");
    let reported = Rc::new(RefCell::new(Vec::<String>::new()));
    let observed = reported.clone();
    let capture = Closure::wrap(Box::new(move |message: JsValue| {
        observed.borrow_mut().push(
            message
                .as_string()
                .unwrap_or_else(|| format!("{message:?}")),
        );
    }) as Box<dyn FnMut(JsValue)>);
    assert!(
        Reflect::set(
            &console,
            &JsValue::from_str("error"),
            &capture.into_js_value()
        )
        .unwrap()
    );
    let turn = Object::new();
    set(&turn, "turn", &JsValue::from_f64(0.0));
    send(0.0, "turn/start", turn.into());
    flush().await;
    get(&session, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session)
        .unwrap();
    send(1.0, "probe/start", start.into());
    flush().await;
    get(&session, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session)
        .unwrap();
    let snapshot = get(&views, "get")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&views, &JsValue::from_str("chat"))
        .unwrap();
    assert!(
        Reflect::set(
            &console,
            &JsValue::from_str("error"),
            &original_console_error
        )
        .unwrap()
    );
    assert!(
        !snapshot.is_undefined(),
        "Conversation view disappeared after start: {:?}",
        reported.borrow()
    );
    assert_eq!(*match_calls.borrow(), 2, "match callback count");
    assert_eq!(*start_calls.borrow(), 1, "start callback count");
    assert_eq!(
        get(&snapshot, "count").as_f64(),
        Some(2.0),
        "Conversation adapter reports: {:?}",
        reported.borrow()
    );
    assert_eq!(
        get(&snapshot, "surfaceOp").as_string().as_deref(),
        Some("append")
    );
    assert_eq!(
        get(&get(&snapshot, "location"), "label")
            .as_string()
            .as_deref(),
        Some("located")
    );
    assert_eq!(get(&snapshot, "anchorSeq").as_f64(), Some(1.25));
    assert_eq!(
        get(&snapshot, "nodeLocation").as_string().as_deref(),
        Some("turn")
    );
    assert_eq!(get(&snapshot, "nodeTurn").as_f64(), Some(0.0));
    assert_eq!(
        get(&snapshot, "visibility").as_string().as_deref(),
        Some("hidden")
    );

    let update = Object::new();
    set(&update, "id", &JsValue::from_str("one"));
    set(&update, "delta", &JsValue::from_f64(3.0));
    send(2.0, "probe/update", update.into());
    flush().await;
    get(&session, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session)
        .unwrap();
    let snapshot = get(&views, "get")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&views, &JsValue::from_str("chat"))
        .unwrap();
    assert_eq!(get(&snapshot, "count").as_f64(), Some(5.0));

    let second = Object::new();
    set(&second, "id", &JsValue::from_str("two"));
    set(&second, "count", &JsValue::from_f64(1.0));
    send(3.0, "probe/start", second.into());
    flush().await;
    get(&session, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session)
        .unwrap();
    let snapshot = get(&views, "get")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&views, &JsValue::from_str("chat"))
        .unwrap();
    assert_eq!(get(&snapshot, "count").as_f64(), Some(6.0));

    let trajectory = Object::new();
    set(&trajectory, "value", &JsValue::from_str("kept"));
    send(4.0, "trajectory/start", trajectory.into());
    flush().await;
    get(&session, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session)
        .unwrap();
    let snapshot = get(&views, "get")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&views, &JsValue::from_str("trajectory"))
        .unwrap();
    assert_eq!(get(&snapshot, "anchorSeq").as_f64(), Some(4.5));
    assert_eq!(
        get(&snapshot, "location").as_string().as_deref(),
        Some("turn")
    );
    assert_eq!(get(&snapshot, "turn").as_f64(), Some(0.0));
    assert_eq!(get(&snapshot, "value").as_string().as_deref(), Some("kept"));
    assert!(get(&snapshot, "visibility").is_undefined());
}
