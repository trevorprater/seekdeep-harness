//! Live React coverage for the compiled directory browser.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Promise, Reflect};
use seekdeep_client_ui_directory_picker_browse::{
    browse_directory_flow_component, configure_client_ui_directory_picker_browse,
    directory_browser_component,
};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten=values=>values.flat(Infinity).filter(value=>value!==null&&value!==undefined&&value!==false)
let cached
function engine(){const states=[],refs=[],effects=[],pending=[];let si=0,ri=0,ei=0,updateCount=0;const Fragment=Symbol('Fragment');const React={Fragment,createElement(kind,supplied,...children){const flat=flatten(children),props={...(supplied??{})};if(flat.length===1)props.children=flat[0];else if(flat.length>1)props.children=flat;const node={kind,props,children:flat,focused:false,focus(){this.focused=true;document.activeElement=this},blur(){this.focused=false;if(document.activeElement===this)document.activeElement=document.body},contains(target){return target===this||containsNode(this,target)},closest(){return this},querySelector(){return walkSelected(this)},scrollLeft:0,scrollWidth:100};if(props.ref&&typeof props.ref==='object')props.ref.current=node;return node},useState(initial){const at=si++;if(!(at in states))states[at]=typeof initial==='function'?initial():initial;return[states[at],value=>{updateCount+=1;states[at]=typeof value==='function'?value(states[at]):value}]},useRef(initial){const at=ri++;if(!(at in refs))refs[at]={current:initial};return refs[at]},useEffect(run,deps){const at=ei++,old=effects[at];const same=deps!==undefined&&old?.deps!==undefined&&old.deps.length===deps.length&&deps.every((value,index)=>Object.is(value,old.deps[index]));if(!same)pending.push({at,run,deps})}};function begin(component,props){si=0;ri=0;ei=0;pending.length=0;return resolve(React.createElement(component,props))}function resolve(value){if(Array.isArray(value))return flatten(value.map(resolve));if(value===null||value===undefined||value===false||typeof value!=='object')return value;if(!('kind'in value))return value;if(typeof value.kind==='function')return resolve(value.kind(value.props));if(value.kind==='Modal'&&!value.props.open)return null;value.children=flatten(value.children.map(resolve));return value}function containsNode(root,target){if(root===target)return true;if(!root||typeof root!=='object')return false;return(root.children??[]).some(child=>containsNode(child,target))}function findAutoFocus(root){if(!root||typeof root!=='object')return null;if(root.props?.autoFocus)return root;for(const child of root.children??[]){const found=findAutoFocus(child);if(found)return found}return null}function walkSelected(root){if(!root||typeof root!=='object')return null;if(root.props?.['aria-current']===true)return root;for(const child of root.children??[]){const found=walkSelected(child);if(found)return found}return null}return{React,render(component,props){const tree=begin(component,props);if(document.activeElement!==document.body&&!containsNode(tree,document.activeElement))document.activeElement=document.body;if(document.activeElement===document.body)findAutoFocus(tree)?.focus();for(const item of pending){effects[item.at]?.cleanup?.();const cleanup=item.run();effects[item.at]={deps:item.deps===undefined?undefined:[...item.deps],cleanup:typeof cleanup==='function'?cleanup:undefined}}return tree},renderWithoutCommit(component,props){return begin(component,props)},updates(){return updateCount},reset(){for(const effect of effects.reverse())effect?.cleanup?.();states.length=0;refs.length=0;effects.length=0;pending.length=0}}}
export function makeDirectoryBrowserBench(){if(cached){cached.reset();return cached}const e=engine(),styles=[];globalThis.document={body:{},activeElement:null,hasFocus(){return true},head:{appendChild(node){styles.push(node);return node}},createElement(){return{attrs:{},setAttribute(key,value){this.attrs[key]=value},textContent:''}},querySelector(){return null}};document.activeElement=document.body;const primitives={};for(const name of ['Button','IconCheckOutline16','IconChevronRightOutline14','IconEditOutline16','IconFolderClose16','IconFolderOpen16','IconPlusOutline16','Modal'])primitives[name]=name;const copy={'browser.title':'Select Workspace Directory','browser.home':'Home','browser.newFolder':'New folder','browser.folderName':'Folder name','browser.createIn':'New folder in "{name}"','browser.untitledFolder':'Untitled folder','browser.create':'Create','browser.cancel':'Cancel','browser.open':'Open','browser.editPath':'Edit path','browser.loading':'Loading…','browser.truncated':'Too many folders to list; only the beginning is shown.','browser.showHidden':'Show hidden files'};cached={...e,primitives,styles,t:(key,vars={})=>Object.entries(vars).reduce((text,[name,value])=>text.replaceAll(`{${name}}`,String(value)),copy[key]??key)};return cached}
const HOME='/home/u',DOCS='/home/u/Documents',HARNESS='/home/u/Documents/harness'
function listing(path){const asked=path??HOME,target=asked.length>1&&asked.endsWith('/')?asked.slice(0,-1):asked;const tree={ [HOME]:{path:HOME,home:HOME,crumbs:[{name:'/',path:'/',hidden:false},{name:'home',path:'/home',hidden:false},{name:'u',path:HOME,hidden:false}],entries:[{name:'.config',path:`${HOME}/.config`,hidden:true},{name:'Documents',path:DOCS,hidden:false}],truncated:false},[DOCS]:{path:DOCS,home:HOME,crumbs:[{name:'/',path:'/',hidden:false},{name:'home',path:'/home',hidden:false},{name:'u',path:HOME,hidden:false},{name:'Documents',path:DOCS,hidden:false}],entries:[{name:'harness',path:HARNESS,hidden:false}],truncated:false},[HARNESS]:{path:HARNESS,home:HOME,crumbs:[{name:'/',path:'/',hidden:false},{name:'home',path:'/home',hidden:false},{name:'u',path:HOME,hidden:false},{name:'Documents',path:DOCS,hidden:false},{name:'harness',path:HARNESS,hidden:false}],entries:[],truncated:false},[`${HOME}/.config`]:{path:`${HOME}/.config`,home:HOME,crumbs:[{name:'/',path:'/',hidden:false},{name:'home',path:'/home',hidden:false},{name:'u',path:HOME,hidden:false},{name:'.config',path:`${HOME}/.config`,hidden:true}],entries:[],truncated:false}};if(!(target in tree))return Promise.reject(new Error(`cannot list ${target}`));return Promise.resolve(structuredClone(tree[target]))}
export function makeDirectoryBrowserFrame(bench,open=true){const calls=[],created=new Map(),pending=[],pendingCreates=[];const frame={open,calls,listMode:'normal',createMode:'normal'};frame.props={open,listDirectory(path,signal){calls.push(['list',path,signal]);if(frame.listMode==='pending')return new Promise((resolve,reject)=>{const abort=()=>reject(new DOMException('aborted','AbortError'));signal?.addEventListener?.('abort',abort,{once:true});pending.push(()=>{signal?.removeEventListener?.('abort',abort);resolveListing(path).then(resolve,reject)})});return resolveListing(path)},createDirectory(path,name){calls.push(['create',path,name]);const child=`${path}/${name}`;created.set(child,path);if(frame.createMode==='pending')return new Promise(resolve=>pendingCreates.push(()=>resolve(child)));return Promise.resolve(child)},onOpen(path){calls.push(['open',path])},onClose(){calls.push(['close'])},onPicked(path){calls.push(['picked',path])},onCancel(){calls.push(['cancel'])},onError(error){calls.push(['error',error])},busy:false,t:bench.t};function resolveListing(path){const asked=path??HOME;if(created.has(asked))return Promise.resolve({path:asked,home:HOME,crumbs:[{name:'/',path:'/',hidden:false},{name:'home',path:'/home',hidden:false},{name:'u',path:HOME,hidden:false},{name:asked.slice(asked.lastIndexOf('/')+1),path:asked,hidden:false}],entries:[],truncated:false});return listing(path).then(level=>{const additions=[...created.entries()].filter(([,parent])=>parent===level.path).map(([child])=>({name:child.slice(child.lastIndexOf('/')+1),path:child,hidden:false}));return{...level,entries:[...level.entries,...additions]}})}frame.release=()=>pending.shift()?.();frame.releaseLast=()=>pending.pop()?.();frame.releaseCreate=()=>pendingCreates.shift()?.();return frame}
export function uiRender(bench,component,frame){frame.props.open=frame.open;return bench.render(component,frame.props)}
export function uiRenderWithoutCommit(bench,component,frame){frame.props.open=frame.open;return bench.renderWithoutCommit(component,frame.props)}
function walk(root,out=[],seen=new Set()){if(!root||typeof root!=='object'||seen.has(root))return out;seen.add(root);if(Array.isArray(root)){root.forEach(value=>walk(value,out,seen));return out}if('kind'in root)out.push(root);(root.children??[]).forEach(value=>walk(value,out,seen));for(const key of ['footer','icon'])walk(root.props?.[key],out,seen);return out}
function textOf(root){const parts=[];const visit=value=>{if(typeof value==='string'||typeof value==='number')parts.push(String(value));else if(Array.isArray(value))value.forEach(visit);else if(value&&typeof value==='object')(value.children??[]).forEach(visit)};visit(root);return parts.join('')}
export function uiFindKind(root,kind){return walk(root).find(node=>node.kind===kind)}
export function uiFindKinds(root,kind){return walk(root).filter(node=>node.kind===kind)}
export function uiFindButton(root,text){return walk(root).find(node=>(node.kind==='button'||node.kind==='Button')&&textOf(node)===text)}
export function uiFindAria(root,label){return walk(root).find(node=>node.props?.['aria-label']===label)}
export function uiFindText(root,text){return walk(root).find(node=>textOf(node)===text)}
export function uiFindClass(root,classPart){return walk(root).find(node=>String(node.props?.className??'').includes(classPart))}
export function uiProp(node,key){return node?.props?.[key]}
export function uiNodeProp(node,key){return node?.[key]}
export function uiClick(node){return node.props.onClick?.({target:node,preventDefault(){},stopPropagation(){}})}
export function uiChange(node,value){return node.props.onChange?.({target:{value}})}
export function uiKey(node,key){return node.props.onKeyDown?.({key,preventDefault(){},stopPropagation(){}})}
export function uiComposition(node,start){return node.props[start?'onCompositionStart':'onCompositionEnd']?.()}
export function uiBlur(node,relatedTarget=null){return node.props.onBlur?.({currentTarget:node,relatedTarget})}
export function uiClose(node){return node.props.onClose?.()}
export function uiCalls(frame){return frame.calls}
export function uiSetOpen(frame,value){frame.open=value}
export function uiSetBusy(frame,value){frame.props.busy=value}
export function uiSetListMode(frame,value){frame.listMode=value}
export function uiRelease(frame){frame.release()}
export function uiReleaseLast(frame){frame.releaseLast()}
export function uiSetCreateMode(frame,value){frame.createMode=value}
export function uiReleaseCreate(frame){frame.releaseCreate()}
export function uiUnmount(bench){bench.reset()}
export function uiUpdates(bench){return bench.updates()}
export function uiTick(ms=0){return new Promise(resolve=>setTimeout(resolve,ms))}
"#)]
extern "C" {
    fn makeDirectoryBrowserBench() -> JsValue;
    fn makeDirectoryBrowserFrame(bench: &JsValue, open: bool) -> JsValue;
    fn uiRender(bench: &JsValue, component: &JsValue, frame: &JsValue) -> JsValue;
    fn uiRenderWithoutCommit(bench: &JsValue, component: &JsValue, frame: &JsValue) -> JsValue;
    fn uiFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn uiFindKinds(root: &JsValue, kind: &str) -> Array;
    fn uiFindButton(root: &JsValue, text: &str) -> JsValue;
    fn uiFindAria(root: &JsValue, label: &str) -> JsValue;
    fn uiFindText(root: &JsValue, text: &str) -> JsValue;
    fn uiFindClass(root: &JsValue, class_part: &str) -> JsValue;
    fn uiProp(node: &JsValue, key: &str) -> JsValue;
    fn uiNodeProp(node: &JsValue, key: &str) -> JsValue;
    fn uiClick(node: &JsValue) -> JsValue;
    fn uiChange(node: &JsValue, value: &str) -> JsValue;
    fn uiKey(node: &JsValue, key: &str) -> JsValue;
    fn uiComposition(node: &JsValue, start: bool) -> JsValue;
    fn uiBlur(node: &JsValue, related_target: &JsValue) -> JsValue;
    fn uiClose(node: &JsValue) -> JsValue;
    fn uiCalls(frame: &JsValue) -> Array;
    fn uiSetOpen(frame: &JsValue, value: bool);
    fn uiSetBusy(frame: &JsValue, value: bool);
    fn uiSetListMode(frame: &JsValue, value: &str);
    fn uiRelease(frame: &JsValue);
    fn uiReleaseLast(frame: &JsValue);
    fn uiSetCreateMode(frame: &JsValue, value: &str);
    fn uiReleaseCreate(frame: &JsValue);
    fn uiUnmount(bench: &JsValue);
    fn uiUpdates(bench: &JsValue) -> u32;
    fn uiTick(ms: f64) -> Promise;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn configure() -> JsValue {
    let bench = makeDirectoryBrowserBench();
    configure_client_ui_directory_picker_browse(
        property(&bench, "React"),
        property(&bench, "primitives"),
    )
    .unwrap();
    bench
}

async fn settle() {
    for _ in 0..5 {
        JsFuture::from(uiTick(0.0)).await.unwrap();
    }
}

fn calls_named(frame: &JsValue, name: &str) -> Vec<Array> {
    uiCalls(frame)
        .iter()
        .map(|call| Array::from(&call))
        .filter(|call| call.get(0).as_string().as_deref() == Some(name))
        .collect()
}

fn signal_aborted(call: &Array) -> bool {
    property(&call.get(2), "aborted").as_bool() == Some(true)
}

#[wasm_bindgen_test(async)]
async fn open_home_hidden_selection_and_open_target_are_live() {
    let bench = configure();
    let closed = makeDirectoryBrowserFrame(&bench, false);
    let tree = uiRender(&bench, &directory_browser_component().unwrap(), &closed);
    assert!(tree.is_null());
    assert_eq!(uiCalls(&closed).length(), 0);

    let frame = makeDirectoryBrowserFrame(&bench, true);
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    settle().await;
    let home = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert_eq!(
        uiFindKinds(&home, "div")
            .iter()
            .filter(|node| uiProp(node, "role").as_string().as_deref() == Some("list"))
            .count(),
        1
    );
    assert!(uiFindButton(&home, "Documents").is_object());
    assert!(uiFindButton(&home, ".config").is_undefined());
    assert!(uiFindButton(&home, "Home").is_object());
    uiClick(&uiFindButton(&home, "Show hidden files"));
    let revealed = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindButton(&revealed, ".config").is_object());
    assert_eq!(
        uiProp(
            &uiFindButton(&revealed, "Show hidden files"),
            "aria-pressed"
        ),
        JsValue::TRUE
    );
    uiClick(&uiFindButton(&revealed, "Documents"));
    let selected = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert_eq!(
        uiProp(&uiFindButton(&selected, "Documents"), "aria-current"),
        JsValue::TRUE
    );
    settle().await;
    let two_pane = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindButton(&two_pane, "harness").is_object());
    uiClick(&uiFindButton(&two_pane, "Open"));
    assert!(uiCalls(&frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("open")
            && call.get(1).as_string().as_deref() == Some("/home/u/Documents")
    }));
}

#[wasm_bindgen_test(async)]
async fn browse_flow_adapts_pick_and_cancel_without_driving_owner_error() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    let component = browse_directory_flow_component().unwrap();
    let _ = uiRender(&bench, &component, &frame);
    settle().await;
    let home = uiRender(&bench, &component, &frame);
    uiClick(&uiFindButton(&home, "Open"));
    uiClick(&uiFindButton(&home, "Cancel"));
    let picks = calls_named(&frame, "picked");
    assert_eq!(picks.len(), 1);
    assert_eq!(picks[0].get(1).as_string().as_deref(), Some("/home/u"));
    assert_eq!(calls_named(&frame, "cancel").len(), 1);
    assert!(calls_named(&frame, "open").is_empty());
    assert!(calls_named(&frame, "close").is_empty());
    assert!(calls_named(&frame, "error").is_empty());
}

#[wasm_bindgen_test(async)]
async fn path_preview_and_nested_creation_drive_the_controller_effects() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    settle().await;
    let home = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClick(&uiFindAria(&home, "Edit path"));
    let editor = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let input = uiFindAria(&editor, "Edit path");
    assert_eq!(
        uiProp(&input, "value").as_string().as_deref(),
        Some("/home/u/")
    );
    uiChange(&input, "/home/u/Documents/har");
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    JsFuture::from(uiTick(300.0)).await.unwrap();
    settle().await;
    let preview = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindButton(&preview, "harness").is_object());

    uiKey(&uiFindClass(&preview, "editorScope"), "Escape");
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let crumb = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindAria(&crumb, "Edit path").is_object());
    uiClick(&uiFindButton(&crumb, "New folder"));
    let create = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let folder = uiFindAria(&create, "Folder name");
    uiChange(&folder, "fresh");
    let named = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClick(&uiFindButton(&named, "Create"));
    settle().await;
    let landed = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiCalls(&frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("create")
            && call.get(2).as_string().as_deref() == Some("fresh")
    }));
    assert!(uiFindText(&landed, "fresh").is_object());
}

#[wasm_bindgen_test(async)]
async fn slow_scan_silence_restarts_and_close_aborts_the_wire() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    uiSetListMode(&frame, "pending");
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    JsFuture::from(uiTick(200.0)).await.unwrap();
    let silent = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&silent, "Loading…").is_undefined());
    JsFuture::from(uiTick(150.0)).await.unwrap();
    let slow = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&slow, "Loading…").is_object());
    let first = calls_named(&frame, "list");
    assert_eq!(first.len(), 1);
    assert!(!signal_aborted(&first[0]));

    uiSetOpen(&frame, false);
    assert!(uiRender(&bench, &directory_browser_component().unwrap(), &frame).is_null());
    assert!(signal_aborted(&calls_named(&frame, "list")[0]));
    uiSetOpen(&frame, true);
    let fresh = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&fresh, "Loading…").is_undefined());
    assert_eq!(calls_named(&frame, "list").len(), 2);
    JsFuture::from(uiTick(200.0)).await.unwrap();
    let still_silent = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&still_silent, "Loading…").is_undefined());
    JsFuture::from(uiTick(150.0)).await.unwrap();
    let slow_again = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&slow_again, "Loading…").is_object());

    uiReleaseLast(&frame);
    settle().await;
    let landed = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&landed, "Loading…").is_undefined());
    assert!(uiFindButton(&landed, "Documents").is_object());
}

#[wasm_bindgen_test(async)]
async fn a_new_pick_aborts_the_old_scan_and_restarts_its_silence_window() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    settle().await;
    let home = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiSetListMode(&frame, "pending");
    uiClick(&uiFindButton(&home, "Documents"));
    let pending = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    JsFuture::from(uiTick(300.0)).await.unwrap();
    let slow = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&slow, "Loading…").is_object());
    uiClick(&uiFindButton(&pending, "Documents"));
    let restarted = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&restarted, "Loading…").is_undefined());
    let calls = calls_named(&frame, "list");
    assert_eq!(calls.len(), 3);
    assert!(signal_aborted(&calls[1]));
    assert!(!signal_aborted(&calls[2]));
    JsFuture::from(uiTick(200.0)).await.unwrap();
    let silent = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&silent, "Loading…").is_undefined());
    JsFuture::from(uiTick(150.0)).await.unwrap();
    let slow_again = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindText(&slow_again, "Loading…").is_object());
    uiSetOpen(&frame, false);
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(signal_aborted(&calls_named(&frame, "list")[2]));
}

#[wasm_bindgen_test(async)]
async fn ime_enter_is_ignored_for_both_path_and_folder_inputs() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    settle().await;
    let home = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClick(&uiFindAria(&home, "Edit path"));
    let editor = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let input = uiFindAria(&editor, "Edit path");
    uiChange(&input, "/home/u/Documents");
    let list_count = calls_named(&frame, "list").len();
    uiComposition(&input, true);
    uiKey(&input, "Enter");
    assert_eq!(calls_named(&frame, "list").len(), list_count);
    uiComposition(&input, false);
    uiKey(&input, "Enter");
    settle().await;
    let documents = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(calls_named(&frame, "list").len() > list_count);
    assert_eq!(
        uiNodeProp(&uiFindAria(&documents, "Edit path"), "focused"),
        JsValue::TRUE
    );

    uiClick(&uiFindButton(&documents, "New folder"));
    let create = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let folder = uiFindAria(&create, "Folder name");
    uiChange(&folder, "新建");
    uiComposition(&folder, true);
    uiKey(&folder, "Enter");
    assert!(calls_named(&frame, "create").is_empty());
    uiComposition(&folder, false);
    uiKey(&folder, "Enter");
    settle().await;
    let creates = calls_named(&frame, "create");
    assert_eq!(creates.len(), 1);
    assert_eq!(
        creates[0].get(1).as_string().as_deref(),
        Some("/home/u/Documents")
    );
    assert_eq!(creates[0].get(2).as_string().as_deref(), Some("新建"));
}

#[wasm_bindgen_test(async)]
async fn nested_dialog_and_busy_fences_refuse_parent_dismissal() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    settle().await;
    let home = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClick(&uiFindButton(&home, "New folder"));
    let nested = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let modals = uiFindKinds(&nested, "Modal");
    assert_eq!(modals.length(), 2);
    uiClose(&modals.get(1));
    let parent = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindAria(&parent, "Folder name").is_undefined());
    assert!(calls_named(&frame, "close").is_empty());
    uiClose(&uiFindKinds(&parent, "Modal").get(0));
    assert_eq!(calls_named(&frame, "close").len(), 1);

    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    uiSetCreateMode(&frame, "pending");
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    settle().await;
    let home = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClick(&uiFindButton(&home, "New folder"));
    let create = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiChange(&uiFindAria(&create, "Folder name"), "pending");
    let named = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClick(&uiFindButton(&named, "Create"));
    let creating = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let modals = uiFindKinds(&creating, "Modal");
    uiClose(&modals.get(1));
    uiClose(&modals.get(0));
    let fenced = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(uiFindAria(&fenced, "Folder name").is_object());
    assert!(calls_named(&frame, "close").is_empty());
    uiReleaseCreate(&frame);
    settle().await;

    uiSetBusy(&frame, true);
    let busy = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClose(&uiFindKinds(&busy, "Modal").get(0));
    assert!(calls_named(&frame, "close").is_empty());
    assert_eq!(
        uiProp(&uiFindButton(&busy, "Open"), "disabled"),
        JsValue::TRUE
    );
}

#[wasm_bindgen_test(async)]
async fn path_editor_focus_is_reparked_after_escape_unmounts_the_input() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    settle().await;
    let home = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClick(&uiFindAria(&home, "Edit path"));
    let editor = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let input = uiFindAria(&editor, "Edit path");
    assert_eq!(uiNodeProp(&input, "focused"), JsValue::TRUE);
    uiKey(&uiFindClass(&editor, "editorScope"), "Escape");
    let crumb = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let edit_zone = uiFindAria(&crumb, "Edit path");
    assert_eq!(uiNodeProp(&edit_zone, "focused"), JsValue::TRUE);
}

#[wasm_bindgen_test(async)]
async fn an_interrupted_render_does_not_consume_a_pending_focus_request() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    let component = directory_browser_component().unwrap();
    let _ = uiRender(&bench, &component, &frame);
    settle().await;
    let home = uiRender(&bench, &component, &frame);
    uiClick(&uiFindAria(&home, "Edit path"));
    let editor = uiRender(&bench, &component, &frame);
    let input = uiFindAria(&editor, "Edit path");
    uiChange(&input, "/home/u/Documents");
    uiKey(&input, "Enter");
    settle().await;

    let _ = uiRenderWithoutCommit(&bench, &component, &frame);
    let committed = uiRender(&bench, &component, &frame);
    assert_eq!(
        uiNodeProp(&uiFindAria(&committed, "Edit path"), "focused"),
        JsValue::TRUE
    );
}

#[wasm_bindgen_test(async)]
async fn editor_supersession_aborts_each_wire_scan_immediately() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    settle().await;
    let home = uiRender(&bench, &directory_browser_component().unwrap(), &frame);

    uiSetListMode(&frame, "pending");
    uiClick(&uiFindButton(&home, "Documents"));
    let selecting = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    let selection_call = calls_named(&frame, "list").pop().unwrap();
    assert!(!signal_aborted(&selection_call));
    uiClick(&uiFindAria(&selecting, "Edit path"));
    let editor = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(signal_aborted(&selection_call));

    let input = uiFindAria(&editor, "Edit path");
    uiChange(&input, "/home/u/Documents");
    uiKey(&input, "Enter");
    let submitted = calls_named(&frame, "list").pop().unwrap();
    assert!(!signal_aborted(&submitted));
    uiChange(&input, "/home/u/Documents/har");
    let changed = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(signal_aborted(&submitted));

    let changed_input = uiFindAria(&changed, "Edit path");
    uiKey(&changed_input, "Enter");
    let resubmitted = calls_named(&frame, "list").pop().unwrap();
    assert!(!signal_aborted(&resubmitted));
    uiKey(&uiFindClass(&changed, "editorScope"), "Escape");
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    assert!(signal_aborted(&resubmitted));
}

#[wasm_bindgen_test(async)]
async fn unmount_fences_a_pending_creation_before_it_can_relist() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    uiSetCreateMode(&frame, "pending");
    let _ = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    settle().await;
    let home = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClick(&uiFindButton(&home, "New folder"));
    let create = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiChange(&uiFindAria(&create, "Folder name"), "slow");
    let named = uiRender(&bench, &directory_browser_component().unwrap(), &frame);
    uiClick(&uiFindButton(&named, "Create"));
    let list_count = calls_named(&frame, "list").len();
    uiUnmount(&bench);
    let update_count = uiUpdates(&bench);
    uiReleaseCreate(&frame);
    settle().await;
    assert_eq!(calls_named(&frame, "list").len(), list_count);
    assert_eq!(uiUpdates(&bench), update_count);
}

#[wasm_bindgen_test(async)]
async fn guarded_no_ops_do_not_schedule_redundant_renders() {
    let bench = configure();
    let frame = makeDirectoryBrowserFrame(&bench, true);
    let component = directory_browser_component().unwrap();
    let _ = uiRender(&bench, &component, &frame);
    settle().await;
    let home = uiRender(&bench, &component, &frame);
    uiClick(&uiFindAria(&home, "Edit path"));
    let editor = uiRender(&bench, &component, &frame);
    uiChange(&uiFindAria(&editor, "Edit path"), "   ");
    let blank = uiRender(&bench, &component, &frame);
    let before_blank_enter = uiUpdates(&bench);
    uiKey(&uiFindAria(&blank, "Edit path"), "Enter");
    assert_eq!(uiUpdates(&bench), before_blank_enter);

    uiKey(&uiFindClass(&blank, "editorScope"), "Escape");
    let crumb = uiRender(&bench, &component, &frame);
    uiClick(&uiFindButton(&crumb, "New folder"));
    let create = uiRender(&bench, &component, &frame);
    uiChange(&uiFindAria(&create, "Folder name"), "   ");
    let whitespace = uiRender(&bench, &component, &frame);
    let before_folder_enter = uiUpdates(&bench);
    uiKey(&uiFindAria(&whitespace, "Folder name"), "Enter");
    assert_eq!(uiUpdates(&bench), before_folder_enter);

    uiChange(&uiFindAria(&whitespace, "Folder name"), "pending");
    let named = uiRender(&bench, &component, &frame);
    uiSetCreateMode(&frame, "pending");
    uiClick(&uiFindButton(&named, "Create"));
    let creating = uiRender(&bench, &component, &frame);
    let before_blocked_close = uiUpdates(&bench);
    uiClose(&uiFindKinds(&creating, "Modal").get(1));
    assert_eq!(uiUpdates(&bench), before_blocked_close);
    uiReleaseCreate(&frame);
    settle().await;
}
