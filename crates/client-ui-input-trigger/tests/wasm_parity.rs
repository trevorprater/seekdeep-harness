//! Live Rust/WASM trigger service, controller, source pipeline, and `MenuView` parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_input_trigger::{
    apply_client_ui_input_trigger, configure_client_ui_input_trigger, exported_menu_view_component,
    input_trigger_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
class FakeNode {
  constructor(kind, props, children) { this.kind=kind; this.props=props??{}; this.children=children.flat(Infinity).filter(v=>v!==null&&v!==undefined&&v!==false); this.parentElement=null; this.scrolled=0; for(const child of this.children)if(child instanceof FakeNode)child.parentElement=this }
  contains(target){return target===this||this.children.some(child=>child instanceof FakeNode&&child.contains(target))}
  closest(){return this.composerCard??null}
  scrollIntoView(){this.scrolled++}
}
globalThis.Node=FakeNode
const styles=[],listeners=new Map(),ids=new Map()
if(typeof globalThis.document==='undefined')globalThis.document={}
Object.assign(document,{currentScript:null,querySelector(selector){const match=/^style\[data-plugin=(.+)\]$/.exec(selector);if(!match)return null;const plugin=JSON.parse(match[1]);return styles.find(n=>n.attributes['data-plugin']===plugin)??null},querySelectorAll(selector){const n=this.querySelector(selector);return n?[n]:[]},createElement(kind){return{kind,attributes:{},textContent:'',setAttribute(name,value){this.attributes[name]=value}}},head:{appendChild(node){styles.push(node);return node}},addEventListener(name,fn){const rows=listeners.get(name)??new Set();rows.add(fn);listeners.set(name,rows)},removeEventListener(name,fn){listeners.get(name)?.delete(fn)},getElementById(id){return ids.get(id)??null}})
const ZH={command:'命令',skill:'技能',subagent:'子智能体',loading:'正在加载…','suggestions.aria':'触发候选建议'}

function hooks(){const states=[],refs=[],effects=[];let si=0,ri=0,ei=0;const Fragment=Symbol('Fragment');const React={Fragment,createElement(kind,props,...children){const node=new FakeNode(kind,props,children);if(props?.id)ids.set(props.id,node);if(props?.ref&&typeof props.ref==='object')props.ref.current=node;return node},useRef(initial){const i=ri++;if(!(i in refs))refs[i]={current:initial};return refs[i]},useEffect(run,deps){const i=ei++;const old=effects[i];const changed=!old||deps.length!==old.deps.length||deps.some((v,j)=>!Object.is(v,old.deps[j]));if(changed){old?.cleanup?.();const cleanup=run();effects[i]={deps:[...deps],cleanup:typeof cleanup==='function'?cleanup:undefined}}},useSyncExternalStore(_sub,get){return get()}};return{React,render(component,props){si=0;ri=0;ei=0;return component(props)},dispose(){for(const e of effects.reverse())e?.cleanup?.()}}}
function textOf(node){if(node===null||node===undefined||node===false)return'';if(typeof node==='string'||typeof node==='number')return String(node);if(Array.isArray(node))return node.map(textOf).join('');return(node.children??[]).map(textOf).join('')}
function all(node,predicate,rows=[]){if(node===null||node===undefined||node===false||typeof node==='string'||typeof node==='number')return rows;if(!Array.isArray(node)&&predicate(node))rows.push(node);for(const child of Array.isArray(node)?node:node.children??[])all(child,predicate,rows);return rows}
function one(node,p){return all(node,p)[0]}
function scope(id){const effects=[],events=[];const actx={id,effect(setup){const off=setup();effects.push(off);return off},bail(subject,event,payload){events.push({subject,event,payload});return actx.accept!==false}};return{actx,effects,events,dispose(){for(const off of effects.splice(0).reverse())off()}}}

export function makeTriggerBench(){const h=hooks(),services=new Map(),effects=[],entries=[],locales=[],scopes=new Map();const sessions={scope(id){return scopes.get(id)?.actx},scopeOf(actx){return actx?.id}};const own=off=>{effects.push(off);return off};const ctx={sessions,locale:{register(ns,dictionaries){const row={ns,dictionaries};locales.push(row);return()=>locales.splice(locales.indexOf(row),1)}},slots:{inject(_name,install){return own(install())},register(options,component){const row={options,component};entries.push(row);return()=>entries.splice(entries.indexOf(row),1)}},reflect:{provide(name,value){services.set(name,value);return()=>services.delete(name)}},inject(_deps,callback){callback(ctx)},effect(setup){return own(setup())}};return{hooks:h,React:h.React,primitives:{useAnchoredMaxHeight(){return 280}},ctx,services,effects,entries,locales,scopes,mint(id){const s=scope(id);scopes.set(id,s);return s}}}
export function triggerService(bench){return bench.services.get('inputTriggers')}
export function triggerEntries(bench){return bench.entries}
export function triggerLocales(bench){return bench.locales}
export function triggerMint(bench,id){return bench.mint(id)}
export function triggerDisposeScope(scope){scope.dispose()}
export function triggerEvents(scope){return scope.events}
export function triggerSetAccept(scope,value){scope.actx.accept=value}
export function triggerDispose(bench){bench.hooks.dispose();for(const off of bench.effects.splice(0).reverse())off()}
export function triggerTick(){return Promise.resolve().then(()=>Promise.resolve()).then(()=>Promise.resolve())}
export function triggerStyleCount(){return styles.filter(n=>n.attributes['data-plugin']==='@seekdeep-ai/seekdeep-client-ui-input-trigger').length}

export function makeSource(trigger,name,order=0){const calls={warm:[],candidates:[],picks:[],space:[],enter:[],serialized:[],lexiconListeners:new Set()};let candidates=[{name:name+'-one',description:'first',icon:'*'},{name:name+'-two'}],lexicon=[name+'-one'],candidateMode='success';const source={trigger,name,order,calls,warm(session){calls.warm.push(session)},candidates(session,req){calls.candidates.push({session,req});if(candidateMode==='pending')return new Promise(resolve=>{source.resolve=()=>resolve(candidates)});if(candidateMode==='failure')return Promise.reject(new Error('boom'));return Promise.resolve(candidates)},onPick(input){calls.picks.push(input);return source.pickOutcomeSet?source.pickOutcome:{text:'/'+input.candidate.name+' '}},matchSpace(session,token){calls.space.push({session,token});return source.spaceOutcome},matchEnter(session,line,signal){calls.enter.push({session,line,signal});return source.enterFailure?Promise.reject(new Error(source.enterFailure)):Promise.resolve(source.enterOutcome)},lexicon(){return source.lexiconValue},subscribeLexicon(_session,listener){calls.lexiconListeners.add(listener);return()=>calls.lexiconListeners.delete(listener)},codec:{clipboardText(ref){return'/'+ref},serialize(ref,signal){calls.serialized.push({ref,signal});return Promise.resolve('<'+ref+'>')}}};source.setCandidates=v=>{candidates=v};source.setCandidateMode=v=>{candidateMode=v};source.setLexicon=v=>{source.lexiconValue=v};source.lexiconValue=lexicon;return source}
export function sourceCalls(source){return source.calls}
export function sourceSetMode(source,mode){source.setCandidateMode(mode)}
export function sourceSetCandidates(source,value){source.setCandidates(value)}
export function sourceResolve(source){source.resolve?.()}
export function sourceSetLexicon(source,value){source.setLexicon(value)}
export function sourceNotifyLexicon(source){for(const fn of[...source.calls.lexiconListeners])fn()}
export function sourceSetSpaceOutcome(source,value){source.spaceOutcome=value}
export function sourceSetEnterOutcome(source,value){source.enterOutcome=value}
export function sourceSetEnterFailure(source,value){source.enterFailure=value}
export function sourceSetPickOutcome(source,value){source.pickOutcomeSet=true;source.pickOutcome=value}
export function sourceCandidate(name){return{name}}
export function triggerStoreSnapshot(store){return store.getSnapshot()}
export function triggerMenuRender(bench,component,props){return bench.hooks.render(component,props)}
export function triggerMenuProps(menu,picks,dismisses){return{menu,onPick(source,index){picks.push([source,index])},onDismiss(){dismisses.push(true)},t(key){return ZH[key]??key}}}
export function triggerText(tree){return textOf(tree)}
export function triggerOptions(tree){return all(tree,n=>n.props?.role==='option')}
export function triggerListbox(tree){return one(tree,n=>n.props?.role==='listbox')}
export function triggerMouseDown(node){let prevented=false;node.props.onMouseDown({preventDefault(){prevented=true}});return prevented}
export function triggerDispatchPointer(target){for(const fn of[...(listeners.get('pointerdown')??[])])fn({target})}
export function triggerBody(){return document.body??new FakeNode('body',{},[])}
export function triggerSignal(){return new AbortController().signal}
export function triggerAbortedSignal(){const controller=new AbortController();controller.abort('stop');return controller.signal}
"#)]
extern "C" {
    fn makeTriggerBench() -> JsValue;
    fn triggerService(bench: &JsValue) -> JsValue;
    fn triggerEntries(bench: &JsValue) -> Array;
    fn triggerLocales(bench: &JsValue) -> Array;
    fn triggerMint(bench: &JsValue, id: &str) -> JsValue;
    fn triggerDisposeScope(scope: &JsValue);
    fn triggerEvents(scope: &JsValue) -> Array;
    fn triggerSetAccept(scope: &JsValue, value: bool);
    fn triggerDispose(bench: &JsValue);
    fn triggerTick() -> Promise;
    fn triggerStyleCount() -> u32;
    fn makeSource(trigger: &str, name: &str, order: f64) -> JsValue;
    fn sourceCalls(source: &JsValue) -> JsValue;
    fn sourceSetMode(source: &JsValue, mode: &str);
    fn sourceSetCandidates(source: &JsValue, value: &JsValue);
    fn sourceResolve(source: &JsValue);
    fn sourceSetLexicon(source: &JsValue, value: &JsValue);
    fn sourceNotifyLexicon(source: &JsValue);
    fn sourceSetSpaceOutcome(source: &JsValue, value: &JsValue);
    fn sourceSetEnterOutcome(source: &JsValue, value: &JsValue);
    fn sourceSetEnterFailure(source: &JsValue, value: &str);
    fn sourceSetPickOutcome(source: &JsValue, value: &JsValue);
    fn sourceCandidate(name: &str) -> JsValue;
    fn triggerStoreSnapshot(store: &JsValue) -> JsValue;
    fn triggerMenuRender(bench: &JsValue, component: &Function, props: &JsValue) -> JsValue;
    fn triggerMenuProps(menu: &JsValue, picks: &Array, dismisses: &Array) -> JsValue;
    fn triggerText(tree: &JsValue) -> String;
    fn triggerOptions(tree: &JsValue) -> Array;
    fn triggerListbox(tree: &JsValue) -> JsValue;
    fn triggerMouseDown(node: &JsValue) -> bool;
    fn triggerDispatchPointer(target: &JsValue);
    fn triggerBody() -> JsValue;
    fn triggerSignal() -> JsValue;
    fn triggerAbortedSignal() -> JsValue;
}
fn property(value: &JsValue, key: &str) -> JsValue {
    let direct = Reflect::get(value, &JsValue::from_str(key)).unwrap();
    if !direct.is_undefined() {
        return direct;
    }
    let props = Reflect::get(value, &JsValue::from_str("props")).unwrap_or(JsValue::UNDEFINED);
    Reflect::get(&props, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}
fn call(value: &JsValue, key: &str) -> Function {
    property(value, key).dyn_into().unwrap()
}
fn configure(bench: &JsValue) {
    configure_client_ui_input_trigger(property(bench, "React"), property(bench, "primitives"))
        .unwrap();
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn service_controller_fetch_pick_arbitration_lexicon_and_scope_lifecycle_are_live() {
    let bench = makeTriggerBench();
    configure(&bench);
    apply_client_ui_input_trigger(property(&bench, "ctx")).unwrap();
    assert_eq!(
        input_trigger_inject()
            .iter()
            .map(|v| v.as_string().unwrap())
            .collect::<Vec<_>>(),
        ["sessions", "locale"]
    );
    assert_eq!(triggerStyleCount(), 1);
    assert_eq!(triggerLocales(&bench).length(), 1);
    assert_eq!(triggerEntries(&bench).length(), 1);
    let service = triggerService(&bench);
    let command = makeSource("/", "command", 0.0);
    let skill = makeSource("/", "skill", 2.0);
    let at = makeSource("@", "command", 0.0);
    let off_command = call(&service, "registerSource")
        .call1(&service, &command)
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    call(&service, "registerSource")
        .call1(&service, &skill)
        .unwrap();
    call(&service, "registerSource")
        .call1(&service, &at)
        .unwrap();
    assert!(
        call(&service, "registerSource")
            .call1(&service, &command)
            .is_err()
    );
    let scope = triggerMint(&bench, "s1");
    let controller = call(&service, "sessionOf")
        .call1(&service, &property(&scope, "actx"))
        .unwrap();
    let same = call(&service, "sessionOf")
        .call1(&service, &property(&scope, "actx"))
        .unwrap();
    assert!(Object::is(&controller, &same));
    assert_eq!(
        Array::from(&property(&sourceCalls(&command), "warm")).length(),
        1
    );
    call(&controller, "track")
        .call4(
            &controller,
            &JsValue::from_str("/co"),
            &JsValue::from_f64(3.0),
            &js_sys::JSON::parse(r#"{"tier":"plain"}"#).unwrap(),
            &JsValue::from_f64(7.0),
        )
        .unwrap();
    let pending = triggerStoreSnapshot(&property(&controller, "menu"));
    assert_eq!(property(&pending, "open").as_bool(), Some(true));
    assert_eq!(Array::from(&property(&pending, "groups")).length(), 2);
    JsFuture::from(triggerTick()).await.unwrap();
    let ready = triggerStoreSnapshot(&property(&controller, "menu"));
    assert_eq!(
        property(&property(&ready, "highlight"), "source")
            .as_string()
            .as_deref(),
        Some("command")
    );
    assert!(property(&property(&ready, "hit"), "span").is_object());
    call(&controller, "pick")
        .call2(
            &controller,
            &JsValue::from_str("command"),
            &JsValue::from_f64(0.0),
        )
        .unwrap();
    let events = triggerEvents(&scope);
    assert_eq!(
        property(&events.get(0), "event").as_string().as_deref(),
        Some("slash/input-insert-text")
    );
    assert_eq!(
        call(&controller, "arbitrate")
            .call2(&controller, &JsValue::from_str("down"), &JsValue::FALSE)
            .unwrap()
            .as_string()
            .as_deref(),
        Some("pass")
    );
    call(&controller, "track")
        .call4(
            &controller,
            &JsValue::from_str("/co"),
            &JsValue::from_f64(3.0),
            &js_sys::JSON::parse(r#"{"tier":"plain"}"#).unwrap(),
            &JsValue::from_f64(8.0),
        )
        .unwrap();
    JsFuture::from(triggerTick()).await.unwrap();
    assert_eq!(
        call(&controller, "arbitrate")
            .call2(&controller, &JsValue::from_str("down"), &JsValue::FALSE)
            .unwrap()
            .as_string()
            .as_deref(),
        Some("consumed")
    );
    let lexicon = triggerStoreSnapshot(&property(&controller, "lexicon"))
        .dyn_into::<js_sys::Map>()
        .unwrap();
    assert_eq!(
        Array::from(&lexicon.get(&JsValue::from_str("/"))).length(),
        2
    );
    sourceSetLexicon(&skill, &Array::of1(&JsValue::from_str("new-skill")));
    sourceNotifyLexicon(&skill);
    let lexicon = triggerStoreSnapshot(&property(&controller, "lexicon"))
        .dyn_into::<js_sys::Map>()
        .unwrap();
    assert_eq!(
        Array::from(&lexicon.get(&JsValue::from_str("/")))
            .get(1)
            .as_string()
            .as_deref(),
        Some("new-skill")
    );
    let serialized = call(&controller, "serializeReference")
        .call3(
            &controller,
            &JsValue::from_str("skill"),
            &JsValue::from_str("x"),
            &JsValue::UNDEFINED,
        )
        .unwrap();
    assert_eq!(
        JsFuture::from(Promise::resolve(&serialized))
            .await
            .unwrap()
            .as_string()
            .as_deref(),
        Some("<x>")
    );
    sourceSetEnterOutcome(
        &command,
        &js_sys::JSON::parse(r#"{"text":"done"}"#).unwrap(),
    );
    let adjudicated = call(&controller, "adjudicate")
        .call2(
            &controller,
            &JsValue::from_str("/command"),
            &triggerSignal(),
        )
        .unwrap();
    assert_eq!(
        property(
            &JsFuture::from(Promise::resolve(&adjudicated))
                .await
                .unwrap(),
            "text"
        )
        .as_string()
        .as_deref(),
        Some("done")
    );
    off_command.call0(&JsValue::UNDEFINED).unwrap();
    triggerDisposeScope(&scope);
    let reborn_scope = triggerMint(&bench, "s1");
    let reborn = call(&service, "sessionOf")
        .call1(&service, &property(&reborn_scope, "actx"))
        .unwrap();
    assert!(!Object::is(&controller, &reborn));
    triggerDispose(&bench);
    assert_eq!(triggerEntries(&bench).length(), 0);
}

#[wasm_bindgen_test(async)]
async fn query_supersession_abort_empty_failure_launcher_and_space_outcomes_are_live() {
    let bench = makeTriggerBench();
    configure(&bench);
    apply_client_ui_input_trigger(property(&bench, "ctx")).unwrap();
    let service = triggerService(&bench);
    let source = makeSource("/", "command", 0.0);
    sourceSetMode(&source, "pending");
    call(&service, "registerSource")
        .call1(&service, &source)
        .unwrap();
    let scope = triggerMint(&bench, "s1");
    let controller = call(&service, "sessionOf")
        .call1(&service, &property(&scope, "actx"))
        .unwrap();
    let guard = js_sys::JSON::parse(r#"{"tier":"plain"}"#).unwrap();
    call(&controller, "track")
        .call4(
            &controller,
            &JsValue::from_str("/a"),
            &JsValue::from_f64(2.0),
            &guard,
            &JsValue::from_f64(1.0),
        )
        .unwrap();
    let first = Array::from(&property(&sourceCalls(&source), "candidates")).get(0);
    call(&controller, "track")
        .call4(
            &controller,
            &JsValue::from_str("/ab"),
            &JsValue::from_f64(3.0),
            &guard,
            &JsValue::from_f64(2.0),
        )
        .unwrap();
    assert_eq!(
        property(&property(&property(&first, "req"), "signal"), "aborted").as_bool(),
        Some(true)
    );
    sourceSetMode(&source, "success");
    call(&controller, "track")
        .call4(
            &controller,
            &JsValue::from_str("/ab"),
            &JsValue::from_f64(3.0),
            &guard,
            &JsValue::from_f64(3.0),
        )
        .unwrap();
    JsFuture::from(triggerTick()).await.unwrap();
    sourceSetSpaceOutcome(
        &source,
        &js_sys::JSON::parse(r#"{"text":"/done "}"#).unwrap(),
    );
    assert_eq!(
        call(&controller, "onSpace")
            .call0(&controller)
            .unwrap()
            .as_bool(),
        Some(true)
    );
    triggerSetAccept(&scope, false);
    call(&controller, "track")
        .call4(
            &controller,
            &JsValue::from_str("/ab"),
            &JsValue::from_f64(3.0),
            &guard,
            &JsValue::from_f64(4.0),
        )
        .unwrap();
    JsFuture::from(triggerTick()).await.unwrap();
    assert_eq!(
        call(&controller, "onSpace")
            .call0(&controller)
            .unwrap()
            .as_bool(),
        Some(false)
    );
    let hit=js_sys::JSON::parse(r#"{"trigger":"/","query":"","position":"leading","span":{"start":0,"end":0,"draftRev":5}}"#).unwrap();
    call(&controller, "toggleSource")
        .call2(&controller, &JsValue::from_str("command"), &hit)
        .unwrap();
    assert_eq!(
        triggerStoreSnapshot(&property(&controller, "launcher"))
            .as_string()
            .as_deref(),
        Some("command")
    );
    call(&controller, "toggleSource")
        .call2(&controller, &JsValue::from_str("command"), &hit)
        .unwrap();
    assert!(triggerStoreSnapshot(&property(&controller, "launcher")).is_null());
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn late_sources_outcome_variants_failures_empty_and_disposal_are_live() {
    let bench = makeTriggerBench();
    configure(&bench);
    apply_client_ui_input_trigger(property(&bench, "ctx")).unwrap();
    let service = triggerService(&bench);
    let scope = triggerMint(&bench, "late");
    let controller = call(&service, "sessionOf")
        .call1(&service, &property(&scope, "actx"))
        .unwrap();
    let source = makeSource("/", "late", 0.0);
    sourceSetCandidates(&source, &Array::of1(&sourceCandidate("row")));
    let off = call(&service, "registerSource")
        .call1(&service, &source)
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(
        Array::from(&property(&sourceCalls(&source), "warm")).length(),
        1
    );
    let guard = js_sys::JSON::parse(r#"{"tier":"plain"}"#).unwrap();
    let outcomes = [
        (
            js_sys::JSON::parse(r#"{"claim":{"token":"/row"}}"#).unwrap(),
            "slash/input-begin-command",
        ),
        (
            js_sys::JSON::parse(
                r#"{"insert":{"source":"late","ref":"row","label":"row","clipboardText":"/row"}}"#,
            )
            .unwrap(),
            "slash/input-insert-reference",
        ),
        (
            js_sys::JSON::parse(r#"{"text":"/row "}"#).unwrap(),
            "slash/input-insert-text",
        ),
    ];
    for (revision, (outcome, expected)) in outcomes.into_iter().enumerate() {
        sourceSetPickOutcome(&source, &outcome);
        call(&controller, "track")
            .call4(
                &controller,
                &JsValue::from_str("/r"),
                &JsValue::from_f64(2.0),
                &guard,
                &JsValue::from_f64(usize_as_f64(revision + 1)),
            )
            .unwrap();
        JsFuture::from(triggerTick()).await.unwrap();
        call(&controller, "pick")
            .call2(
                &controller,
                &JsValue::from_str("late"),
                &JsValue::from_f64(0.0),
            )
            .unwrap();
        let events = triggerEvents(&scope);
        assert_eq!(
            property(&events.get(events.length() - 1), "event")
                .as_string()
                .as_deref(),
            Some(expected)
        );
    }
    let before = triggerEvents(&scope).length();
    sourceSetPickOutcome(&source, &JsValue::from_str("handled"));
    call(&controller, "track")
        .call4(
            &controller,
            &JsValue::from_str("/r"),
            &JsValue::from_f64(2.0),
            &guard,
            &JsValue::from_f64(9.0),
        )
        .unwrap();
    JsFuture::from(triggerTick()).await.unwrap();
    assert_eq!(
        call(&controller, "arbitrate")
            .call2(&controller, &JsValue::from_str("enter"), &JsValue::FALSE,)
            .unwrap()
            .as_string()
            .as_deref(),
        Some("pick-highlighted")
    );
    assert_eq!(triggerEvents(&scope).length(), before);
    off.call0(&JsValue::UNDEFINED).unwrap();
    assert!(
        !property(
            &triggerStoreSnapshot(&property(&controller, "menu")),
            "open"
        )
        .as_bool()
        .unwrap()
    );

    let failing = makeSource("/", "failing", 0.0);
    sourceSetMode(&failing, "failure");
    call(&service, "registerSource")
        .call1(&service, &failing)
        .unwrap();
    call(&controller, "track")
        .call4(
            &controller,
            &JsValue::from_str("/f"),
            &JsValue::from_f64(2.0),
            &guard,
            &JsValue::from_f64(10.0),
        )
        .unwrap();
    JsFuture::from(triggerTick()).await.unwrap();
    assert_eq!(
        property(
            &triggerStoreSnapshot(&property(&controller, "menu")),
            "open"
        )
        .as_bool(),
        Some(false)
    );
    sourceSetMode(&failing, "success");
    sourceSetCandidates(&failing, &Array::new());
    call(&controller, "track")
        .call4(
            &controller,
            &JsValue::from_str("/x"),
            &JsValue::from_f64(2.0),
            &guard,
            &JsValue::from_f64(11.0),
        )
        .unwrap();
    JsFuture::from(triggerTick()).await.unwrap();
    assert_eq!(
        property(
            &triggerStoreSnapshot(&property(&controller, "menu")),
            "open"
        )
        .as_bool(),
        Some(false)
    );
}

#[wasm_bindgen_test(async)]
async fn enter_polling_rejection_abort_and_missing_serializer_are_live() {
    let bench = makeTriggerBench();
    configure(&bench);
    apply_client_ui_input_trigger(property(&bench, "ctx")).unwrap();
    let service = triggerService(&bench);
    let first = makeSource("/", "first", 0.0);
    let second = makeSource("/", "second", 1.0);
    sourceSetEnterOutcome(&second, &JsValue::from_str("handled"));
    call(&service, "registerSource")
        .call1(&service, &first)
        .unwrap();
    call(&service, "registerSource")
        .call1(&service, &second)
        .unwrap();
    let scope = triggerMint(&bench, "poll");
    let controller = call(&service, "sessionOf")
        .call1(&service, &property(&scope, "actx"))
        .unwrap();
    let result = call(&controller, "adjudicate")
        .call2(
            &controller,
            &JsValue::from_str("/anything"),
            &triggerSignal(),
        )
        .unwrap();
    assert_eq!(
        JsFuture::from(Promise::resolve(&result))
            .await
            .unwrap()
            .as_string()
            .as_deref(),
        Some("handled")
    );
    assert_eq!(
        Array::from(&property(&sourceCalls(&first), "enter")).length(),
        1
    );
    assert_eq!(
        Array::from(&property(&sourceCalls(&second), "enter")).length(),
        1
    );
    sourceSetEnterFailure(&first, "warm failed");
    let rejected = call(&controller, "adjudicate")
        .call2(
            &controller,
            &JsValue::from_str("/anything"),
            &triggerSignal(),
        )
        .unwrap();
    assert!(JsFuture::from(Promise::resolve(&rejected)).await.is_err());
    let aborted = call(&controller, "adjudicate")
        .call2(
            &controller,
            &JsValue::from_str("/anything"),
            &triggerAbortedSignal(),
        )
        .unwrap();
    assert!(JsFuture::from(Promise::resolve(&aborted)).await.is_err());
    let missing = call(&controller, "serializeReference")
        .call3(
            &controller,
            &JsValue::from_str("missing"),
            &JsValue::from_str("row"),
            &triggerSignal(),
        )
        .unwrap();
    assert!(JsFuture::from(Promise::resolve(&missing)).await.is_err());
}

#[wasm_bindgen_test]
fn menu_view_ready_pending_highlight_pick_and_dismiss_are_live() {
    let bench = makeTriggerBench();
    configure(&bench);
    let component = exported_menu_view_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let store = js_sys::Object::new();
    let state=js_sys::JSON::parse(r#"{"open":true,"hit":null,"generation":1,"groups":[{"source":"command","status":"ready","items":[{"name":"goal","description":"set goal","icon":"*"}]},{"source":"skill","status":"pending","items":[]}],"highlight":{"source":"command","index":0}}"#).unwrap();
    let get = Closure::wrap(Box::new(move || state.clone()) as Box<dyn FnMut() -> JsValue>);
    Reflect::set(
        &store,
        &JsValue::from_str("getSnapshot"),
        &get.into_js_value(),
    )
    .unwrap();
    let subscribe = Closure::wrap(
        Box::new(move |_listener: Function| Function::new_no_args(""))
            as Box<dyn FnMut(Function) -> Function>,
    );
    Reflect::set(
        &store,
        &JsValue::from_str("subscribe"),
        &subscribe.into_js_value(),
    )
    .unwrap();
    let picks = Array::new();
    let dismisses = Array::new();
    let props = triggerMenuProps(&store.into(), &picks, &dismisses);
    let tree = triggerMenuRender(&bench, &component, &props);
    let list = triggerListbox(&tree);
    assert_eq!(
        property(&list, "aria-activedescendant")
            .as_string()
            .as_deref(),
        Some("seekdeep-slash-option-command-0")
    );
    let text = triggerText(&tree);
    for expected in ["命令", "goal", "set goal", "技能", "正在加载"] {
        assert!(text.contains(expected), "missing {expected:?} in {text:?}");
    }
    let option = triggerOptions(&tree).get(0);
    assert!(triggerMouseDown(&option));
    assert_eq!(picks.length(), 1);
    triggerDispatchPointer(&triggerBody());
    assert_eq!(dismisses.length(), 1);
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
