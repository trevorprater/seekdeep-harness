//! Live Cordis, Slot, Session, locale, and RPC assembly parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Promise, Reflect};
use seekdeep_client_ui_agent_preset::{
    agent_preset_inject, apply_client_ui_agent_preset, configure_client_ui_agent_preset,
};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const ok=value=>({result:{ok:true,value}})
export function makeAgentPresetApplyBench(){
  const effects=[],scopeEffects=[],localeCalls=[],apiCalls=[],notes=[],starts=[]
  const handlers=new Map(),contextHandlers=new Map()
  const add=(map,name,fn)=>{const set=map.get(name)??new Set();set.add(fn);map.set(name,set);return()=>set.delete(fn)}
  const emit=(map,name,...args)=>{for(const fn of [...(map.get(name)??[])])fn(...args)}
  const roster=[{id:'standard',trust:'system',isDefault:true},{id:'cordis',trust:'system',isDefault:false},{id:'mine',trust:'user',isDefault:false}]
  const api={agentPresets:{list(request){apiCalls.push(['list',structuredClone(request)]);return Promise.resolve(ok({presets:structuredClone(roster),authorable:true,hasDocument:false}))},select(request){apiCalls.push(['select',structuredClone(request)]);return Promise.resolve(ok({agentPreset:request.agentPreset}))},read(request){return Promise.resolve(ok({name:request.agentPreset,content:'[]\n'}))},copy(request){return Promise.resolve(ok({agentPreset:request.agentPreset}))},openDocument(request){return Promise.resolve(ok({opened:false,path:`/presets/${request.agentPreset}`}))},remove(){return Promise.resolve(ok({}))}},settings:{describe(){return Promise.resolve(ok({writable:true}))},update(request){apiCalls.push(['update',structuredClone(request)]);for(const row of roster)row.isDefault=row.id===request.patch.default;return Promise.resolve(ok({}))}}}
  function ledger(initial=[]){const declared=new Set(initial),rows=new Map(),injectors=new Map();const reconcile=name=>{for(const item of injectors.get(name)??[]){if(declared.has(name)&&!item.cleanup)item.cleanup=item.setup();else if(!declared.has(name)&&item.cleanup){item.cleanup();item.cleanup=undefined}}};return{declare(name){declared.add(name);reconcile(name)},register(options,component){if(!declared.has(options.name))throw new Error(`undeclared ${options.name}`);const row={options,component};const list=rows.get(options.name)??[];list.push(row);rows.set(options.name,list);return()=>{const at=list.indexOf(row);if(at>=0)list.splice(at,1)}},inject(name,setup){const item={setup,cleanup:undefined};const list=injectors.get(name)??[];list.push(item);injectors.set(name,list);reconcile(name);const stop=()=>{item.cleanup?.();item.cleanup=undefined;const at=list.indexOf(item);if(at>=0)list.splice(at,1)};effects.push(stop);return stop},entries(name){return[...(rows.get(name)??[])]}}}
  const rootSlots=ledger([]),scopeSlots=ledger(['conversation.hero.agentPreset','conversation.session.header.actions'])
  const locale={register(ns,dictionaries){localeCalls.push([ns,dictionaries]);return()=>localeCalls.push(['disposed',ns])},bind(){return key=>key}}
  const remote={$on(name,fn){return add(handlers,name,fn)}}
  let listState={current:undefined,byId:{}}
  const listListeners=new Set()
  const sessions={list:{getSnapshot(){return listState},subscribe(fn){listListeners.add(fn);return()=>listListeners.delete(fn)}},noteAgentPreset(id,preset){notes.push([id,preset]);if(listState.byId[id])listState.byId[id].agentPreset=preset}}
  const workspaces={startSession(){starts.push(true);listState={current:'s1',byId:{s1:{id:'s1',blank:true,agentPreset:'standard'}}};for(const fn of [...listListeners])fn()}}
  const connection={api}
  const scope={slots:scopeSlots,sessions,workspaces,remote,get(name){return name==='connection'?connection:undefined},effect(setup){const dispose=setup();if(typeof dispose==='function')scopeEffects.push(dispose);return dispose}}
  const ctx={slots:rootSlots,locale,remote,get(name){return name==='connection'?connection:undefined},effect(setup){const dispose=setup();if(typeof dispose==='function')effects.push(dispose);return dispose},on(name,fn){return add(contextHandlers,name,fn)},inject(deps,callback){callback(scope)}}
  const React={Fragment:Symbol('Fragment'),createElement(){},useState(){return[false,()=>{}]},useEffect(){},useLayoutEffect(){},useRef(value){return{current:value}}}
  const primitive=()=>()=>null
  const primitives={Button:primitive(),IconAgentPresetOutline16:primitive(),IconBrowseOutline16:primitive(),IconChevronDownOutline14:primitive(),IconCopyOutline16:primitive(),IconFolderOpenOutline16:primitive(),IconPlusOutline16:primitive(),IconTrashOutline16:primitive(),Menu:primitive(),Modal:primitive(),Tooltip:primitive()}
  globalThis.document={head:{appendChild(node){return node}},createElement(){return{setAttribute(){},textContent:''}},querySelector(){return null}}
  return{ctx,scope,rootSlots,scopeSlots,apiCalls,localeCalls,handlers,contextHandlers,notes,starts,React,primitives,declareSettings(){rootSlots.declare('settings.general.item');rootSlots.declare('settings.section')},emitRemote(name,...args){emit(handlers,name,...args)},dispose(){for(const fn of [...scopeEffects].reverse())fn();for(const fn of [...effects].reverse())fn()}}
}
export function apEntries(bench,scope,name){return(scope?bench.scopeSlots:bench.rootSlots).entries(name)}
export function apDeclareSettings(bench){bench.declareSettings()}
export function apInject(row){return row.options.inject()}
export function apCall(face,name,...args){return face[name](...args)}
export function apTick(){return new Promise(resolve=>setTimeout(resolve,0))}
export function apEmitRemote(bench,name,...args){bench.emitRemote(name,...args)}
export function apDispose(bench){bench.dispose()}
"#)]
extern "C" {
    fn makeAgentPresetApplyBench() -> JsValue;
    fn apEntries(bench: &JsValue, scoped: bool, name: &str) -> Array;
    fn apDeclareSettings(bench: &JsValue);
    fn apInject(row: &JsValue) -> JsValue;
    fn apCall(face: &JsValue, name: &str, argument: &JsValue) -> JsValue;
    fn apTick() -> Promise;
    fn apEmitRemote(bench: &JsValue, name: &str, first: &str, second: &str);
    fn apDispose(bench: &JsValue);
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

async fn settle() {
    for _ in 0..6 {
        JsFuture::from(apTick()).await.unwrap();
    }
}

#[wasm_bindgen_test(async)]
async fn apply_registers_late_root_and_scoped_surfaces_creator_handoff_events_and_teardown() {
    let bench = makeAgentPresetApplyBench();
    configure_client_ui_agent_preset(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    apply_client_ui_agent_preset(property(&bench, "ctx")).unwrap();
    assert_eq!(
        agent_preset_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["slots", "locale", "connection", "remote"]
    );
    assert_eq!(
        apEntries(&bench, false, "settings.general.item").length(),
        0
    );
    assert_eq!(apEntries(&bench, false, "settings.section").length(), 0);
    assert_eq!(
        apEntries(&bench, true, "conversation.hero.agentPreset").length(),
        1
    );
    assert_eq!(
        apEntries(&bench, true, "conversation.session.header.actions").length(),
        1
    );

    apDeclareSettings(&bench);
    let rows = apEntries(&bench, false, "settings.general.item");
    let sections = apEntries(&bench, false, "settings.section");
    assert_eq!(rows.length(), 1);
    assert_eq!(sections.length(), 1);
    assert_eq!(
        property(&property(&rows.get(0), "options"), "order").as_f64(),
        Some(-25.0)
    );
    assert_eq!(
        property(&property(&sections.get(0), "options"), "order").as_f64(),
        Some(20.0)
    );
    let section_face = apInject(&sections.get(0));
    assert!(property(&section_face, "startCreatorDraft").is_function());
    apCall(&section_face, "startCreatorDraft", &JsValue::UNDEFINED);
    settle().await;
    assert_eq!(Array::from(&property(&bench, "starts")).length(), 1);
    assert!(
        Array::from(&property(&bench, "apiCalls"))
            .iter()
            .any(|call| {
                let call = Array::from(&call);
                call.get(0).as_string().as_deref() == Some("select")
                    && property(&call.get(1), "agentPreset").as_string().as_deref()
                        == Some("cordis")
            })
    );
    assert!(Array::from(&property(&bench, "notes")).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("s1")
            && call.get(1).as_string().as_deref() == Some("cordis")
    }));

    apEmitRemote(&bench, "agent-preset/selected", "s1", "minimal");
    assert!(Array::from(&property(&bench, "notes")).iter().any(|call| {
        let call = Array::from(&call);
        call.get(1).as_string().as_deref() == Some("minimal")
    }));
    let before = Array::from(&property(&bench, "apiCalls")).length();
    apEmitRemote(&bench, "settings/document-updated", "agent-presets", "");
    settle().await;
    assert!(Array::from(&property(&bench, "apiCalls")).length() > before);

    apDispose(&bench);
    assert_eq!(
        apEntries(&bench, false, "settings.general.item").length(),
        0
    );
    assert_eq!(apEntries(&bench, false, "settings.section").length(), 0);
    assert_eq!(
        apEntries(&bench, true, "conversation.hero.agentPreset").length(),
        0
    );
    assert_eq!(
        apEntries(&bench, true, "conversation.session.header.actions").length(),
        0
    );
}
