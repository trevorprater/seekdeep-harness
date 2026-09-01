//! Live Cordis, Store, locale, Slot, and injected-action assembly parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_workspace::{
    apply_client_ui_workspace, configure_client_ui_workspace, configure_client_ui_workspace_apply,
    workspace_browser_component, workspace_inject, workspace_picker_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let cached
export function makeWorkspaceApplyBench(){
  if(cached){cached.reset();return cached}
  const styles=[],states=[],refs=[],effects=[];let si=0,ri=0,ei=0
  globalThis.document={head:{appendChild(node){styles.push(node);return node}},createElement(){return{attrs:{},setAttribute(key,value){this.attrs[key]=value},textContent:''}},querySelector(selector){const match=selector.match(/data-plugin-css="([^"]+)"/);return match?styles.find(style=>style.attrs['data-plugin-css']===match[1])??null:null},addEventListener(){},removeEventListener(){}}
  const Fragment=Symbol('Fragment')
  const flatten=values=>values.flat(Infinity).filter(value=>value!==null&&value!==undefined&&value!==false)
  const React={Fragment,createElement(kind,supplied,...children){const flat=flatten(children);const props={...(supplied??{})};if(flat.length===1)props.children=flat[0];else if(flat.length>1)props.children=flat;const node={kind,props,children:flat,focus(){},blur(){},contains(){return false},getBoundingClientRect(){return{top:100,height:34}}};if(props.ref&&typeof props.ref==='object')props.ref.current=node;return node},useCallback(value){return value},useEffect(run,deps){const at=ei++;const old=effects[at];const same=old&&old.deps.length===deps.length&&deps.every((value,index)=>Object.is(value,old.deps[index]));if(!same){old?.cleanup?.();const cleanup=run();effects[at]={deps:[...deps],cleanup:typeof cleanup==='function'?cleanup:undefined}}},useMemo(factory){return factory()},useRef(initial){const at=ri++;if(!(at in refs))refs[at]={current:initial};return refs[at]},useState(initial){const at=si++;if(!(at in states))states[at]=typeof initial==='function'?initial():initial;return[states[at],value=>{states[at]=typeof value==='function'?value(states[at]):value}]}}
  function resolve(value){if(Array.isArray(value))return flatten(value.map(resolve));if(value===null||value===undefined||value===false||typeof value!=='object')return value;if(!('kind'in value))return value;if(typeof value.kind==='function')return resolve(value.kind(value.props));if(value.kind===Fragment)return{kind:'Fragment',props:value.props,children:flatten(value.children.map(resolve))};return{...value,children:flatten(value.children.map(resolve))}}
  const primitives={};for(const name of ['Button','HoverCard','IconArchiveOutline20','IconBranchOutline16','IconCloseFill14','IconEditOutline16','IconEllipsisOutline16','IconFolderClose16','IconFolderOpen16','IconPersonalizationOutline16','IconPlusOutline16','IconProjectAddOutline16','IconSearchOutline16','IconTrashOutline16','IconTriangleRightFill14','Menu','Modal','StateDot','Tooltip'])primitives[name]=name
  const stores=[]
  function defineStore(declaration){const handle={declaration,create(){const state=declaration.init();const actions=Object.fromEntries(Object.entries(declaration.actions).map(([name,action])=>[name,(...args)=>action(state,...args)]));return{state,actions}}};stores.push(handle);return handle}
  cached={React,primitives,defineStore,styles,stores,render(component,props){si=0;ri=0;ei=0;return resolve(React.createElement(component,props))},reset(){for(const effect of effects.reverse())effect?.cleanup?.();styles.length=0;stores.length=0;states.length=0;refs.length=0;effects.length=0}}
  return cached
}
function makeSlots(){const declared=new Set(),rows=new Map(),waiters=new Map(),listeners=new Map();const notify=name=>{for(const listener of listeners.get(name)??[])listener()};const reconcile=name=>{for(const waiter of waiters.get(name)??[]){if(declared.has(name)&&!waiter.cleanup)waiter.cleanup=waiter.setup();else if(!declared.has(name)&&waiter.cleanup){waiter.cleanup();waiter.cleanup=undefined}}};const undeclare=name=>{if(!declared.delete(name))return;for(const row of [...(rows.get(name)??[])])row.dispose();rows.delete(name);reconcile(name);notify(name)};return{declare(name){declared.add(name);reconcile(name)},undeclare,register(options,component){if(!declared.has(options.name))throw new Error(`undeclared ${options.name}`);const row={options,component,disposed:false};const list=rows.get(options.name)??[];list.push(row);rows.set(options.name,list);for(const child of Object.keys(options.children??{})){declared.add(child);reconcile(child)}notify(options.name);const dispose=()=>{if(row.disposed)return;row.disposed=true;const index=list.indexOf(row);if(index>=0)list.splice(index,1);for(const child of Object.keys(options.children??{}))undeclare(child);notify(options.name)};row.dispose=dispose;return dispose},inject(name,setup){const waiter={setup,cleanup:undefined};const list=waiters.get(name)??[];list.push(waiter);waiters.set(name,list);reconcile(name);return()=>{waiter.cleanup?.();waiter.cleanup=undefined;const index=list.indexOf(waiter);if(index>=0)list.splice(index,1)}},entries(name){return[...(rows.get(name)??[])].filter(row=>!row.disposed)},subscribe(name,listener){const set=listeners.get(name)??new Set();set.add(listener);listeners.set(name,set);return()=>set.delete(listener)}}}
export function makeWorkspaceApplyFrame(bench){const calls=[],effects=[],slots=makeSlots(),localeCopies={};let searchFail=false;const renameCalls=[];const sessionSnapshot={ids:['session'],byId:{session:{id:'session',displayTitle:'Old title',running:false,blank:false,updatedAt:1}},current:'session',phase:'ready',subagentsByParent:{},jobsBySession:{}};const workspaceSnapshot={items:[{workspaceId:'workspace',path:'/projects/workspace',title:'workspace',sessionIds:['session'],createdAt:'2026-01-01T00:00:00.000Z',updatedAt:'2026-01-01T00:00:00.000Z'}],archivedSessionIds:[],state:'idle',phase:'ready',error:null};const sessions={searchResultLimit:20,open(id){calls.push(['open',id])},search(query,signal){calls.push(['search',query,signal]);return Promise.resolve(searchFail?{ok:false,error:{message:'index unavailable'}}:{ok:true,value:{items:[{sessionId:'session',snippet:'match'}],hasMore:false}})},binding(id){calls.push(['binding',id]);return id==='missing'?undefined:{session:{rename(title){renameCalls.push([id,title]);return Promise.resolve({ok:true,value:{title}})}}}},fork(request){calls.push(['fork',request.sessionId,request.increaseTitle]);return Promise.resolve('forked')}};const workspaces={startSession(id){calls.push(['startSession',id])},rename(id,title){calls.push(['renameWorkspace',id,title]);return Promise.resolve()},delete(id){calls.push(['deleteWorkspace',id]);return Promise.resolve()},insertBefore(id,before){calls.push(['insertWorkspaceBefore',id,before]);return Promise.resolve()},archiveSession(id){calls.push(['archiveSession',id]);return Promise.resolve()},insertSessionBefore(workspace,id,before){calls.push(['insertSessionBefore',workspace,id,before]);return Promise.resolve()},create(input){calls.push(['create',input.path]);return Promise.resolve({workspaceId:'created',path:input.path,title:'created',sessionIds:[],createdAt:'0',updatedAt:'0'})}};const locale={register(namespace,copies){localeCopies[namespace]=copies;calls.push(['locale',namespace]);return()=>{delete localeCopies[namespace]}}};const ctx={slots,sessions,workspaces,locale,effect(setup,label){calls.push(['effect',label]);const cleanup=setup();if(typeof cleanup==='function')effects.push(cleanup);return cleanup}};return{ctx,slots,calls,renameCalls,localeCopies,sessionSnapshot,workspaceSnapshot,setSearchFail(value){searchFail=value},declare(){slots.declare('sidebar.workspaces');slots.declare('conversation.hero.workspace')},dispose(){for(const cleanup of effects.reverse())cleanup();slots.undeclare('sidebar.workspaces');slots.undeclare('conversation.hero.workspace')}}}
export function aEntries(frame,name){return frame.slots.entries(name)}
export function aDeclare(frame){frame.declare()}
export function aUndeclare(frame,name){frame.slots.undeclare(name)}
export function aSetSearchFail(frame,value){frame.setSearchFail(value)}
export function aCalls(frame){return frame.calls}
export function aRenameCalls(frame){return frame.renameCalls}
export function aLocale(frame,namespace,language,key){return frame.localeCopies[namespace]?.[language]?.[key]}
export function aDispose(frame){frame.dispose()}
export function aRegisterOccupant(frame,name){return frame.slots.register({name},()=>null)}
export function aCreateStore(row){return row.options.store.create()}
const common={search:'Search',copy:'Copy',close:'Close',cancel:'Cancel'}
export function aAssembleBrowser(bench,frame){const row=frame.slots.entries('sidebar.workspaces')[0],face=row.options.inject(),store=row.options.store.create(),t=(key,vars={})=>Object.entries(vars).reduce((text,[name,value])=>text.replaceAll(`{${name}}`,String(value)),frame.localeCopies.workspace?.en?.[key]??common[key]??key);return{component:row.component,props:{wide:true,expandSidebar(){},useSessions:selector=>selector(frame.sessionSnapshot),useWorkspaces:selector=>selector(frame.workspaceSnapshot),useStore:selector=>selector(store.state),actions:store.actions,...face,useDirectoryFlow:selector=>selector(face.hooks.directoryFlow.getSnapshot()),renderSlot(){return null},t},store}}
export function aRender(bench,assembled){return bench.render(assembled.component,assembled.props)}
function walk(root,out=[],seen=new Set()){if(!root||typeof root!=='object'||seen.has(root))return out;seen.add(root);if(Array.isArray(root)){root.forEach(value=>walk(value,out,seen));return out}if('kind'in root)out.push(root);(root.children??[]).forEach(value=>walk(value,out,seen));for(const key of ['anchor','content','footer'])walk(root.props?.[key],out,seen);return out}
function textOf(root){const parts=[];const visit=value=>{if(typeof value==='string'||typeof value==='number')parts.push(String(value));else if(Array.isArray(value))value.forEach(visit);else if(value&&typeof value==='object')(value.children??[]).forEach(visit)};visit(root);return parts.join('')}
export function aFind(root,key,value){return walk(root).find(node=>Object.is(node.props?.[key],value))}
export function aFindKindText(root,kind,text){return walk(root).find(node=>node.kind===kind&&textOf(node)===text)}
export function aMenuByAria(root,label){return walk(root).find(node=>node.kind==='Menu'&&walk(node.props?.anchor).some(child=>child.props?.['aria-label']===label))}
export function aClick(node){return node.props.onClick?.({target:node,preventDefault(){},stopPropagation(){}})}
export function aSelect(menu,id){return menu.props.onSelect(id)}
export function aChange(node,value){return node.props.onChange?.({target:{value}})}
export function aUpdateTitle(frame,title){frame.sessionSnapshot={...frame.sessionSnapshot,byId:{...frame.sessionSnapshot.byId,session:{...frame.sessionSnapshot.byId.session,displayTitle:title}}}}
export function aText(root){return textOf(root)}
"#)]
extern "C" {
    fn makeWorkspaceApplyBench() -> JsValue;
    fn makeWorkspaceApplyFrame(bench: &JsValue) -> JsValue;
    fn aEntries(frame: &JsValue, name: &str) -> Array;
    fn aDeclare(frame: &JsValue);
    fn aUndeclare(frame: &JsValue, name: &str);
    fn aSetSearchFail(frame: &JsValue, value: bool);
    fn aCalls(frame: &JsValue) -> Array;
    fn aRenameCalls(frame: &JsValue) -> Array;
    fn aLocale(frame: &JsValue, namespace: &str, language: &str, key: &str) -> JsValue;
    fn aDispose(frame: &JsValue);
    fn aRegisterOccupant(frame: &JsValue, name: &str) -> Function;
    fn aCreateStore(row: &JsValue) -> JsValue;
    fn aAssembleBrowser(bench: &JsValue, frame: &JsValue) -> JsValue;
    fn aRender(bench: &JsValue, assembled: &JsValue) -> JsValue;
    fn aFind(root: &JsValue, key: &str, value: &JsValue) -> JsValue;
    fn aFindKindText(root: &JsValue, kind: &str, text: &str) -> JsValue;
    fn aMenuByAria(root: &JsValue, label: &str) -> JsValue;
    fn aClick(node: &JsValue) -> JsValue;
    fn aSelect(menu: &JsValue, id: &str) -> JsValue;
    fn aChange(node: &JsValue, value: &str) -> JsValue;
    fn aUpdateTitle(frame: &JsValue, title: &str);
    fn aText(root: &JsValue) -> String;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn call(value: &JsValue, name: &str, arguments: &[JsValue]) -> JsValue {
    let function = property(value, name).dyn_into::<Function>().unwrap();
    let values = Array::new();
    for argument in arguments {
        values.push(argument);
    }
    function.apply(value, &values).unwrap()
}

fn configure() -> JsValue {
    let bench = makeWorkspaceApplyBench();
    configure_client_ui_workspace(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    configure_client_ui_workspace_apply(property(&bench, "defineStore").dyn_into().unwrap());
    bench
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn apply_waits_for_declarations_routes_actions_and_owns_flow_lifetimes() {
    let bench = configure();
    let frame = makeWorkspaceApplyFrame(&bench);
    assert_eq!(
        workspace_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        vec!["slots", "sessions", "workspaces", "locale"]
    );
    apply_client_ui_workspace(property(&frame, "ctx")).unwrap();
    assert_eq!(aEntries(&frame, "sidebar.workspaces").length(), 0);
    aDeclare(&frame);
    let browser_rows = aEntries(&frame, "sidebar.workspaces");
    let picker_rows = aEntries(&frame, "conversation.hero.workspace");
    assert_eq!(browser_rows.length(), 1);
    assert_eq!(picker_rows.length(), 1);
    let browser_row = browser_rows.get(0);
    let picker_row = picker_rows.get(0);
    assert!(Object::is(
        &property(&browser_row, "component"),
        &workspace_browser_component().unwrap()
    ));
    assert!(Object::is(
        &property(&picker_row, "component"),
        &workspace_picker_component().unwrap()
    ));
    assert_eq!(
        property(&property(&browser_row, "options"), "locale")
            .as_string()
            .as_deref(),
        Some("workspace")
    );
    assert_eq!(
        aLocale(&frame, "workspace", "zh", "session.new")
            .as_string()
            .as_deref(),
        Some("新会话")
    );

    let browser_face = property(&property(&browser_row, "options"), "inject")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    call(
        &browser_face,
        "startSession",
        &[JsValue::from_str("workspace-id")],
    );
    call(&browser_face, "startSession", &[JsValue::UNDEFINED]);
    call(&browser_face, "open", &[JsValue::from_str("session")]);
    let signal = js_sys::Object::new();
    let search = call(
        &browser_face,
        "searchSessions",
        &[JsValue::from_str("match"), signal.clone().into()],
    );
    let search = JsFuture::from(search.dyn_into::<js_sys::Promise>().unwrap())
        .await
        .unwrap();
    assert_eq!(Array::from(&property(&search, "items")).length(), 1);
    assert_eq!(
        property(&browser_face, "searchResultLimit").as_f64(),
        Some(20.0)
    );
    let rename = call(
        &browser_face,
        "renameSession",
        &[JsValue::from_str("session"), JsValue::from_str("Renamed")],
    );
    JsFuture::from(rename.dyn_into::<js_sys::Promise>().unwrap())
        .await
        .unwrap();
    assert_eq!(aRenameCalls(&frame).length(), 1);
    let missing = call(
        &browser_face,
        "renameSession",
        &[JsValue::from_str("missing"), JsValue::from_str("Name")],
    );
    assert!(
        JsFuture::from(missing.dyn_into::<js_sys::Promise>().unwrap())
            .await
            .is_err()
    );
    call(
        &browser_face,
        "forkSession",
        &[JsValue::from_str("session")],
    );
    JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    assert!(aCalls(&frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("open")
            && call.get(1).as_string().as_deref() == Some("forked")
    }));
    for (name, arguments) in [
        (
            "renameWorkspace",
            vec![JsValue::from_str("w"), JsValue::from_str("Name")],
        ),
        ("deleteWorkspace", vec![JsValue::from_str("w")]),
        (
            "insertWorkspaceBefore",
            vec![JsValue::from_str("w"), JsValue::from_str("before")],
        ),
        ("archiveSession", vec![JsValue::from_str("session")]),
        (
            "insertSessionBefore",
            vec![
                JsValue::from_str("w"),
                JsValue::from_str("session"),
                JsValue::UNDEFINED,
            ],
        ),
        (
            "createWorkspace",
            vec![js_sys::JSON::parse(r#"{"path":"/tmp/project"}"#).unwrap()],
        ),
    ] {
        call(&browser_face, name, &arguments);
    }

    let hooks = property(&browser_face, "hooks");
    let source = property(&hooks, "directoryFlow");
    assert_eq!(call(&source, "getSnapshot", &[]), JsValue::FALSE);
    let notifications = Array::new();
    let captured = notifications.clone();
    let listener = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
        captured.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let unsubscribe = call(&source, "subscribe", &[listener.into_js_value()])
        .dyn_into::<Function>()
        .unwrap();
    let dispose_occupant = aRegisterOccupant(&frame, "sidebar.workspaces.directoryFlow");
    assert_eq!(call(&source, "getSnapshot", &[]), JsValue::TRUE);
    assert!(notifications.length() > 0);
    dispose_occupant.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(call(&source, "getSnapshot", &[]), JsValue::FALSE);
    unsubscribe.call0(&JsValue::UNDEFINED).unwrap();

    let picker_face = property(&property(&picker_row, "options"), "inject")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    call(
        &picker_face,
        "createWorkspace",
        &[js_sys::JSON::parse(r#"{"path":"/tmp/picker"}"#).unwrap()],
    );

    aSetSearchFail(&frame, true);
    let refused = call(
        &browser_face,
        "searchSessions",
        &[JsValue::from_str("fail"), Object::new().into()],
    );
    assert!(
        JsFuture::from(refused.dyn_into::<js_sys::Promise>().unwrap())
            .await
            .is_err()
    );

    let first_store = property(&browser_row, "options");
    let first_store = property(&first_store, "store");
    let instance = aCreateStore(&browser_row);
    call(
        &property(&instance, "actions"),
        "setGroupBy",
        &[JsValue::from_str("flat")],
    );
    assert_eq!(
        property(&property(&instance, "state"), "groupBy")
            .as_string()
            .as_deref(),
        Some("flat")
    );
    aUndeclare(&frame, "sidebar.workspaces");
    property(&frame, "slots").dyn_into::<Object>().ok();
    call(
        &property(&frame, "slots"),
        "declare",
        &[JsValue::from_str("sidebar.workspaces")],
    );
    let second_row = aEntries(&frame, "sidebar.workspaces").get(0);
    let second_store = property(&property(&second_row, "options"), "store");
    assert!(!Object::is(&first_store, &second_store));

    aDispose(&frame);
    assert_eq!(aEntries(&frame, "sidebar.workspaces").length(), 0);
    assert_eq!(aEntries(&frame, "conversation.hero.workspace").length(), 0);
}

#[wasm_bindgen_test(async)]
async fn assembled_row_rename_reaches_session_binding_and_relabels_on_list_echo() {
    let bench = configure();
    let frame = makeWorkspaceApplyFrame(&bench);
    apply_client_ui_workspace(property(&frame, "ctx")).unwrap();
    aDeclare(&frame);
    let assembled = aAssembleBrowser(&bench, &frame);
    let _ = aRender(&bench, &assembled);
    let tree = aRender(&bench, &assembled);
    let action = aFind(
        &tree,
        "aria-label",
        &JsValue::from_str("Session actions for Old title"),
    );
    assert!(action.is_object());
    let _ = aClick(&action);
    let menu_tree = aRender(&bench, &assembled);
    aSelect(
        &aMenuByAria(&menu_tree, "Session actions for Old title"),
        "rename",
    );
    let dialog = aRender(&bench, &assembled);
    let input = aFind(&dialog, "aria-label", &JsValue::from_str("Session name"));
    assert!(input.is_object());
    aChange(&input, "  New title  ");
    let valid = aRender(&bench, &assembled);
    let modal = aFind(&valid, "title", &JsValue::from_str("Rename session"));
    let confirm = aFindKindText(&modal, "Button", "Rename");
    let _ = aClick(&confirm);
    JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    assert!(aRenameCalls(&frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("session")
            && call.get(1).as_string().as_deref() == Some("New title")
    }));
    aUpdateTitle(&frame, "New title");
    let echoed = aRender(&bench, &assembled);
    assert!(
        aFind(
            &echoed,
            "aria-label",
            &JsValue::from_str("Session actions for New title")
        )
        .is_object()
    );
    assert!(
        aFind(
            &echoed,
            "aria-label",
            &JsValue::from_str("Session actions for Old title")
        )
        .is_undefined()
    );
    aDispose(&frame);
}
