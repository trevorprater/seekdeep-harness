//! Live React parity for Agent preset label, menu, row, and hero seat.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_agent_preset::{
    agent_preset_label_component, agent_preset_row_component, agent_preset_seat_component,
    agent_preset_section_component, configure_client_ui_agent_preset,
};
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten = values => values.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
let cached
export function makeAgentPresetComponentBench() {
  if (cached) { cached.reset(); return cached }
  const states=[], refs=[], effects=[], layouts=[], timers=[], styles=[], observers=[]
  let si=0,ri=0,ei=0,li=0,reduced=false,timerId=0,overflow=false
  const Fragment=Symbol('Fragment')
  const React={
    Fragment,
    createElement(kind,supplied,...children){const flat=flatten(children);const props={...(supplied??{})};if(flat.length===1)props.children=flat[0];else if(flat.length>1)props.children=flat;const node={kind,props,children:flat,focused:false,get scrollHeight(){return overflow?400:80},get clientHeight(){return 80},focus(){this.focused=true}};if(typeof props.ref==='function')props.ref(node);else if(props.ref&&typeof props.ref==='object')props.ref.current=node;return node},
    useState(initial){const at=si++;if(!(at in states))states[at]=typeof initial==='function'?initial():initial;return[states[at],value=>{states[at]=typeof value==='function'?value(states[at]):value}]},
    useRef(initial){const at=ri++;if(!(at in refs))refs[at]={current:initial};return refs[at]},
    useEffect(run,deps){const at=ei++;const old=effects[at];const same=old&&old.deps.length===deps.length&&deps.every((v,i)=>Object.is(v,old.deps[i]));if(!same){old?.cleanup?.();const cleanup=run();effects[at]={deps:[...deps],cleanup:typeof cleanup==='function'?cleanup:undefined}}},
    useLayoutEffect(run,deps){const at=li++;const old=layouts[at];const same=old&&old.deps.length===deps.length&&deps.every((v,i)=>Object.is(v,old.deps[i]));if(!same)layouts[at]={deps:[...deps],run,pending:true,cleanup:old?.cleanup}},
  }
  function resolve(value){if(Array.isArray(value))return flatten(value.map(resolve));if(value===null||value===undefined||value===false||typeof value!=='object')return value;if(!('kind'in value))return value;if(typeof value.kind==='function')return resolve(value.kind(value.props));if(value.kind===Fragment)return{kind:'Fragment',props:value.props,children:flatten(value.children.map(resolve))};return{...value,children:flatten(value.children.map(resolve))}}
  globalThis.window={matchMedia(){return{matches:reduced}},setTimeout(callback,delay){const row={id:++timerId,callback,delay,cleared:false};timers.push(row);return row.id},clearTimeout(id){const row=timers.find(row=>row.id===id);if(row)row.cleared=true}}
  globalThis.ResizeObserver=class{constructor(callback){this.callback=callback;this.connected=true;observers.push(this)}observe(){}disconnect(){this.connected=false}}
  globalThis.document={head:{appendChild(node){styles.push(node);return node}},createElement(kind){return{kind,attrs:{},setAttribute(k,v){this.attrs[k]=v},textContent:''}},querySelector(selector){const match=selector.match(/data-plugin-css="([^"]+)"/);return match?styles.find(row=>row.attrs['data-plugin-css']===match[1])??null:null}}
  const primitive=name=>props=>React.createElement(name,props,props.children)
  const copy={headerHint:'Session agent preset',title:'Agent preset',description:'Preset for new sessions.',loading:'Loading…',userTrust:'Local',builtIn:'Built-in',inUse:'In use',setDefault:'Use by default',builtInGroup:'Built-in presets',customGroup:'Your presets',sectionIntro:'Create presets by copying one or using Creator mode.',creatorDraft:'Create with Creator mode',duplicateUnavailable:'No writable preset root.',brokenBadge:'Broken',brokenNoCopy:'Fix before copying',view:'View',openLocation:'Open location',showLocation:'Show location',duplicate:'Duplicate',delete:'Delete',revealedPathLabel:'Preset directory',error:'Could not load presets.',retry:'Retry',copyTitle:'Copy preset',copyOf:'Copy of',copyIntro:'Choose an id and optional display name.',close:'Close',cancel:'Cancel',create:'Create',creating:'Creating…',presetId:'Preset id',presetIdPlaceholder:'my-preset',displayName:'Display name',displayNamePlaceholder:'My preset',idRequired:'Enter an id.',idInvalid:'Use lowercase letters, digits, and hyphens.',idTaken:'That id is already in use.',composition:'Composition',deleteTitle:'Delete preset',deleteDescription:'This removes the preset files.',deleteConfirm:'Delete',deleting:'Deleting…',seatHint:'Preset for the next session',noDescription:'No description',presetStandardName:'Standard',presetStandardDescription:'Full coding agent.',presetMinimalName:'Minimal',presetMinimalDescription:'Minimal agent.',presetCodeName:'Code',presetCodeDescription:'Code agent.',presetCordisName:'Cordis',presetCordisDescription:'Self-authoring agent.'}
  const primitives={Button:primitive('Button'),IconAgentPresetOutline16:primitive('IconAgentPresetOutline16'),IconBrowseOutline16:primitive('IconBrowseOutline16'),IconChevronDownOutline14:primitive('IconChevronDownOutline14'),IconCopyOutline16:primitive('IconCopyOutline16'),IconFolderOpenOutline16:primitive('IconFolderOpenOutline16'),IconPlusOutline16:primitive('IconPlusOutline16'),IconTrashOutline16:primitive('IconTrashOutline16'),Menu:primitive('Menu'),Modal:primitive('Modal'),Tooltip:primitive('Tooltip')}
  cached={React,primitives,styles,t:key=>copy[key]??key,render(component,props){si=0;ri=0;ei=0;li=0;const tree=resolve(React.createElement(component,props));for(const row of layouts){if(row?.pending){row.cleanup?.();const cleanup=row.run();row.cleanup=typeof cleanup==='function'?cleanup:undefined;row.pending=false}}return tree},reset(){for(const effect of effects.reverse())effect?.cleanup?.();for(const layout of layouts.reverse())layout?.cleanup?.();states.length=0;refs.length=0;effects.length=0;layouts.length=0;timers.length=0;observers.length=0;reduced=false;overflow=false},timers,setReduced(value){reduced=value},setOverflow(value){overflow=value;for(const observer of observers)if(observer.connected)observer.callback()},runTimers(){for(const row of timers.splice(0))if(!row.cleared)row.callback()}}
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
export function apSetOverflow(bench,value){bench.setOverflow(value)}
export function apFindText(root,text){return walk(root).find(node=>{const parts=[];const visit=value=>{if(typeof value==='string'||typeof value==='number')parts.push(String(value));else if(Array.isArray(value))value.forEach(visit);else if(value&&typeof value==='object')(value.children??[]).forEach(visit)};visit(node);return parts.join('')===text})}
export function apChange(node,value){return node.props.onChange?.({target:{value}})}
export function makeAgentPresetSectionProps(bench,state){const calls=[];const record=name=>(...args)=>{calls.push([name,...args]);return Promise.resolve()};return{calls,props:{t:bench.t,useAgentPresetSection:selector=>selector(state),load:record('load'),close:record('close'),startCreatorDraft:record('startCreatorDraft'),view:record('view'),closeView:record('closeView'),beginCopy:record('beginCopy'),cancelCopy:record('cancelCopy'),setCopyId:record('setCopyId'),setCopyName:record('setCopyName'),confirmCopy:record('confirmCopy'),openLocation:record('openLocation'),confirmDelete:record('confirmDelete'),remove:record('remove'),makeDefault:record('makeDefault')}}}
export function apCalls(value){return value.calls}
export function apFindKinds(root,kind){return walk(root).filter(node=>node.kind===kind)}
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
    fn apSetOverflow(bench: &JsValue, value: bool);
    fn apFindText(root: &JsValue, text: &str) -> JsValue;
    fn apChange(node: &JsValue, value: &str) -> JsValue;
    fn makeAgentPresetSectionProps(bench: &JsValue, state: &JsValue) -> JsValue;
    fn apCalls(value: &JsValue) -> Array;
    fn apFindKinds(root: &JsValue, kind: &str) -> Array;
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

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One composed fixture pins cards, dialogs, actions, and layout effects.
fn management_section_composes_cards_actions_dialogs_and_overflow_tooltips() {
    let bench = configure();
    let state = js_sys::JSON::parse(
        r#"{
          "status":"ready","error":null,"authorable":true,"hasDocument":true,
          "rows":[
            {"id":"standard","trust":"system","isDefault":true,"name":"File ignored","description":"File description ignored"},
            {"id":"cordis","trust":"system","isDefault":false},
            {"id":"mine","trust":"user","isDefault":false,"name":"Mine","description":"My local preset"}
          ],
          "copy":null,"view":null,"pendingDelete":null,"deleting":false,
          "revealedPaths":{"mine":"/presets/mine"}
        }"#,
    )
    .unwrap();
    let frame = makeAgentPresetSectionProps(&bench, &state);
    let props = property(&frame, "props");
    let component = agent_preset_section_component().unwrap();
    let tree = apRender(&bench, &component, &props);
    assert!(apText(&tree).contains("Built-in presets"));
    assert!(apText(&tree).contains("Your presets"));
    assert!(apText(&tree).contains("/presets/mine"));
    assert_eq!(
        apProp(
            &apFindProp(&tree, "aria-label", &JsValue::from_str("In use: Standard")),
            "disabled"
        ),
        JsValue::TRUE
    );
    for (label, method) in [
        ("Use by default: Mine", "makeDefault"),
        ("Open location: Mine", "openLocation"),
        ("Duplicate: Mine", "beginCopy"),
        ("Delete: Mine", "confirmDelete"),
        ("View: Standard", "view"),
    ] {
        apClick(&apFindProp(&tree, "aria-label", &JsValue::from_str(label)));
        assert!(
            apCalls(&frame)
                .iter()
                .any(|call| { Array::from(&call).get(0).as_string().as_deref() == Some(method) })
        );
    }
    apClick(&apFindText(&tree, "Create with Creator mode"));
    assert!(apCalls(&frame).iter().any(|call| {
        Array::from(&call).get(0).as_string().as_deref() == Some("startCreatorDraft")
    }));
    assert!(
        apCalls(&frame)
            .iter()
            .any(|call| { Array::from(&call).get(0).as_string().as_deref() == Some("close") })
    );

    Reflect::set(
        &state,
        &JsValue::from_str("copy"),
        &js_sys::JSON::parse(
            r#"{"from":"standard","fromTitle":"Standard","id":"Upper Case","name":"","saving":false,"error":null}"#,
        )
        .unwrap(),
    )
    .unwrap();
    let copy_tree = apRender(&bench, &component, &props);
    let copy_modal = apFindKinds(&copy_tree, "Modal").get(0);
    assert_eq!(apProp(&copy_modal, "open"), JsValue::TRUE);
    assert!(
        apProp(&copy_modal, "title")
            .as_string()
            .unwrap()
            .contains("Copy of Standard")
    );
    assert!(apText(&copy_tree).contains("Use lowercase letters, digits, and hyphens."));
    let id_input = apFindProp(&copy_tree, "placeholder", &JsValue::from_str("my-preset"));
    apChange(&id_input, "my-copy");
    assert!(apCalls(&frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("setCopyId")
            && call.get(1).as_string().as_deref() == Some("my-copy")
    }));

    Reflect::set(&state, &JsValue::from_str("copy"), &JsValue::NULL).unwrap();
    Reflect::set(
        &state,
        &JsValue::from_str("view"),
        &js_sys::JSON::parse(
            r#"{"id":"retired","title":"Retired mode","content":"- id: tool-bash\n"}"#,
        )
        .unwrap(),
    )
    .unwrap();
    Reflect::set(
        &state,
        &JsValue::from_str("pendingDelete"),
        &JsValue::from_str("mine"),
    )
    .unwrap();
    let modal_tree = apRender(&bench, &component, &props);
    let modals = apFindKinds(&modal_tree, "Modal");
    assert_eq!(modals.length(), 3);
    assert!(apText(&modal_tree).contains("- id: tool-bash\n"));
    assert_eq!(
        apProp(&modals.get(2), "title").as_string().as_deref(),
        Some("Delete preset")
    );

    apSetOverflow(&bench, true);
    let _ = apRender(&bench, &component, &props);
    let overflow_tree = apRender(&bench, &component, &props);
    assert_eq!(
        apProp(&apFindKind(&overflow_tree, "Tooltip"), "disabled"),
        JsValue::FALSE
    );

    Reflect::set(
        &state,
        &JsValue::from_str("status"),
        &JsValue::from_str("unavailable"),
    )
    .unwrap();
    assert!(apRender(&bench, &component, &props).is_null());
    Reflect::set(
        &state,
        &JsValue::from_str("status"),
        &JsValue::from_str("error"),
    )
    .unwrap();
    Reflect::set(
        &state,
        &JsValue::from_str("error"),
        &JsValue::from_str("roster unavailable"),
    )
    .unwrap();
    let error_tree = apRender(&bench, &component, &props);
    assert!(apText(&error_tree).contains("roster unavailable"));
    apClick(&apFindText(&error_tree, "Retry"));
    assert!(
        apCalls(&frame)
            .iter()
            .filter(|call| { Array::from(call).get(0).as_string().as_deref() == Some("load") })
            .count()
            >= 2
    );
}
