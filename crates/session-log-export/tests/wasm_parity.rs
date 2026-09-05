//! Live controller, React surface, and Client assembly parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_session_log_export::{
    apply_session_log_export, configure_session_log_export, configure_session_log_export_apply,
    create_session_log_download_controller, session_log_download_dialog_component,
    session_log_download_header_action_component, session_log_export_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten=values=>values.flat(Infinity).filter(value=>value!==null&&value!==undefined&&value!==false)
let cached
export function makeSessionExportBench(){
  if(cached){cached.reset();return cached}
  globalThis.window=globalThis
  Object.defineProperty(globalThis,'location',{configurable:true,value:{origin:'https://harness.example'}})
  const states=[];let si=0
  const Fragment=Symbol('Fragment')
  const React={Fragment,createElement(kind,supplied,...children){const flat=flatten(children),props={...(supplied??{})};if(flat.length===1)props.children=flat[0];else if(flat.length>1)props.children=flat;return{kind,props,children:flat}},useState(initial){const at=si++;if(!(at in states))states[at]=initial;return[states[at],value=>{states[at]=value}]}}
  function resolve(value){if(Array.isArray(value))return flatten(value.map(resolve));if(value===null||value===undefined||value===false||typeof value!=='object')return value;if(!('kind'in value))return value;if(typeof value.kind==='function')return resolve(value.kind(value.props));if(value.kind===Fragment)return{kind:'Fragment',props:value.props,children:flatten(value.children.map(resolve))};return{...value,children:flatten(value.children.map(resolve))}}
  const styles=[]
  globalThis.document={head:{appendChild(node){styles.push(node);return node}},createElement(kind){return{kind,attrs:{},setAttribute(key,value){this.attrs[key]=value},textContent:'',click(){this.clicked=true}}},querySelector(){return null}}
  const copy={'dialog.preparingTitle':'Exporting Session','dialog.preparingDescription':'Preparing a ZIP containing this Session, its sub-Sessions, and attachments.','dialog.successTitle':'Session download started','dialog.successDescription':'The browser is downloading the Session ZIP.','dialog.errorTitle':'Session export failed','dialog.commandFailed':'Could not start the Session export.','dialog.close':'Close'}
  cached={React,primitives:{Button:'Button',IconDownloadOutline16:'IconDownloadOutline16',Modal:'Modal'},styles,t:key=>copy[key]??key,render(component,props){si=0;return resolve(React.createElement(component,props))},reset(){states.length=0;styles.length=0}}
  return cached
}
export function makeDownloadOps(){const calls=[],saves=[];let mode='success',release;const fetcher=(url,init)=>{calls.push([String(url),init.method,init.signal]);if(mode==='pending')return new Promise(resolve=>{release=resolve});if(mode==='transport')return Promise.reject(new Error('offline'));return Promise.resolve(new Response(mode==='http'?'backend unavailable':'zip',{status:mode==='http'?500:200}))};const saver=(url,filename)=>saves.push([url,filename]);return{fetcher,saver,calls,saves,setMode(value){mode=value},release(){release?.(new Response('zip',{status:200}))}}}
export function xCall(value,name,...args){return value[name](...args)}
export function xProp(value,key){return value?.props?.[key]}
export function xSetMode(value,mode){value.setMode(mode)}
export function xRelease(value){value.release()}
export function xTick(){return new Promise(resolve=>setTimeout(resolve,0))}
export function makeSurfaceProps(bench,state){const calls=[];return{props:{sessionId:'session',useSessionLogDownload:selector=>selector(state),request:id=>calls.push(['request',id]),dismiss:id=>calls.push(['dismiss',id]),t:bench.t},calls}}
export function xRender(bench,component,props){return bench.render(component,props)}
function walk(root,out=[]){if(!root||typeof root!=='object')return out;if(Array.isArray(root)){root.forEach(value=>walk(value,out));return out}if('kind'in root)out.push(root);(root.children??[]).forEach(value=>walk(value,out));for(const key of ['footer'])walk(root.props?.[key],out);return out}
function textOf(root){const parts=[];const visit=value=>{if(typeof value==='string'||typeof value==='number')parts.push(String(value));else if(Array.isArray(value))value.forEach(visit);else if(value&&typeof value==='object')(value.children??[]).forEach(visit)};visit(root);return parts.join('')}
export function xFindKind(root,kind){return walk(root).find(node=>node.kind===kind)}
export function xFindText(root,text){return walk(root).find(node=>textOf(node)===text)}
export function xFindKindText(root,kind,text){return walk(root).find(node=>node.kind===kind&&textOf(node)===text)}
export function xSummary(root){return walk(root).map(node=>`${String(node.kind)}|${JSON.stringify(node.props)}|${textOf(node)}`).join('\n')}
export function xClick(node){return node.props.onClick?.()}
export function xCalls(value){return value.calls}
function slots(){const declared=new Set(),rows=new Map(),waiters=new Map();const reconcile=name=>{for(const waiter of waiters.get(name)??[]){if(declared.has(name)&&!waiter.cleanup)waiter.cleanup=waiter.setup();else if(!declared.has(name)&&waiter.cleanup){waiter.cleanup();waiter.cleanup=undefined}}};return{declare(name){declared.add(name);reconcile(name)},register(options,component){const row={options,component};const list=rows.get(options.name)??[];list.push(row);rows.set(options.name,list);return()=>{const index=list.indexOf(row);if(index>=0)list.splice(index,1)}},inject(name,setup){const waiter={setup};const list=waiters.get(name)??[];list.push(waiter);waiters.set(name,list);reconcile(name)},entries(name){return rows.get(name)??[]}}}
export function makeApplyFrame(){const slotService=slots(),calls=[],effects=[],events=new Map(),provided={},copies={};const ctx={slots:slotService,locale:{register(ns,value){copies[ns]=value;return()=>delete copies[ns]}},provide(name,value){provided[name]=value;calls.push(['provide',name])},effect(setup,label){calls.push(['effect',label]);const cleanup=setup();if(typeof cleanup==='function')effects.push(cleanup)},on(name,listener){events.set(name,listener)}};return{ctx,slots:slotService,calls,events,provided,copies,declare(){slotService.declare('conversation.session.header.utilities')},emit(...args){events.get('command/executed')?.(...args)},dispose(){return Promise.all(effects.reverse().map(cleanup=>cleanup()))}}}
export function xDeclare(frame){frame.declare()}
export function xEntries(frame,name){return frame.slots.entries(name)}
export function xEmit(frame,session,command,result){return frame.emit(session,command,result)}
export function xProvided(frame,name){return frame.provided[name]}
export function xDispose(frame){return frame.dispose()}
export function makeControllerConstructor(factory){return function SessionLogDownloadController(){return factory()}}
"#)]
extern "C" {
    fn makeSessionExportBench() -> JsValue;
    fn makeDownloadOps() -> JsValue;
    #[wasm_bindgen(variadic)]
    fn xCall(value: &JsValue, name: &str, args: &Array) -> JsValue;
    fn xProp(value: &JsValue, key: &str) -> JsValue;
    fn xSetMode(value: &JsValue, mode: &str);
    fn xRelease(value: &JsValue);
    fn xTick() -> Promise;
    fn makeSurfaceProps(bench: &JsValue, state: &JsValue) -> JsValue;
    fn xRender(bench: &JsValue, component: &JsValue, props: &JsValue) -> JsValue;
    fn xFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn xFindText(root: &JsValue, text: &str) -> JsValue;
    fn xFindKindText(root: &JsValue, kind: &str, text: &str) -> JsValue;
    fn xSummary(root: &JsValue) -> String;
    fn xClick(node: &JsValue) -> JsValue;
    fn xCalls(value: &JsValue) -> Array;
    fn makeApplyFrame() -> JsValue;
    fn xDeclare(frame: &JsValue);
    fn xEntries(frame: &JsValue, name: &str) -> Array;
    fn xEmit(frame: &JsValue, session: &str, command: &str, result: &JsValue) -> JsValue;
    fn xProvided(frame: &JsValue, name: &str) -> JsValue;
    fn xDispose(frame: &JsValue) -> Promise;
    fn makeControllerConstructor(factory: &Function) -> Function;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn call(value: &JsValue, name: &str, arguments: &[JsValue]) -> JsValue {
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    xCall(value, name, &args)
}

fn configure() -> JsValue {
    let bench = makeSessionExportBench();
    configure_session_log_export(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    bench
}

#[wasm_bindgen_test(async)]
async fn controller_face_publishes_one_flight_success_failure_and_dismissal() {
    let _bench = configure();
    let operations = makeDownloadOps();
    let controller = create_session_log_download_controller(
        Some(property(&operations, "fetcher").dyn_into().unwrap()),
        Some(property(&operations, "saver").dyn_into().unwrap()),
    )
    .unwrap();
    let store = property(&controller, "store");
    let notifications = Array::new();
    let captured = notifications.clone();
    let listener = Closure::wrap(Box::new(move || {
        captured.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let unsubscribe = call(&store, "subscribe", &[listener.into_js_value()])
        .dyn_into::<Function>()
        .unwrap();
    xSetMode(&operations, "pending");
    let first = call(&controller, "download", &[JsValue::from_str("a/b")]);
    let second = call(&controller, "download", &[JsValue::from_str("a/b")]);
    let state = call(&store, "getSnapshot", &[]);
    assert_eq!(
        property(&property(&state, "bySession"), "a/b")
            .dyn_into::<Object>()
            .ok()
            .and_then(|entry| property(&entry, "status").as_string())
            .as_deref(),
        Some("downloading")
    );
    JsFuture::from(xTick()).await.unwrap();
    xRelease(&operations);
    JsFuture::from(first.dyn_into::<Promise>().unwrap())
        .await
        .unwrap();
    JsFuture::from(second.dyn_into::<Promise>().unwrap())
        .await
        .unwrap();
    assert_eq!(
        property(&operations, "calls")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        1
    );
    assert_eq!(
        property(&operations, "saves")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        1
    );
    let saved = Array::from(&property(&operations, "saves")).get(0);
    assert_eq!(
        Array::from(&saved).get(1).as_string().as_deref(),
        Some("seekdeep-session-a_b.zip")
    );
    call(&controller, "dismiss", &[JsValue::from_str("a/b")]);
    let dismissed = call(&store, "getSnapshot", &[]);
    assert_eq!(
        property(&property(&dismissed, "bySession"), "a/b")
            .dyn_into::<Object>()
            .ok()
            .map(|entry| property(&entry, "open")),
        Some(JsValue::FALSE)
    );
    xSetMode(&operations, "http");
    JsFuture::from(
        call(&controller, "download", &[JsValue::from_str("failure")])
            .dyn_into::<Promise>()
            .unwrap(),
    )
    .await
    .unwrap();
    let failed = call(&store, "getSnapshot", &[]);
    assert!(
        property(&property(&failed, "bySession"), "failure")
            .dyn_into::<Object>()
            .ok()
            .and_then(|entry| property(&entry, "error").as_string())
            .is_some_and(|error| error.contains("HTTP 500 backend unavailable"))
    );
    assert!(notifications.length() >= 4);
    unsubscribe.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(
        call(&controller, "dispose", &[])
            .dyn_into::<Promise>()
            .unwrap(),
    )
    .await
    .unwrap();
}

#[wasm_bindgen_test]
fn dialog_and_header_preserve_copy_busy_and_shared_actions() {
    let bench = configure();
    let empty = js_sys::JSON::parse(r#"{"bySession":{}}"#).unwrap();
    let frame = makeSurfaceProps(&bench, &empty);
    let header = xRender(
        &bench,
        &session_log_download_header_action_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(
        xProp(&xFindKind(&header, "button"), "disabled"),
        JsValue::FALSE
    );
    let downloading = js_sys::JSON::parse(
        r#"{"bySession":{"session":{"open":true,"status":"downloading","error":null}}}"#,
    )
    .unwrap();
    let frame = makeSurfaceProps(&bench, &downloading);
    let header = xRender(
        &bench,
        &session_log_download_header_action_component().unwrap(),
        &property(&frame, "props"),
    );
    let button = xFindKind(&header, "button");
    assert_eq!(
        xProp(&button, "disabled"),
        JsValue::TRUE,
        "{}",
        xSummary(&header)
    );
    assert_eq!(xProp(&button, "aria-busy"), JsValue::TRUE);
    assert_eq!(
        xProp(&xFindKind(&header, "Modal"), "title")
            .as_string()
            .as_deref(),
        Some("Exporting Session")
    );
    let failed = js_sys::JSON::parse(
        r#"{"bySession":{"session":{"open":true,"status":"error","error":""}}}"#,
    )
    .unwrap();
    let frame = makeSurfaceProps(&bench, &failed);
    let dialog = xRender(
        &bench,
        &session_log_download_dialog_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(
        xProp(&xFindKind(&dialog, "Modal"), "description")
            .as_string()
            .as_deref(),
        Some("Could not start the Session export.")
    );
    xClick(&xFindKindText(&dialog, "Button", "Close"));
    assert_eq!(
        Array::from(&xCalls(&frame))
            .get(0)
            .dyn_into::<Array>()
            .unwrap()
            .get(0)
            .as_string()
            .as_deref(),
        Some("dismiss")
    );
}

#[wasm_bindgen_test(async)]
async fn apply_owns_controller_locale_header_and_successful_export_events() {
    let _bench = configure();
    let operations = makeDownloadOps();
    let fetcher = property(&operations, "fetcher")
        .dyn_into::<Function>()
        .unwrap();
    let saver = property(&operations, "saver")
        .dyn_into::<Function>()
        .unwrap();
    let constructor = Closure::wrap(Box::new(move || {
        create_session_log_download_controller(Some(fetcher.clone()), Some(saver.clone()))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let factory = constructor.into_js_value().dyn_into::<Function>().unwrap();
    configure_session_log_export_apply(makeControllerConstructor(&factory));
    let frame = makeApplyFrame();
    apply_session_log_export(property(&frame, "ctx")).unwrap();
    assert_eq!(
        session_log_export_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        vec!["slots", "locale"]
    );
    assert!(xProvided(&frame, "sessionLogDownload").is_object());
    assert_eq!(
        xEntries(&frame, "conversation.session.header.utilities").length(),
        0
    );
    xDeclare(&frame);
    let entry = xEntries(&frame, "conversation.session.header.utilities").get(0);
    assert!(Object::is(
        &property(&entry, "component"),
        &session_log_download_header_action_component().unwrap()
    ));
    assert_eq!(
        property(&property(&entry, "options"), "id")
            .as_string()
            .as_deref(),
        Some("session-log-download")
    );
    xEmit(
        &frame,
        "session",
        "plan",
        &js_sys::JSON::parse(r#"{"kind":"success"}"#).unwrap(),
    );
    assert_eq!(
        property(&operations, "calls")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        0
    );
    xEmit(
        &frame,
        "session",
        "export",
        &js_sys::JSON::parse(r#"{"kind":"success"}"#).unwrap(),
    );
    JsFuture::from(xTick()).await.unwrap();
    assert_eq!(
        property(&operations, "calls")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        1
    );
    JsFuture::from(xDispose(&frame)).await.unwrap();
}
