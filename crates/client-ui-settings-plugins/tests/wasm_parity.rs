//! Live controller, component, tab, and plugin-assembly parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_settings_plugins::{
    apply_client_ui_settings_plugins, configurable_plugins_tab_component,
    configure_client_ui_settings_plugins, create_plugins_bash_controller,
    create_plugins_web_search_controller, plugins_bash_card_component,
    plugins_secret_field_component, plugins_settings_section_component,
    plugins_value_field_component, settings_plugins_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten = values => values.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
function engine() {
  const states=[], refs=[], effects=[], memos=[]
  let si=0,ri=0,ei=0,mi=0,id=0
  const Fragment=Symbol('Fragment')
  const React={
    Fragment,
    createElement(kind,supplied,...children){const flat=flatten(children);const props={...(supplied??{})};if(flat.length===1)props.children=flat[0];else if(flat.length>1)props.children=flat;const node={kind,props,children:flat,focused:false,focus(){this.focused=true}};if(typeof props.ref==='function')props.ref(node);else if(props.ref&&typeof props.ref==='object')props.ref.current=node;return node},
    useState(initial){const at=si++;if(!(at in states))states[at]=typeof initial==='function'?initial():initial;return[states[at],value=>{states[at]=typeof value==='function'?value(states[at]):value}]},
    useRef(initial){const at=ri++;if(!(at in refs))refs[at]={current:initial};return refs[at]},
    useEffect(run,deps){const at=ei++;const old=effects[at];const same=old&&old.deps.length===deps.length&&deps.every((v,i)=>Object.is(v,old.deps[i]));if(!same){old?.cleanup?.();const cleanup=run();effects[at]={deps:[...deps],cleanup:typeof cleanup==='function'?cleanup:undefined}}},
    useId(){const at=mi++;if(!(at in memos))memos[at]=`plugins-${++id}`;return memos[at]},
  }
  function resolve(value){if(Array.isArray(value))return flatten(value.map(resolve));if(value===null||value===undefined||value===false||typeof value!=='object')return value;if(!('kind'in value))return value;if(typeof value.kind==='function')return resolve(value.kind(value.props));if(value.kind===Fragment)return{kind:'Fragment',props:value.props,children:flatten(value.children.map(resolve))};return{...value,children:flatten(value.children.map(resolve))}}
  return{React,render(component,props){si=0;ri=0;ei=0;mi=0;return resolve(React.createElement(component,props))},reset(){for(const effect of effects.reverse())effect?.cleanup?.();states.length=0;refs.length=0;effects.length=0;memos.length=0}}
}

let cachedBench
export function makePluginsBench(){
  if(cachedBench){cachedBench.reset();return cachedBench}
  const e=engine(),styles=[]
  globalThis.document={head:{appendChild(node){styles.push(node);return node}},createElement(kind){return{kind,attrs:{},setAttribute(k,v){this.attrs[k]=v},textContent:''}},querySelector(selector){const m=selector.match(/data-plugin-css="([^"]+)"/);return m?styles.find(row=>row.attrs['data-plugin-css']===m[1])??null:null}}
  const primitive=name=>props=>e.React.createElement(name,props,props.children)
  const copy={nav:'Plugins',title:'Plugins',intro:'Configure and inspect the plugins installed in this deployment.',tabs:'Plugin views',configurableTab:'Plugin configuration',empty:'This deployment exposes no plugin settings.',overridden:'Overridden',reset:'Reset to default',readOnly:'This deployment stores settings read-only.',expand:'Show settings',collapse:'Hide settings',save:'Save',saving:'Saving…',discard:'Discard',unsaved:'Unsaved',saveFailed:'Save failed.',invalidNumber:'Enter a number.',bashTitle:'Shell',bashDescription:'Limits every command the agent runs.',bashTimeoutMs:'Command timeout (ms)',bashTimeoutMsHint:'How long one command may run.',bashMaxOutputBytes:'Output cap per stream (bytes)',bashMaxOutputBytesHint:'Output cap.',agentLoopTitle:'Agent loop',agentLoopDescription:'How the agent dispatches tool calls.',agentLoopMaxParallel:'Parallel tool calls',agentLoopMaxParallelHint:'Parallel hint.',webSearchTitle:'Web search',webSearchDescription:'The DeepSeek search provider.',webSearchApiKey:'API key',webSearchApiKeyHint:'Stored outside settings.',webSearchApiKeySet:'A key is configured.',webSearchApiKeyUnset:'No key is configured.',webSearchBaseUrl:'Endpoint',webSearchBaseUrlHint:'Provider default.',webSearchMaxUses:'Max searches per request',webSearchMaxUsesHint:'Search budget.'}
  cachedBench={...e,primitives:{IconChevronDownOutline14:primitive('IconChevronDownOutline14')},styles,t:key=>copy[key]??key,clsx:(...values)=>values.filter(Boolean).join(' '),resolveSlotLabel:value=>typeof value==='function'?value():value}
  return cachedBench
}

export function makeScope(initial={}){const listeners=new Set(),calls=[];let accept=true;let state={status:'ready',value:{},base:{},user:{},revision:1,writable:true,mode:'host',...structuredClone(initial)};const publish=patch=>{state={...state,...structuredClone(patch)};for(const listener of [...listeners])listener()};return{calls,getSnapshot(){return state},subscribe(listener){listeners.add(listener);return()=>listeners.delete(listener)},set(field,value){calls.push(['set',field,structuredClone(value)]);if(accept)publish({value:{...(state.value??{}),[field]:value},user:{...(state.user??{}),[field]:value},revision:state.revision+1});return Promise.resolve()},unset(field){calls.push(['unset',field]);if(accept){const user={...(state.user??{})};delete user[field];const value={...(state.value??{})};if(state.base&&field in state.base)value[field]=state.base[field];else delete value[field];publish({value,user,revision:state.revision+1})}return Promise.resolve()},publish,setAccept(value){accept=value}}}
const ok=value=>({result:{ok:true,value}}), fail=message=>({result:{ok:false,error:{message}}})
export function makePluginsApi(configured=false){const calls=[],views={DEEPSEEK_API_KEY:{configured,writable:true}},api={credentials:{describe(request){calls.push(['describe',structuredClone(request)]);return Promise.resolve(ok({credentials:Object.fromEntries(request.refs.map(ref=>[ref,views[ref]]).filter(([,view])=>view!==undefined))}))},set(request){calls.push(['set',structuredClone(request)]);views[request.ref]={configured:true,writable:true};return Promise.resolve(ok({stored:true}))}}};return{api,calls,views,setView(ref,view){views[ref]=view},refuseSet(){api.credentials.set=request=>{calls.push(['set',structuredClone(request)]);return Promise.resolve(fail('refused'))}}}}

function walk(root,out=[]){if(!root||typeof root!=='object')return out;if(Array.isArray(root)){root.forEach(v=>walk(v,out));return out}if('kind'in root)out.push(root);(root.children??[]).forEach(v=>walk(v,out));return out}
export function pRender(bench,component,props){try{return bench.render(component,props)}catch(error){throw error instanceof Error?error:new Error(`render threw ${String(error)}`)}}
export function pFind(root,key,value){return walk(root).find(node=>value===undefined?key in node.props:Object.is(node.props[key],value))}
export function pFindText(root,text){return walk(root).find(node=>{const parts=[];const visit=v=>{if(typeof v==='string'||typeof v==='number')parts.push(String(v));else if(Array.isArray(v))v.forEach(visit);else if(v&&typeof v==='object')(v.children??[]).forEach(visit)};visit(node);return parts.join('')===text})}
export function pText(root){const parts=[];const visit=v=>{if(typeof v==='string'||typeof v==='number')parts.push(String(v));else if(Array.isArray(v))v.forEach(visit);else if(v&&typeof v==='object')(v.children??[]).forEach(visit)};visit(root);return parts.join('')}
export function pClick(node){return node.props.onClick?.({target:node,preventDefault(){},stopPropagation(){}})}
export function pChange(node,value){return node.props.onChange?.({target:{value}})}
export function pKey(node,key){return node.props.onKeyDown?.({key,preventDefault(){}})}
export function pProp(node,key){return node?.props?.[key]}
export function pCalls(value){return value.calls}
export function pTick(){return new Promise(resolve=>setTimeout(resolve,0))}
export function pScopePublish(scope,patch){scope.publish(patch)}
export function pScopeAccept(scope,value){scope.setAccept(value)}
export function pMakeCardProps(bench,face,hookName){const store=Object.values(face.hooks)[0];return{...face,t:bench.t,[hookName]:selector=>selector(store.getSnapshot())}}
export function makeSectionProps(bench,rows){const renders=[];return{t:bench.t,useTabs:selector=>selector(rows),renderSlot:(name,owner,options)=>{renders.push([name,owner,options]);return`${options.only}-panel`},renders}}

function createSlots(effects){const declared=new Set(['root']),rows=new Map(),injectors=new Map(),versions=new Map(),listeners=new Map();const bump=key=>{versions.set(key,(versions.get(key)??0)+1);for(const listener of listeners.get(key)??[])listener()};const reconcile=key=>{for(const item of injectors.get(key)??[]){if(declared.has(key)&&!item.cleanup)item.cleanup=item.setup();else if(!declared.has(key)&&item.cleanup){item.cleanup();item.cleanup=undefined}}};const undeclare=key=>{if(!declared.delete(key))return;reconcile(key);for(const row of [...(rows.get(key)??[])])row.dispose();rows.delete(key);bump(key)};return{register(options,component){if(!declared.has(options.name))throw new Error(`undeclared ${options.name}`);const row={options,component,disposed:false};const list=rows.get(options.name)??[];list.push(row);rows.set(options.name,list);for(const child of Object.keys(options.children??{})){declared.add(child);reconcile(child)}bump(options.name);const dispose=()=>{if(row.disposed)return;row.disposed=true;const at=(rows.get(options.name)??[]).indexOf(row);if(at>=0)rows.get(options.name).splice(at,1);for(const child of Object.keys(options.children??{}))undeclare(child);bump(options.name)};row.dispose=dispose;return dispose},inject(key,setup){const item={setup,cleanup:undefined};const list=injectors.get(key)??[];list.push(item);injectors.set(key,list);reconcile(key);const stop=()=>{item.cleanup?.();item.cleanup=undefined;const at=list.indexOf(item);if(at>=0)list.splice(at,1)};effects.push(stop);return stop},entries(key){return[...(rows.get(key)??[])].filter(row=>!row.disposed)},getVersion(key){return versions.get(key)??0},subscribe(key,listener){const set=listeners.get(key)??new Set();set.add(listener);listeners.set(key,set);return()=>set.delete(listener)},spec(key){return declared.has(key)?{kind:'list',scope:'root'}:undefined}}}
export function makeApplyBench(bench,api,scopes,declare=true){const effects=[],localeCalls=[],remoteHandlers=new Map(),localeListeners=new Set(),slots=createSlots(effects);let locale='en',revision=1;const dictionaries={};const localeFace={register(ns,copy){localeCalls.push([ns,copy]);dictionaries[ns]=copy;return()=>delete dictionaries[ns]},bind(ns){return key=>dictionaries[ns]?.[locale]?.[key]??key},getSnapshot(){return{locale,revision}},subscribe(listener){localeListeners.add(listener);return()=>localeListeners.delete(listener)}};const remote={$on(name,fn){remoteHandlers.set(name,fn);return()=>remoteHandlers.delete(name)}};const settingsScope={bind({namespace}){return scopes[namespace]}};const connection={api:api.api,isLoopback:true};const ctx={slots,locale:localeFace,remote,settingsScope,get(name){return name==='connection'?connection:undefined},effect(setup){const cleanup=setup();if(typeof cleanup==='function')effects.push(cleanup);return cleanup}};let rootDispose;const declareRoot=()=>{if(!rootDispose)rootDispose=slots.register({name:'root',children:{'settings.section':{kind:'list',scope:'root'}}},()=>null);return rootDispose};if(declare)declareRoot();return{ctx,slots,effects,localeCalls,remoteHandlers,declareRoot,dispose(){for(const cleanup of [...effects].reverse())cleanup();rootDispose?.()}}}
export function pEntries(bench,key){return bench.slots.entries(key)}
export function pEmit(map,name,arg){return map.get(name)?.(arg)}
export function pDispose(bench){bench.dispose()}
"#)]
extern "C" {
    fn makePluginsBench() -> JsValue;
    fn makeScope(initial: &JsValue) -> JsValue;
    fn makePluginsApi(configured: bool) -> JsValue;
    fn makeSectionProps(bench: &JsValue, rows: &JsValue) -> JsValue;
    fn makeApplyBench(bench: &JsValue, api: &JsValue, scopes: &JsValue, declare: bool) -> JsValue;
    fn pRender(bench: &JsValue, component: &JsValue, props: &JsValue) -> JsValue;
    fn pFind(root: &JsValue, key: &str, value: &JsValue) -> JsValue;
    fn pFindText(root: &JsValue, text: &str) -> JsValue;
    fn pText(root: &JsValue) -> String;
    fn pClick(node: &JsValue) -> JsValue;
    fn pChange(node: &JsValue, value: &str) -> JsValue;
    fn pKey(node: &JsValue, key: &str) -> JsValue;
    fn pProp(node: &JsValue, key: &str) -> JsValue;
    fn pCalls(value: &JsValue) -> Array;
    fn pTick() -> Promise;
    fn pScopePublish(scope: &JsValue, patch: &JsValue);
    fn pScopeAccept(scope: &JsValue, value: bool);
    fn pMakeCardProps(bench: &JsValue, face: &JsValue, hook_name: &str) -> JsValue;
    fn pEntries(bench: &JsValue, key: &str) -> Array;
    fn pEmit(map: &JsValue, name: &str, arg: &str) -> JsValue;
    fn pDispose(bench: &JsValue);
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

fn configure() -> JsValue {
    let bench = makePluginsBench();
    configure_client_ui_settings_plugins(
        property(&bench, "React"),
        property(&bench, "clsx").dyn_into().unwrap(),
        property(&bench, "primitives"),
        property(&bench, "resolveSlotLabel").dyn_into().unwrap(),
    )
    .unwrap();
    bench
}

#[wasm_bindgen_test]
fn compiled_fields_preserve_staged_accessibility_and_secret_posture() {
    let bench = configure();
    let edits = Array::new();
    let recorded = edits.clone();
    let on_edit = Closure::wrap(Box::new(move |text: String| {
        recorded.push(&JsValue::from_str(&text));
    }) as Box<dyn FnMut(String)>);
    let resets = Array::new();
    let reset_calls = resets.clone();
    let on_reset = Closure::wrap(Box::new(move || {
        reset_calls.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let props = object(&[
        ("id", JsValue::from_str("field")),
        ("label", JsValue::from_str("Command timeout")),
        ("hint", JsValue::from_str("How long one command may run.")),
        ("text", JsValue::from_str("soon")),
        ("overridden", JsValue::TRUE),
        ("invalid", JsValue::TRUE),
        ("overriddenLabel", JsValue::from_str("Overridden")),
        ("resetLabel", JsValue::from_str("Reset to default")),
        ("invalidLabel", JsValue::from_str("Enter a number.")),
        ("disabled", JsValue::FALSE),
        ("numeric", JsValue::TRUE),
        ("onEdit", on_edit.into_js_value()),
        ("onReset", on_reset.into_js_value()),
    ]);
    let tree = pRender(&bench, &plugins_value_field_component().unwrap(), &props);
    let input = pFind(&tree, "id", &JsValue::from_str("field"));
    assert_eq!(pProp(&input, "aria-invalid"), JsValue::TRUE);
    assert_eq!(
        pProp(&input, "inputMode").as_string().as_deref(),
        Some("numeric")
    );
    pChange(&input, "9000");
    pClick(&pFindText(&tree, "Reset to default"));
    assert_eq!(edits.get(0).as_string().as_deref(), Some("9000"));
    assert_eq!(resets.length(), 1);
    let secret = object(&[
        ("id", JsValue::from_str("key")),
        ("label", JsValue::from_str("API key")),
        ("hint", JsValue::from_str("Stored outside settings.")),
        ("text", JsValue::from_str("")),
        ("disabled", JsValue::FALSE),
        ("configured", JsValue::TRUE),
        ("stateLabel", JsValue::from_str("A key is configured.")),
        ("onEdit", Function::new_no_args("").into()),
    ]);
    let tree = pRender(&bench, &plugins_secret_field_component().unwrap(), &secret);
    assert_eq!(
        pProp(&pFind(&tree, "id", &JsValue::from_str("key")), "type")
            .as_string()
            .as_deref(),
        Some("password")
    );
    assert!(pText(&tree).contains("A key is configured."));
}

#[wasm_bindgen_test(async)]
async fn bash_controller_stages_renders_saves_resets_and_reports_refusal() {
    let bench = configure();
    let scope = makeScope(&js_sys::JSON::parse(r#"{"value":{"timeoutMs":60000,"maxOutputBytes":64000},"base":{"timeoutMs":60000,"maxOutputBytes":64000},"user":{}}"#).unwrap());
    let face = create_plugins_bash_controller(scope.clone()).unwrap();
    let props = pMakeCardProps(&bench, &face, "useBashCard");
    let component = plugins_bash_card_component().unwrap();
    let collapsed = pRender(&bench, &component, &props);
    pClick(&pFind(
        &collapsed,
        "aria-label",
        &JsValue::from_str("Show settings: Shell"),
    ));
    let open = pRender(&bench, &component, &props);
    pChange(
        &pFind(
            &open,
            "id",
            &JsValue::from_str("plugin-config-bash-timeout"),
        ),
        "9000",
    );
    let staged = pRender(&bench, &component, &props);
    assert!(pText(&staged).contains("Unsaved"));
    pClick(&pFindText(&staged, "Save"));
    JsFuture::from(pTick()).await.unwrap();
    let calls = pCalls(&scope);
    assert_eq!(
        property(&calls.get(0), "1").as_string().as_deref(),
        Some("timeoutMs")
    );
    pScopePublish(
        &scope,
        &js_sys::JSON::parse(
            r#"{"user":{"timeoutMs":9000},"value":{"timeoutMs":9000,"maxOutputBytes":64000}}"#,
        )
        .unwrap(),
    );
    let saved = pRender(&bench, &component, &props);
    pClick(&pFindText(&saved, "Reset to default"));
    pScopeAccept(&scope, false);
    pClick(&pFindText(&pRender(&bench, &component, &props), "Save"));
    JsFuture::from(pTick()).await.unwrap();
    assert!(pText(&pRender(&bench, &component, &props)).contains("Save failed."));
}

#[wasm_bindgen_test(async)]
async fn web_search_controller_reads_writes_and_switches_credential_references() {
    let bench = configure();
    let scope = makeScope(&js_sys::JSON::parse(r#"{"value":{},"base":{},"user":{}}"#).unwrap());
    let api = makePluginsApi(false);
    let face = create_plugins_web_search_controller(scope.clone(), property(&api, "api")).unwrap();
    JsFuture::from(pTick()).await.unwrap();
    let props = pMakeCardProps(&bench, &face, "useWebSearchCard");
    let component =
        seekdeep_client_ui_settings_plugins::plugins_web_search_card_component().unwrap();
    let collapsed = pRender(&bench, &component, &props);
    pClick(&pFind(
        &collapsed,
        "aria-label",
        &JsValue::from_str("Show settings: Web search"),
    ));
    let open = pRender(&bench, &component, &props);
    assert!(pText(&open).contains("No key is configured."));
    pChange(
        &pFind(
            &open,
            "id",
            &JsValue::from_str("plugin-config-web-search-key"),
        ),
        " ds-secret ",
    );
    let store = property(&property(&face, "hooks"), "webSearchCard");
    let snapshot = property(&store, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&store)
        .unwrap();
    assert_eq!(property(&snapshot, "dirty"), JsValue::TRUE);
    assert_eq!(
        property(&property(&snapshot, "apiKey"), "text")
            .as_string()
            .as_deref(),
        Some(" ds-secret ")
    );
    let staged = pRender(&bench, &component, &props);
    assert!(pText(&staged).contains("Unsaved"));
    let save = pFindText(&staged, "Save");
    assert_eq!(pProp(&save, "disabled"), JsValue::FALSE);
    pClick(&save);
    for _ in 0..4 {
        JsFuture::from(pTick()).await.unwrap();
    }
    let calls = pCalls(&api);
    let set = (0..calls.length())
        .map(|i| calls.get(i))
        .find(|call| property(call, "0").as_string().as_deref() == Some("set"))
        .unwrap_or_else(|| {
            panic!(
                "calls: {}",
                js_sys::JSON::stringify(&calls)
                    .unwrap()
                    .as_string()
                    .unwrap()
            )
        });
    assert_eq!(
        property(&property(&set, "1"), "value")
            .as_string()
            .as_deref(),
        Some("ds-secret")
    );
    assert!(pText(&pRender(&bench, &component, &props)).contains("A key is configured."));
    Reflect::set(
        &property(&api, "views"),
        &JsValue::from_str("SEARCH_KEY"),
        &object(&[("configured", JsValue::FALSE), ("writable", JsValue::FALSE)]),
    )
    .unwrap();
    pScopePublish(
        &scope,
        &js_sys::JSON::parse(r#"{"value":{"apiKeyEnv":"SEARCH_KEY"}}"#).unwrap(),
    );
    JsFuture::from(pTick()).await.unwrap();
    assert_eq!(
        pProp(
            &pFind(
                &pRender(&bench, &component, &props),
                "id",
                &JsValue::from_str("plugin-config-web-search-key")
            ),
            "disabled"
        ),
        JsValue::TRUE
    );
}

#[wasm_bindgen_test]
fn section_tabs_keep_visited_panels_and_follow_keyboard_navigation() {
    let bench = configure();
    let rows=js_sys::JSON::parse(r#"[{"id":"configurable","order":0,"label":"Plugin configuration"},{"id":"all","order":10,"label":"Plugin list"}]"#).unwrap();
    let props = makeSectionProps(&bench, &rows);
    let component = plugins_settings_section_component().unwrap();
    let first = pRender(&bench, &component, &props);
    let configurable = pFindText(&first, "Plugin configuration");
    assert_eq!(pProp(&configurable, "aria-selected"), JsValue::TRUE);
    pKey(&configurable, "ArrowRight");
    let second = pRender(&bench, &component, &props);
    assert_eq!(
        pProp(&pFindText(&second, "Plugin list"), "aria-selected"),
        JsValue::TRUE
    );
    assert!(pText(&second).contains("configurable-panel"));
    assert!(pText(&second).contains("all-panel"));
}

#[wasm_bindgen_test]
fn configurable_tab_switches_between_empty_copy_and_card_slot() {
    let bench = configure();
    let component = configurable_plugins_tab_component().unwrap();
    let empty = object(&[
        ("t", property(&bench, "t")),
        ("cardCount", JsValue::from_f64(0.0)),
        ("renderSlot", Function::new_no_args("").into()),
    ]);
    assert!(pText(&pRender(&bench, &component, &empty)).contains("no plugin settings"));
    let render =
        Closure::wrap(Box::new(move || JsValue::from_str("cards")) as Box<dyn FnMut() -> JsValue>);
    let cards = object(&[
        ("t", property(&bench, "t")),
        ("cardCount", JsValue::from_f64(3.0)),
        ("renderSlot", render.into_js_value()),
    ]);
    assert!(pText(&pRender(&bench, &component, &cards)).contains("cards"));
}

#[wasm_bindgen_test(async)]
async fn apply_registers_live_tabs_cards_invalidation_and_late_declarations() {
    let bench = configure();
    let api = makePluginsApi(false);
    let scopes = object(&[
        ("shell", makeScope(&object(&[]))),
        ("agent-loop", makeScope(&object(&[]))),
        ("web-search-deepseek", makeScope(&object(&[]))),
    ]);
    let apply = makeApplyBench(&bench, &api, &scopes, false);
    apply_client_ui_settings_plugins(property(&apply, "ctx")).unwrap();
    assert_eq!(settings_plugins_inject().length(), 5);
    let locale_call = Array::from(&property(&apply, "localeCalls")).get(0);
    let dictionaries = Array::from(&locale_call).get(1);
    assert_eq!(
        Object::keys(&Object::from(dictionaries))
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["zh", "en"]
    );
    assert_eq!(pEntries(&apply, "settings.section").length(), 0);
    property(&apply, "declareRoot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&apply)
        .unwrap();
    assert_eq!(pEntries(&apply, "settings.section").length(), 1);
    assert_eq!(pEntries(&apply, "settings.plugins.tab").length(), 1);
    assert_eq!(pEntries(&apply, "settings.plugin.item").length(), 3);
    let section = pEntries(&apply, "settings.section").get(0);
    let face = property(&property(&section, "options"), "inject")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let tabs = property(&property(&face, "hooks"), "tabs");
    let first = property(&tabs, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&tabs)
        .unwrap();
    let second = property(&tabs, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&tabs)
        .unwrap();
    assert!(Object::is(&first, &second));
    assert_eq!(property(&first, "length").as_f64(), Some(1.0));
    JsFuture::from(pTick()).await.unwrap();
    let before = pCalls(&api).length();
    pEmit(
        &property(&apply, "remoteHandlers"),
        "credentials/updated",
        "DEEPSEEK_API_KEY",
    );
    JsFuture::from(pTick()).await.unwrap();
    assert!(pCalls(&api).length() > before);
    pDispose(&apply);
    assert_eq!(pEntries(&apply, "settings.section").length(), 0);
    assert_eq!(pEntries(&apply, "settings.plugin.item").length(), 0);
}
