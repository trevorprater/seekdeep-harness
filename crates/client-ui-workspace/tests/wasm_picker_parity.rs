//! Live React parity for Workspace picker and directory-flow core.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_workspace::{
    configure_client_ui_workspace, workspace_pick_flow_component, workspace_picker_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten=values=>values.flat(Infinity).filter(value=>value!==null&&value!==undefined&&value!==false)
let cached
export function makeWorkspacePickerBench(){
  if(cached){cached.reset();return cached}
  const states=[],effects=[],memos=[],styles=[];let si=0,ei=0,mi=0
  const Fragment=Symbol('Fragment')
  const React={Fragment,createElement(kind,supplied,...children){const flat=flatten(children);const props={...(supplied??{})};if(flat.length===1)props.children=flat[0];else if(flat.length>1)props.children=flat;return{kind,props,children:flat}},useState(initial){const at=si++;if(!(at in states))states[at]=typeof initial==='function'?initial():initial;return[states[at],value=>{states[at]=typeof value==='function'?value(states[at]):value}]},useEffect(run,deps){const at=ei++;const old=effects[at];const same=old&&old.deps.length===deps.length&&deps.every((v,i)=>Object.is(v,old.deps[i]));if(!same){old?.cleanup?.();const cleanup=run();effects[at]={deps:[...deps],cleanup:typeof cleanup==='function'?cleanup:undefined}}},useCallback(callback,deps){const at=mi++;const old=memos[at];const same=old&&old.deps.length===deps.length&&deps.every((v,i)=>Object.is(v,old.deps[i]));if(!same)memos[at]={deps:[...deps],value:callback};return memos[at].value}}
  function resolve(value){if(Array.isArray(value))return flatten(value.map(resolve));if(value===null||value===undefined||value===false||typeof value!=='object')return value;if(!('kind'in value))return value;if(typeof value.kind==='function')return resolve(value.kind(value.props));if(value.kind===Fragment)return{kind:'Fragment',props:value.props,children:flatten(value.children.map(resolve))};return{...value,children:flatten(value.children.map(resolve))}}
  const primitive=name=>props=>React.createElement(name,props,props.children)
  const copy={'menu.addWorkspace':'Add workspace…','picker.loading':'Loading workspaces…','folderError.title':'Couldn’t open folder','folderError.retry':'Choose again',close:'Close',cancel:'Cancel'}
  globalThis.document={head:{appendChild(node){styles.push(node);return node}},createElement(){return{setAttribute(){},textContent:''}},querySelector(){return null}}
  cached={React,primitives:{Button:primitive('Button'),IconFolderClose16:primitive('IconFolderClose16'),IconPlusOutline16:primitive('IconPlusOutline16'),Menu:primitive('Menu'),Modal:primitive('Modal')},t:key=>copy[key]??key,render(component,props){si=0;ei=0;mi=0;return resolve(React.createElement(component,props))},reset(){for(const effect of effects.reverse())effect?.cleanup?.();states.length=0;effects.length=0;memos.length=0}}
  return cached
}
export function makePickerProps(bench,state){const calls=[],flowOwners=[];let occupied=true,fail='';const props={t:bench.t,open:true,anchorRef:{current:{getBoundingClientRect(){return{x:1,y:2,width:3,height:4}}}},useWorkspaces:selector=>selector(state),createWorkspace({path}){calls.push(['createWorkspace',path]);return fail?Promise.reject(new Error(fail)):Promise.resolve({workspaceId:`workspace:${path}`,path,title:path})},useDirectoryFlow:selector=>selector(occupied),renderDirectoryFlow(owner){flowOwners.push(owner);return'flow'},onPick:id=>calls.push(['pick',id]),onClose:()=>calls.push(['close']),selectedId:'w1'};return{props,calls,flowOwners,setOccupied(value){occupied=value},setFailure(value){fail=value}}}
export function pickerRender(bench,component,props){return bench.render(component,props)}
function walk(root,out=[]){if(!root||typeof root!=='object')return out;if(Array.isArray(root)){root.forEach(value=>walk(value,out));return out}if('kind'in root)out.push(root);(root.children??[]).forEach(value=>walk(value,out));return out}
export function pickerFindKind(root,kind){return walk(root).find(node=>node.kind===kind)}
export function pickerFindText(root,text){return walk(root).find(node=>{const parts=[];const visit=value=>{if(typeof value==='string'||typeof value==='number')parts.push(String(value));else if(Array.isArray(value))value.forEach(visit);else if(value&&typeof value==='object')(value.children??[]).forEach(visit)};visit(node);return parts.join('')===text})}
export function pickerProp(node,key){return node?.props?.[key]}
export function pickerSelect(menu,id){return menu.props.onSelect(id)}
export function pickerClick(node){return node.props.onClick?.()}
export function pickerTick(){return new Promise(resolve=>setTimeout(resolve,0))}
export function pickerCalls(frame){return frame.calls}
export function pickerOwners(frame){return frame.flowOwners}
export function pickerOccupied(frame,value){frame.setOccupied(value)}
export function pickerFailure(frame,value){frame.setFailure(value)}
export function pickerWrapperProps(frame){return{...frame.props,renderSlot(name,owner){frame.calls.push(['renderSlot',name]);frame.flowOwners.push(owner);return'wrapped-flow'}}}
"#)]
extern "C" {
    fn makeWorkspacePickerBench() -> JsValue;
    fn makePickerProps(bench: &JsValue, state: &JsValue) -> JsValue;
    fn pickerRender(bench: &JsValue, component: &JsValue, props: &JsValue) -> JsValue;
    fn pickerFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn pickerFindText(root: &JsValue, text: &str) -> JsValue;
    fn pickerProp(node: &JsValue, key: &str) -> JsValue;
    fn pickerSelect(menu: &JsValue, id: &str) -> JsValue;
    fn pickerClick(node: &JsValue) -> JsValue;
    fn pickerTick() -> Promise;
    fn pickerCalls(frame: &JsValue) -> Array;
    fn pickerOwners(frame: &JsValue) -> Array;
    fn pickerOccupied(frame: &JsValue, value: bool);
    fn pickerFailure(frame: &JsValue, value: &str);
    fn pickerWrapperProps(frame: &JsValue) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn configure() -> JsValue {
    let bench = makeWorkspacePickerBench();
    configure_client_ui_workspace(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    bench
}

async fn settle() {
    for _ in 0..4 {
        JsFuture::from(pickerTick()).await.unwrap();
    }
}

#[wasm_bindgen_test]
fn existing_workspaces_use_scroll_items_pinned_add_footer_and_selection() {
    let bench = configure();
    let state = js_sys::JSON::parse(
        r#"{"phase":"ready","items":[{"workspaceId":"w1","title":"One"},{"workspaceId":"w2","title":"Two"}]}"#,
    )
    .unwrap();
    let frame = makePickerProps(&bench, &state);
    let tree = pickerRender(
        &bench,
        &workspace_pick_flow_component().unwrap(),
        &property(&frame, "props"),
    );
    let menu = pickerFindKind(&tree, "Menu");
    assert_eq!(pickerProp(&menu, "open"), JsValue::TRUE);
    assert_eq!(Array::from(&pickerProp(&menu, "items")).length(), 2);
    assert_eq!(Array::from(&pickerProp(&menu, "footer")).length(), 1);
    assert_eq!(
        pickerProp(&menu, "selectedId").as_string().as_deref(),
        Some("w1")
    );
    assert!(!property(&pickerProp(&menu, "getAnchorRect"), "name").is_undefined());
    pickerSelect(&menu, "w2");
    assert!(pickerCalls(&frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("pick")
            && call.get(1).as_string().as_deref() == Some("w2")
    }));
    pickerSelect(&menu, "::add-workspace");
    let opened = pickerRender(
        &bench,
        &workspace_pick_flow_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(property(&pickerOwners(&frame).pop(), "open"), JsValue::TRUE);
    assert!(
        pickerCalls(&frame)
            .iter()
            .any(|call| Array::from(&call).get(0).as_string().as_deref() == Some("close"))
    );
    assert_eq!(
        pickerProp(&pickerFindKind(&opened, "Menu"), "open"),
        JsValue::TRUE
    );
    pickerOccupied(&frame, false);
    let _ = pickerRender(
        &bench,
        &workspace_pick_flow_component().unwrap(),
        &property(&frame, "props"),
    );
    let _ = pickerRender(
        &bench,
        &workspace_pick_flow_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(
        property(&pickerOwners(&frame).pop(), "open"),
        JsValue::FALSE
    );
}

#[wasm_bindgen_test(async)]
async fn sole_add_auto_opens_adopts_and_surfaces_retryable_failure() {
    let bench = configure();
    let state = js_sys::JSON::parse(r#"{"phase":"ready","items":[]}"#).unwrap();
    let frame = makePickerProps(&bench, &state);
    let props = property(&frame, "props");
    let _ = pickerRender(&bench, &workspace_pick_flow_component().unwrap(), &props);
    let tree = pickerRender(&bench, &workspace_pick_flow_component().unwrap(), &props);
    assert_eq!(
        pickerProp(&pickerFindKind(&tree, "Menu"), "open"),
        JsValue::FALSE
    );
    let owner = pickerOwners(&frame).pop();
    assert_eq!(property(&owner, "open"), JsValue::TRUE);
    property(&owner, "onPicked")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("/tmp/project"))
        .unwrap();
    settle().await;
    assert!(pickerCalls(&frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("pick")
            && call.get(1).as_string().as_deref() == Some("workspace:/tmp/project")
    }));

    pickerFailure(&frame, "permission denied");
    let _ = pickerRender(&bench, &workspace_pick_flow_component().unwrap(), &props);
    let owner = pickerOwners(&frame).pop();
    property(&owner, "onPicked")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("/root/private"))
        .unwrap();
    settle().await;
    let failed = pickerRender(&bench, &workspace_pick_flow_component().unwrap(), &props);
    let modal = pickerFindKind(&failed, "Modal");
    assert_eq!(pickerProp(&modal, "open"), JsValue::TRUE);
    assert!(pickerFindText(&failed, "permission denied").is_object());
    pickerOccupied(&frame, false);
    let no_flow = pickerRender(&bench, &workspace_pick_flow_component().unwrap(), &props);
    let retry = pickerFindText(
        &pickerProp(&pickerFindKind(&no_flow, "Modal"), "footer"),
        "Choose again",
    );
    assert_eq!(pickerProp(&retry, "disabled"), JsValue::TRUE);
}

#[wasm_bindgen_test]
fn pending_and_wrapper_contracts_preserve_loading_and_directory_slot_name() {
    let bench = configure();
    let pending = js_sys::JSON::parse(r#"{"phase":"pending","items":[]}"#).unwrap();
    let frame = makePickerProps(&bench, &pending);
    let tree = pickerRender(
        &bench,
        &workspace_pick_flow_component().unwrap(),
        &property(&frame, "props"),
    );
    assert!(pickerFindText(&tree, "Loading workspaces…").is_object());
    let wrapped = pickerWrapperProps(&frame);
    let _ = pickerRender(&bench, &workspace_picker_component().unwrap(), &wrapped);
    assert!(pickerCalls(&frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("renderSlot")
            && call.get(1).as_string().as_deref()
                == Some("conversation.hero.workspace.directoryFlow")
    }));
    pickerOccupied(&frame, false);
    let empty = pickerRender(
        &bench,
        &workspace_pick_flow_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(
        pickerProp(&pickerFindKind(&empty, "Menu"), "open"),
        JsValue::FALSE
    );
}
