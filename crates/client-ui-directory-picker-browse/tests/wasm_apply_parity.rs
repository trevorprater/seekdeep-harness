//! Live locale and transactional dual-Slot assembly parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_directory_picker_browse::{
    DIRECTORY_BROWSER_LOCALES, apply_client_ui_directory_picker_browse,
    browse_directory_flow_component, configure_client_ui_directory_picker_browse,
    directory_picker_browse_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let cached
export function makeBrowseApplyBench(){if(cached){cached.reset();return cached}const styles=[];globalThis.document={head:{appendChild(node){styles.push(node);return node}},createElement(){return{attrs:{},setAttribute(key,value){this.attrs[key]=value},textContent:''}},querySelector(){return null}};const React={Fragment:Symbol('Fragment'),createElement(){},useEffect(){},useRef(initial){return{current:initial}},useState(initial){return[initial,()=>{}]}};const primitives={};for(const name of ['Button','IconCheckOutline16','IconChevronRightOutline14','IconEditOutline16','IconFolderClose16','IconFolderOpen16','IconPlusOutline16','Modal'])primitives[name]=name;cached={React,primitives,styles,reset(){styles.length=0}};return cached}
function makeSlots(){const declared=new Set(),rows=new Map(),waiters=new Map();const reconcile=name=>{for(const waiter of [...(waiters.get(name)??[])]){if(declared.has(name)&&!waiter.cleanup){try{waiter.cleanup=waiter.setup()}catch(error){const list=waiters.get(name)??[],index=list.indexOf(waiter);if(index>=0)list.splice(index,1);throw error}}else if(!declared.has(name)&&waiter.cleanup){waiter.cleanup();waiter.cleanup=undefined}}};return{declare(name){declared.add(name);reconcile(name)},undeclare(name){declared.delete(name);reconcile(name)},inject(name,setup){const waiter={setup,cleanup:undefined},list=waiters.get(name)??[];list.push(waiter);waiters.set(name,list);reconcile(name);return()=>{waiter.cleanup?.();waiter.cleanup=undefined;const index=list.indexOf(waiter);if(index>=0)list.splice(index,1)}},register(options,component){if(!declared.has(options.name))throw new Error(`undeclared ${options.name}`);const list=rows.get(options.name)??[];if(list.length>0)throw new Error(`occupied ${options.name}`);const row={options,component};list.push(row);rows.set(options.name,list);return()=>{const index=list.indexOf(row);if(index>=0)list.splice(index,1)}},entries(name){return rows.get(name)??[]},dispose(){for(const list of waiters.values())for(const waiter of [...list])waiter.cleanup?.();waiters.clear()}}}
export function makeBrowseApplyFrame(){const slots=makeSlots(),calls=[],effects=[],dictionaries=new Map();let localeConflict='';const locale={register(ns,language,dict){const key=`${ns}:${language}`;if(localeConflict===language||dictionaries.has(key))throw new Error(`locale occupied ${key}`);dictionaries.set(key,dict);calls.push(['locale',language]);return()=>{dictionaries.delete(key);calls.push(['locale-dispose',language])}},bind(ns){return(key,vars={})=>Object.entries(vars).reduce((text,[name,value])=>text.replaceAll(`{${name}}`,String(value)),dictionaries.get(`${ns}:en`)?.[key]??key)}};const workspaces={listDirectory(path,signal){calls.push(['list',path,signal]);return Promise.resolve({path:'/home/u',home:'/home/u',crumbs:[],entries:[],truncated:false})},createDirectory(path,name){calls.push(['create',path,name]);return Promise.resolve(`${path}/${name}`)}};const ctx={slots,workspaces,locale,effect(setup,label){calls.push(['effect',label]);const cleanup=setup();if(typeof cleanup==='function')effects.push(cleanup)}};return{ctx,slots,calls,dictionaries,effects,declareConversation(){slots.declare('conversation.hero.workspace.directoryFlow')},declareSidebar(){slots.declare('sidebar.workspaces.directoryFlow')},setLocaleConflict(value){localeConflict=value},dispose(){slots.dispose();for(const cleanup of effects.reverse())cleanup()}}}
export function apDeclareConversation(frame){frame.declareConversation()}
export function apDeclareSidebar(frame){frame.declareSidebar()}
export function apEntries(frame,name){return frame.slots.entries(name)}
export function apCalls(frame){return frame.calls}
export function apSetLocaleConflict(frame,value){frame.setLocaleConflict(value)}
export function apDispose(frame){frame.dispose()}
export function apRegisterRival(frame,name){return frame.slots.register({name},()=>null)}
export function apDictionary(frame,language,key){return frame.dictionaries.get(`directory-browser:${language}`)?.[key]}
"#)]
extern "C" {
    fn makeBrowseApplyBench() -> JsValue;
    fn makeBrowseApplyFrame() -> JsValue;
    fn apDeclareConversation(frame: &JsValue);
    fn apDeclareSidebar(frame: &JsValue);
    fn apEntries(frame: &JsValue, name: &str) -> Array;
    fn apCalls(frame: &JsValue) -> Array;
    fn apSetLocaleConflict(frame: &JsValue, value: &str);
    fn apDispose(frame: &JsValue);
    fn apRegisterRival(frame: &JsValue, name: &str) -> Function;
    fn apDictionary(frame: &JsValue, language: &str, key: &str) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn call(value: &JsValue, name: &str, arguments: &[JsValue]) -> JsValue {
    let function = property(value, name).dyn_into::<Function>().unwrap();
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function.apply(value, &args).unwrap()
}

fn configure() -> JsValue {
    let bench = makeBrowseApplyBench();
    configure_client_ui_directory_picker_browse(
        property(&bench, "React"),
        property(&bench, "primitives"),
    )
    .unwrap();
    bench
}

#[wasm_bindgen_test]
fn apply_waits_for_both_declarations_routes_calls_and_tears_down_as_one_pair() {
    let _bench = configure();
    let frame = makeBrowseApplyFrame();
    assert_eq!(
        directory_picker_browse_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        vec!["slots", "workspaces", "locale"]
    );
    apply_client_ui_directory_picker_browse(property(&frame, "ctx")).unwrap();
    apDeclareSidebar(&frame);
    assert_eq!(
        apEntries(&frame, "sidebar.workspaces.directoryFlow").length(),
        0
    );
    apDeclareConversation(&frame);
    let conversation = apEntries(&frame, "conversation.hero.workspace.directoryFlow");
    let sidebar = apEntries(&frame, "sidebar.workspaces.directoryFlow");
    assert_eq!(conversation.length(), 1);
    assert_eq!(sidebar.length(), 1);
    assert!(Object::is(
        &property(&conversation.get(0), "component"),
        &browse_directory_flow_component().unwrap()
    ));
    assert!(Object::is(
        &property(&sidebar.get(0), "component"),
        &browse_directory_flow_component().unwrap()
    ));
    for (key, zh, en) in DIRECTORY_BROWSER_LOCALES {
        assert_eq!(
            apDictionary(&frame, "zh", key).as_string().as_deref(),
            Some(zh)
        );
        assert_eq!(
            apDictionary(&frame, "en", key).as_string().as_deref(),
            Some(en)
        );
    }
    let face = property(&conversation.get(0), "options");
    let face = property(&face, "inject")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    call(
        &face,
        "listDirectory",
        &[JsValue::UNDEFINED, Object::new().into()],
    );
    call(
        &face,
        "createDirectory",
        &[JsValue::from_str("/home/u"), JsValue::from_str("fresh")],
    );
    assert!(
        apCalls(&frame)
            .iter()
            .any(|call| { Array::from(&call).get(0).as_string().as_deref() == Some("list") })
    );
    assert!(
        apCalls(&frame)
            .iter()
            .any(|call| { Array::from(&call).get(0).as_string().as_deref() == Some("create") })
    );
    apDispose(&frame);
    assert_eq!(
        apEntries(&frame, "conversation.hero.workspace.directoryFlow").length(),
        0
    );
    assert_eq!(
        apEntries(&frame, "sidebar.workspaces.directoryFlow").length(),
        0
    );
}

#[wasm_bindgen_test]
fn slot_and_locale_conflicts_roll_back_every_partial_acquisition() {
    let _bench = configure();
    let slot_frame = makeBrowseApplyFrame();
    apDeclareConversation(&slot_frame);
    apDeclareSidebar(&slot_frame);
    let rival = apRegisterRival(&slot_frame, "sidebar.workspaces.directoryFlow");
    assert!(apply_client_ui_directory_picker_browse(property(&slot_frame, "ctx")).is_err());
    assert_eq!(
        apEntries(&slot_frame, "conversation.hero.workspace.directoryFlow").length(),
        0
    );
    assert_eq!(
        apEntries(&slot_frame, "sidebar.workspaces.directoryFlow").length(),
        1
    );
    rival.call0(&JsValue::UNDEFINED).unwrap();
    apDispose(&slot_frame);

    let locale_frame = makeBrowseApplyFrame();
    apSetLocaleConflict(&locale_frame, "en");
    assert!(apply_client_ui_directory_picker_browse(property(&locale_frame, "ctx")).is_err());
    assert!(apDictionary(&locale_frame, "zh", "browser.title").is_undefined());
    assert!(apCalls(&locale_frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("locale-dispose")
            && call.get(1).as_string().as_deref() == Some("zh")
    }));
    apDispose(&locale_frame);
}
