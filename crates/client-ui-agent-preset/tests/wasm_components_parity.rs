//! Live React parity for Agent preset label, menu, row, and hero seat.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_agent_preset::{
    agent_preset_label_component, agent_preset_row_component, agent_preset_seat_component,
    configure_client_ui_agent_preset,
};
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten = values => values.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
let cached
export function makeAgentPresetComponentBench() {
  if (cached) { cached.reset(); return cached }
  const states=[], effects=[], timers=[], styles=[]
  let si=0,ei=0,reduced=false,timerId=0
  const React={
    createElement(kind,supplied,...children){const flat=flatten(children);const props={...(supplied??{})};if(flat.length===1)props.children=flat[0];else if(flat.length>1)props.children=flat;return{kind,props,children:flat,focused:false,focus(){this.focused=true}}},
    useState(initial){const at=si++;if(!(at in states))states[at]=typeof initial==='function'?initial():initial;return[states[at],value=>{states[at]=typeof value==='function'?value(states[at]):value}]},
    useEffect(run,deps){const at=ei++;const old=effects[at];const same=old&&old.deps.length===deps.length&&deps.every((v,i)=>Object.is(v,old.deps[i]));if(!same){old?.cleanup?.();const cleanup=run();effects[at]={deps:[...deps],cleanup:typeof cleanup==='function'?cleanup:undefined}}},
  }
  function resolve(value){if(Array.isArray(value))return flatten(value.map(resolve));if(value===null||value===undefined||value===false||typeof value!=='object')return value;if(!('kind'in value))return value;if(typeof value.kind==='function')return resolve(value.kind(value.props));return{...value,children:flatten(value.children.map(resolve))}}
  globalThis.window={matchMedia(){return{matches:reduced}},setTimeout(callback,delay){const row={id:++timerId,callback,delay,cleared:false};timers.push(row);return row.id},clearTimeout(id){const row=timers.find(row=>row.id===id);if(row)row.cleared=true}}
  globalThis.document={head:{appendChild(node){styles.push(node);return node}},createElement(kind){return{kind,attrs:{},setAttribute(k,v){this.attrs[k]=v},textContent:''}},querySelector(selector){const match=selector.match(/data-plugin-css="([^"]+)"/);return match?styles.find(row=>row.attrs['data-plugin-css']===match[1])??null:null}}
  const primitive=name=>props=>React.createElement(name,props,props.children)
  const copy={headerHint:'Session agent preset',title:'Agent preset',description:'Preset for new sessions.',loading:'Loading…',userTrust:'Local',seatHint:'Preset for the next session',noDescription:'No description',presetStandardName:'Standard',presetStandardDescription:'Full coding agent.',presetMinimalName:'Minimal',presetMinimalDescription:'Minimal agent.',presetCodeName:'Code',presetCodeDescription:'Code agent.',presetCordisName:'Cordis',presetCordisDescription:'Self-authoring agent.'}
  cached={React,primitives:{IconAgentPresetOutline16:primitive('IconAgentPresetOutline16'),IconChevronDownOutline14:primitive('IconChevronDownOutline14'),Menu:primitive('Menu')},styles,t:key=>copy[key]??key,render(component,props){si=0;ei=0;return resolve(React.createElement(component,props))},reset(){for(const effect of effects.reverse())effect?.cleanup?.();states.length=0;effects.length=0;timers.length=0;reduced=false},timers,setReduced(value){reduced=value},runTimers(){for(const row of timers.splice(0))if(!row.cleared)row.callback()}}
  return cached
}
function walk(root,out=[]){if(!root||typeof root!=='object')return out;if(Array.isArray(root)){root.forEach(value=>walk(value,out));return out}if('kind'in root)out.push(root);(root.children??[]).forEach(value=>walk(value,out));return out}
export function apRender(bench,component,props){return bench.render(component,props)}
export function apFindKind(root,kind){return walk(root).find(node=>node.kind===kind)}
export function apFindProp(root,key,value){return walk(root).find(node=>value===undefined?key in node.props:Object.is(node.props[key],value))}
export function apText(root){const parts=[];const visit=value=>{if(typeof value==='string'||typeof value==='number')parts.push(String(value));else if(Array.isArray(value))value.forEach(visit);else if(value&&typeof value==='object')(value.children??[]).forEach(visit)};visit(root);return parts.join('')}
export function apClick(node){return node.props.onClick?.({preventDefault(){}})}
export function apSelect(menu,id){return menu.props.onSelect(id)}
export function apProp(node,key){return node?.props?.[key]}
export function apRunTimers(bench){bench.runTimers()}
export function apSetReduced(bench,value){bench.setReduced(value)}
"#)]
extern "C" {
    fn makeAgentPresetComponentBench() -> JsValue;
    fn apRender(bench: &JsValue, component: &JsValue, props: &JsValue) -> JsValue;
    fn apFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn apFindProp(root: &JsValue, key: &str, value: &JsValue) -> JsValue;
    fn apText(root: &JsValue) -> String;
    fn apClick(node: &JsValue) -> JsValue;
    fn apSelect(menu: &JsValue, id: &str) -> JsValue;
    fn apProp(node: &JsValue, key: &str) -> JsValue;
    fn apRunTimers(bench: &JsValue);
    fn apSetReduced(bench: &JsValue, value: bool);
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry).unwrap();
    }
    value.into()
}

fn options() -> JsValue {
    js_sys::JSON::parse(
        r#"[{"id":"standard","trust":"system"},{"id":"mine","trust":"user","name":"Mine","description":"My local preset"}]"#,
    )
    .unwrap()
}

fn configure() -> JsValue {
    let bench = makeAgentPresetComponentBench();
    configure_client_ui_agent_preset(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    bench
}

#[wasm_bindgen_test]
fn label_loads_only_for_named_sessions_and_resolves_roster_copy() {
    let bench = configure();
    let loads = Array::new();
    let load_calls = loads.clone();
    let load = Closure::wrap(Box::new(move || {
        load_calls.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let sessions = Closure::wrap(Box::new(move |selector: Function| {
        selector
            .call1(
                &JsValue::UNDEFINED,
                &js_sys::JSON::parse(r#"{"byId":{"s1":{"agentPreset":"standard"}}}"#).unwrap(),
            )
            .unwrap()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    let roster = options();
    let presets = Closure::wrap(Box::new(move |selector: Function| {
        selector
            .call1(&JsValue::UNDEFINED, &object(&[("options", roster.clone())]))
            .unwrap()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    let props = object(&[
        ("sessionId", JsValue::from_str("s1")),
        ("useSessions", sessions.into_js_value()),
        ("useAgentPresets", presets.into_js_value()),
        ("load", load.into_js_value()),
        ("t", property(&bench, "t")),
    ]);
    let tree = apRender(&bench, &agent_preset_label_component().unwrap(), &props);
    assert!(apText(&tree).contains("Standard"));
    assert_eq!(
        apProp(&tree, "title").as_string().as_deref(),
        Some("Full coding agent.")
    );
    assert_eq!(
        apProp(&apFindKind(&tree, "IconAgentPresetOutline16"), "size").as_f64(),
        Some(14.0)
    );
    assert_eq!(loads.length(), 1);

    let absent_sessions = Closure::wrap(Box::new(move |selector: Function| {
        selector
            .call1(
                &JsValue::UNDEFINED,
                &js_sys::JSON::parse(r#"{"byId":{"s2":{}}}"#).unwrap(),
            )
            .unwrap()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    let absent_options = options();
    let absent_presets = Closure::wrap(Box::new(move |selector: Function| {
        selector
            .call1(
                &JsValue::UNDEFINED,
                &object(&[("options", absent_options.clone())]),
            )
            .unwrap()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    let absent = object(&[
        ("sessionId", JsValue::from_str("s2")),
        ("useSessions", absent_sessions.into_js_value()),
        ("useAgentPresets", absent_presets.into_js_value()),
        ("load", Function::new_no_args("").into()),
        ("t", property(&bench, "t")),
    ]);
    assert!(apRender(&bench, &agent_preset_label_component().unwrap(), &absent).is_null());
}

#[wasm_bindgen_test]
fn settings_row_uses_shared_menu_copy_closes_then_selects_and_forces_read_only_closed() {
    let bench = configure();
    let state = object(&[
        ("status", JsValue::from_str("ready")),
        ("error", JsValue::NULL),
        ("writable", JsValue::TRUE),
        ("currentValue", JsValue::from_str("standard")),
        ("options", options()),
    ]);
    let state_cell = state.clone();
    let hook = Closure::wrap(Box::new(move |selector: Function| {
        selector.call1(&JsValue::UNDEFINED, &state_cell).unwrap()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    let selected = Array::new();
    let selected_log = selected.clone();
    let select = Closure::wrap(Box::new(move |id: String| {
        selected_log.push(&JsValue::from_str(&id));
    }) as Box<dyn FnMut(String)>);
    let props = object(&[
        ("load", Function::new_no_args("").into()),
        ("select", select.into_js_value()),
        ("useAgentPreset", hook.into_js_value()),
        ("t", property(&bench, "t")),
    ]);
    let first = apRender(&bench, &agent_preset_row_component().unwrap(), &props);
    let menu = apFindKind(&first, "Menu");
    assert_eq!(
        apProp(&menu, "selectedId").as_string().as_deref(),
        Some("standard")
    );
    assert_eq!(apProp(&menu, "align").as_string().as_deref(), Some("end"));
    assert_eq!(
        property(&Array::from(&apProp(&menu, "items")).get(1), "label")
            .as_string()
            .as_deref(),
        Some("Mine · Local")
    );
    apClick(&apProp(&menu, "anchor"));
    let open = apRender(&bench, &agent_preset_row_component().unwrap(), &props);
    let open_menu = apFindKind(&open, "Menu");
    assert_eq!(apProp(&open_menu, "open"), JsValue::TRUE);
    apSelect(&open_menu, "mine");
    assert_eq!(selected.get(0).as_string().as_deref(), Some("mine"));
    let closed = apRender(&bench, &agent_preset_row_component().unwrap(), &props);
    assert_eq!(apProp(&apFindKind(&closed, "Menu"), "open"), JsValue::FALSE);

    Reflect::set(&state, &JsValue::from_str("writable"), &JsValue::FALSE).unwrap();
    let _ = apRender(&bench, &agent_preset_row_component().unwrap(), &props);
    let read_only = apRender(&bench, &agent_preset_row_component().unwrap(), &props);
    let menu = apFindKind(&read_only, "Menu");
    assert_eq!(apProp(&menu, "open"), JsValue::FALSE);
    assert_eq!(apProp(&apProp(&menu, "anchor"), "disabled"), JsValue::TRUE);
}

#[wasm_bindgen_test]
fn seat_renders_described_items_and_acknowledges_motion_and_reduced_motion() {
    let bench = configure();
    let state = object(&[
        ("options", options()),
        ("current", JsValue::from_str("standard")),
        ("error", JsValue::NULL),
        ("busy", JsValue::FALSE),
        ("introduce", JsValue::TRUE),
    ]);
    let state_cell = state.clone();
    let hook = Closure::wrap(Box::new(move |selector: Function| {
        selector.call1(&JsValue::UNDEFINED, &state_cell).unwrap()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    let introductions = Array::new();
    let intro_log = introductions.clone();
    let introduced = Closure::wrap(Box::new(move || {
        intro_log.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let props = object(&[
        ("load", Function::new_no_args("").into()),
        ("select", Function::new_no_args("").into()),
        ("introduced", introduced.into_js_value()),
        ("useAgentPresetSeat", hook.into_js_value()),
        ("t", property(&bench, "t")),
    ]);
    let _ = apRender(&bench, &agent_preset_seat_component().unwrap(), &props);
    let animated = apRender(&bench, &agent_preset_seat_component().unwrap(), &props);
    let menu = apFindKind(&animated, "Menu");
    assert_eq!(apProp(&menu, "align").as_string().as_deref(), Some("start"));
    assert!(
        apText(&property(
            &Array::from(&apProp(&menu, "items")).get(1),
            "label"
        ))
        .contains("My local preset")
    );
    let first_char = apFindProp(
        &apProp(&menu, "anchor"),
        "className",
        &JsValue::from_str("seekdeep-agent-preset-introChar"),
    );
    assert_eq!(
        property(&apProp(&first_char, "style"), "animationDelay")
            .as_string()
            .as_deref(),
        Some("150ms")
    );
    apRunTimers(&bench);
    assert_eq!(introductions.length(), 1);

    apSetReduced(&bench, true);
    Reflect::set(
        &state,
        &JsValue::from_str("current"),
        &JsValue::from_str("mine"),
    )
    .unwrap();
    let _ = apRender(&bench, &agent_preset_seat_component().unwrap(), &props);
    assert_eq!(introductions.length(), 2);
}
