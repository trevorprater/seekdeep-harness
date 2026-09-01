//! Live assembled React parity for the compiled Workspace browser.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Promise, Reflect};
use seekdeep_client_ui_workspace::{configure_client_ui_workspace, workspace_browser_component};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten=values=>values.flat(Infinity).filter(value=>value!==null&&value!==undefined&&value!==false)
let cached
function makeEngine(){
  const states=[],refs=[],effects=[];let si=0,ri=0,ei=0
  const Fragment=Symbol('Fragment')
  const React={
    Fragment,
    createElement(kind,supplied,...children){const flat=flatten(children);const props={...(supplied??{})};if(flat.length===1)props.children=flat[0];else if(flat.length>1)props.children=flat;const node={kind,props,children:flat,focused:false,blurred:false,rect:{top:100,height:34},focus(){this.focused=true},blur(){this.blurred=true},contains(target){return target===this},getBoundingClientRect(){return this.rect}};if(props.ref&&typeof props.ref==='object')props.ref.current=node;else if(typeof props.ref==='function')props.ref(node);return node},
    useState(initial){const at=si++;if(!(at in states))states[at]=typeof initial==='function'?initial():initial;return[states[at],value=>{states[at]=typeof value==='function'?value(states[at]):value}]},
    useRef(initial){const at=ri++;if(!(at in refs))refs[at]={current:initial};return refs[at]},
    useEffect(run,deps){const at=ei++;const old=effects[at];const same=old&&old.deps.length===deps.length&&deps.every((v,i)=>Object.is(v,old.deps[i]));if(!same){old?.cleanup?.();const cleanup=run();effects[at]={deps:[...deps],cleanup:typeof cleanup==='function'?cleanup:undefined}}},
    useCallback(callback){return callback},useMemo(factory){return factory()},
  }
  function resolve(value){if(Array.isArray(value))return flatten(value.map(resolve));if(value===null||value===undefined||value===false||typeof value!=='object')return value;if(!('kind'in value))return value;if(typeof value.kind==='function')return resolve(value.kind(value.props));if(value.kind===Fragment)return{kind:'Fragment',props:value.props,children:flatten(value.children.map(resolve))};return{...value,children:flatten(value.children.map(resolve))}}
  return{React,render(component,props){si=0;ri=0;ei=0;return resolve(React.createElement(component,props))},reset(){for(const effect of effects.reverse())effect?.cleanup?.();states.length=0;refs.length=0;effects.length=0}}
}
export function makeWorkspaceBrowserBench(){
  if(cached){cached.reset();return cached}
  const engine=makeEngine(),styles=[],listeners=new Map()
  globalThis.document={body:{},head:{appendChild(node){styles.push(node);return node}},createElement(kind){return{kind,attrs:{},setAttribute(k,v){this.attrs[k]=v},textContent:''}},querySelector(selector){const match=selector.match(/data-plugin-css="([^"]+)"/);return match?styles.find(style=>style.attrs['data-plugin-css']===match[1])??null:null},addEventListener(name,fn){const set=listeners.get(name)??new Set();set.add(fn);listeners.set(name,set)},removeEventListener(name,fn){listeners.get(name)?.delete(fn)}}
  const primitives={};for(const name of ['Button','HoverCard','IconArchiveOutline20','IconBranchOutline16','IconCloseFill14','IconEditOutline16','IconEllipsisOutline16','IconFolderClose16','IconFolderOpen16','IconPersonalizationOutline16','IconPlusOutline16','IconProjectAddOutline16','IconSearchOutline16','IconTrashOutline16','IconTriangleRightFill14','Menu','Modal','StateDot','Tooltip'])primitives[name]=name
  const copy={
    'group.ungrouped':'Ungrouped','session.new':'New Session','section.workspaces':'Workspaces','section.sessions':'Sessions','viewOptions.label':'View options','groupBy.label':'Group by','groupBy.workspace':'WorkSpace','groupBy.flat':'In one list','orderBy.label':'Order by','orderBy.manual':'Manual','orderBy.updated':'Last updated','sessions.expand':'Show {n} more sessions','sessions.collapse':'Show less','empty.none':'No sessions yet','workspace.add':'Add workspace','search':'Search','search.sessions.aria':'Search sessions','search.placeholder':'Search sessions...','search.clear':'Clear search','search.results.aria':'Search results','search.pending':'Searching session history…','search.unavailable':'Content search is temporarily unavailable. Showing name matches.','search.noMatches':'No matching sessions','search.hasMore':'Showing the first {n} results. Narrow your search.','menu.addWorkspace':'Add workspace…','picker.loading':'Loading workspaces…','folderError.title':'Couldn’t open folder','folderError.retry':'Choose again','rename':'Rename','rename.workspace.title':'Rename workspace','rename.session.title':'Rename session','field.workspaceName':'Workspace name','field.sessionName':'Session name','delete.workspace':'Delete workspace','delete.desc':'This removes “{name}” from the workspace list. The folder and session logs will be kept. Its sessions will appear under Ungrouped.','delete.pending':'Deleting workspace…','menu.fork':'Fork session','menu.archiveSession':'Archive session','actions.workspace.aria':'Workspace actions for {name}','actions.session.aria':'Session actions for {name}','actions.newSession.aria':'New session in {name}','status.running':'Running','status.subagentsRunning.one':'{n} subagent running','status.subagentsRunning.other':'{n} subagents running','status.idle':'Idle','status.waitingApproval':'Waiting for approval','status.planReview':'Plan awaiting review','status.waitingAnswer':'Waiting for answer','status.completed':'Completed','hover.created':'Created {time}','hover.copied':'Copied','date.ymd':'{y}-{m}-{d}','time.now':'now','time.minutes':'{n}min','time.hours':'{n}h','time.days':'{n}d','time.months':'{n}mo','time.years':'{n}y','time.ago':'{t} ago',copy:'Copy',close:'Close',cancel:'Cancel','conflict.named':'A workspace named “{name}” already exists.',
  }
  const t=(key,vars={})=>Object.entries(vars).reduce((text,[name,value])=>text.replaceAll(`{${name}}`,String(value)),copy[key]??key)
  cached={...engine,primitives,styles,listeners,t}
  return cached
}
const session=(id,updatedAt)=>({id,displayTitle:id,running:false,blank:false,updatedAt})
const workspace=(id,ids,title=id)=>({workspaceId:id,path:`/projects/${id}`,title,sessionIds:ids,createdAt:'2026-01-01T00:00:00.000Z',updatedAt:'2026-01-01T00:00:00.000Z'})
export function makeBrowserFrame(bench){
  const calls=[]
  const frame={
    sessions:{ids:['alpha-s','beta-s'],byId:{'alpha-s':session('alpha-s',2),'beta-s':session('beta-s',1)},phase:'ready',current:undefined,subagentsByParent:{},jobsBySession:{}},
    workspaces:{items:[workspace('alpha',['alpha-s']),workspace('beta',['beta-s'])],archivedSessionIds:[],state:'idle',phase:'ready',error:null,baselinesReady:true,recentWorkspaceId:'alpha'},
    store:{groupBy:'workspace',orderBy:'updated',groupExpansion:{},sessionOrderByAccount:{},sessionUpdatedAtByAccount:{}},calls,directory:true,searchMode:'ok',renameMode:'ok',sessionRenameMode:'ok',deleteMode:'ok',
  }
  const actions={setGroupBy(value){calls.push(['setGroupBy',value]);frame.store.groupBy=value},setOrderBy(value){calls.push(['setOrderBy',value]);frame.store.orderBy=value},setGroupExpanded(key,value){calls.push(['setGroupExpanded',key,value]);frame.store.groupExpansion={...frame.store.groupExpansion,[key]:value}},retainAccountKeys(keys){calls.push(['retainAccountKeys',...keys]);const keep=new Set(keys);for(const map of ['groupExpansion','sessionOrderByAccount','sessionUpdatedAtByAccount'])for(const key of Object.keys(frame.store[map]))if(!keep.has(key))delete frame.store[map][key]},syncSessionOrderAccount(key,order,updated){calls.push(['syncSessionOrderAccount',key,[...order],{...updated}]);frame.store.sessionOrderByAccount={...frame.store.sessionOrderByAccount,[key]:[...order]};frame.store.sessionUpdatedAtByAccount={...frame.store.sessionUpdatedAtByAccount,[key]:{...updated}}},setSessionOrder(key,order){calls.push(['setSessionOrder',key,[...order]]);frame.store.sessionOrderByAccount={...frame.store.sessionOrderByAccount,[key]:[...order]}}}
  frame.props={wide:true,expandSidebar(){calls.push(['expandSidebar'])},useSessions:selector=>selector(frame.sessions),useWorkspaces:selector=>selector(frame.workspaces),useStore:selector=>selector(frame.store),actions,startSession:id=>calls.push(['startSession',id]),open:id=>calls.push(['open',id]),searchSessions(query,signal){calls.push(['search',query,signal]);if(frame.searchMode==='fail')return Promise.reject(new Error('search failed'));return Promise.resolve({items:[{sessionId:'beta-s',snippet:'content match'}],hasMore:true})},searchResultLimit:20,renameSession:(id,title)=>{calls.push(['renameSession',id,title]);if(frame.sessionRenameMode==='fail')return Promise.reject(new Error('session rename failed'));return Promise.resolve()},forkSession:id=>calls.push(['forkSession',id]),renameWorkspace:(id,title)=>{calls.push(['renameWorkspace',id,title]);if(frame.renameMode==='fail')return Promise.reject(new Error('rename failed'));if(frame.renameMode==='text')return Promise.reject('rename denied');return Promise.resolve()},deleteWorkspace:id=>{calls.push(['deleteWorkspace',id]);if(frame.deleteMode==='fail')return Promise.reject(new Error('delete failed'));if(frame.deleteMode==='text')return Promise.reject('delete denied');return Promise.resolve()},archiveSession:id=>{calls.push(['archiveSession',id]);return Promise.resolve()},insertWorkspaceBefore:(id,before)=>{calls.push(['insertWorkspaceBefore',id,before]);return Promise.resolve()},insertSessionBefore:(workspaceId,id,before)=>{calls.push(['insertSessionBefore',workspaceId,id,before]);return Promise.resolve()},createWorkspace:({path})=>{calls.push(['createWorkspace',path]);return Promise.resolve(workspace(`created:${path}`,[]))},useDirectoryFlow:selector=>selector(frame.directory),renderSlot(name,owner){calls.push(['renderSlot',name,owner.open]);return owner.open?'directory-flow':null},t:bench.t}
  return frame
}
export function bRender(bench,component,props){return bench.render(component,props)}
function walk(root,out=[],seen=new Set()){if(!root||typeof root!=='object'||seen.has(root))return out;seen.add(root);if(Array.isArray(root)){root.forEach(value=>walk(value,out,seen));return out}if('kind'in root)out.push(root);(root.children??[]).forEach(value=>walk(value,out,seen));for(const key of ['anchor','content','footer'])walk(root.props?.[key],out,seen);return out}
function textOf(root){const parts=[],seen=new Set();const visit=value=>{if(typeof value==='string'||typeof value==='number')parts.push(String(value));else if(Array.isArray(value))value.forEach(visit);else if(value&&typeof value==='object'&&!seen.has(value)){seen.add(value);(value.children??[]).forEach(visit)}};visit(root);return parts.join('')}
export function bFind(root,key,value){return walk(root).find(node=>value===undefined?key in node.props:Object.is(node.props?.[key],value))}
export function bFindKind(root,kind){return walk(root).find(node=>node.kind===kind)}
export function bFindKinds(root,kind){return walk(root).filter(node=>node.kind===kind)}
export function bFindText(root,text){return walk(root).find(node=>textOf(node)===text)}
export function bFindRoleText(root,role,text){return walk(root).find(node=>node.props?.role===role&&textOf(node).includes(text))}
export function bFindKindText(root,kind,text){return walk(root).find(node=>node.kind===kind&&textOf(node)===text)}
export function bMenuByAnchorAria(root,label){return walk(root).find(node=>node.kind==='Menu'&&walk(node.props?.anchor).some(child=>child.props?.['aria-label']===label))}
export function bModalByTitle(root,title){return walk(root).find(node=>node.kind==='Modal'&&node.props?.title===title)}
export function bText(root){return textOf(root)}
export function bSummary(root){return walk(root).map(node=>`${String(node.kind)}|${node.props?.className??''}|${textOf(node)}`).join('\n')}
export function bProp(node,key){return node?.props?.[key]}
export function bClick(node){const event={target:node,stopped:false,prevented:false,stopPropagation(){this.stopped=true},preventDefault(){this.prevented=true}};node.props.onClick?.(event);return event}
export function bSelect(menu,id){return menu.props.onSelect(id)}
export function bChange(node,value){return node.props.onChange?.({target:{value}})}
export function bKey(node,key){const event={key,prevented:false,preventDefault(){this.prevented=true}};node.props.onKeyDown?.(event);return event}
export function bCalls(frame){return frame.calls}
export function bStore(frame){return frame.store}
export function bSetWide(frame,value){frame.props.wide=value}
export function bSetSearchMode(frame,value){frame.searchMode=value}
export function bSetDirectory(frame,value){frame.directory=value}
export function bSetMode(frame,key,value){frame[key]=value}
export function bSetExpanded(frame,key,value){frame.store.groupExpansion={...frame.store.groupExpansion,[key]:value}}
export function bRemoveWorkspace(frame,id){frame.workspaces={...frame.workspaces,items:frame.workspaces.items.filter(workspace=>workspace.workspaceId!==id)}}
export function bSetOrderBy(frame,value){frame.store.orderBy=value}
export function bSetData(frame,sessions,workspaces){frame.sessions=structuredClone(sessions);frame.workspaces={...frame.workspaces,items:structuredClone(workspaces)}}
export function bSetRect(node,top,height){node.rect={top,height}}
export function bFindClassText(root,classPart,text){return walk(root).find(node=>String(node.props?.className??'').includes(classPart)&&walk(node).some(child=>textOf(child).includes(text)))}
export function bDrag(node,kind,clientY=0){const event={clientY,currentTarget:node,prevented:false,preventDefault(){this.prevented=true},dataTransfer:{effectAllowed:'',dropEffect:'',payload:[],setData(...args){this.payload.push(args)}}};node.props[kind]?.(event);return event}
export function bEmit(bench,name){const event={prevented:false,preventDefault(){this.prevented=true},dataTransfer:{dropEffect:''}};for(const listener of bench.listeners.get(name)??[])listener(event);return event}
export function bTick(ms=0){return new Promise(resolve=>setTimeout(resolve,ms))}
"#)]
extern "C" {
    fn makeWorkspaceBrowserBench() -> JsValue;
    fn makeBrowserFrame(bench: &JsValue) -> JsValue;
    fn bRender(bench: &JsValue, component: &JsValue, props: &JsValue) -> JsValue;
    fn bFind(root: &JsValue, key: &str, value: &JsValue) -> JsValue;
    fn bFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn bFindKinds(root: &JsValue, kind: &str) -> Array;
    fn bFindText(root: &JsValue, text: &str) -> JsValue;
    fn bFindRoleText(root: &JsValue, role: &str, text: &str) -> JsValue;
    fn bFindKindText(root: &JsValue, kind: &str, text: &str) -> JsValue;
    fn bMenuByAnchorAria(root: &JsValue, label: &str) -> JsValue;
    fn bModalByTitle(root: &JsValue, title: &str) -> JsValue;
    fn bText(root: &JsValue) -> String;
    fn bSummary(root: &JsValue) -> String;
    fn bProp(node: &JsValue, key: &str) -> JsValue;
    fn bClick(node: &JsValue) -> JsValue;
    fn bSelect(menu: &JsValue, id: &str) -> JsValue;
    fn bChange(node: &JsValue, value: &str) -> JsValue;
    fn bKey(node: &JsValue, key: &str) -> JsValue;
    fn bCalls(frame: &JsValue) -> Array;
    fn bStore(frame: &JsValue) -> JsValue;
    fn bSetWide(frame: &JsValue, value: bool);
    fn bSetSearchMode(frame: &JsValue, value: &str);
    fn bSetDirectory(frame: &JsValue, value: bool);
    fn bSetMode(frame: &JsValue, key: &str, value: &str);
    fn bSetExpanded(frame: &JsValue, key: &str, value: bool);
    fn bRemoveWorkspace(frame: &JsValue, id: &str);
    fn bSetOrderBy(frame: &JsValue, value: &str);
    fn bSetData(frame: &JsValue, sessions: &JsValue, workspaces: &JsValue);
    fn bSetRect(node: &JsValue, top: f64, height: f64);
    fn bFindClassText(root: &JsValue, class_part: &str, text: &str) -> JsValue;
    fn bDrag(node: &JsValue, kind: &str, client_y: f64) -> JsValue;
    fn bEmit(bench: &JsValue, name: &str) -> JsValue;
    fn bTick(ms: f64) -> Promise;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn configure() -> JsValue {
    let bench = makeWorkspaceBrowserBench();
    configure_client_ui_workspace(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    bench
}

fn render(bench: &JsValue, frame: &JsValue) -> JsValue {
    bRender(
        bench,
        &workspace_browser_component().unwrap(),
        &property(frame, "props"),
    )
}

fn has_call(frame: &JsValue, name: &str, argument: Option<&str>) -> bool {
    bCalls(frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some(name)
            && argument.is_none_or(|argument| call.get(1).as_string().as_deref() == Some(argument))
    })
}

#[wasm_bindgen_test(async)]
async fn grouped_and_flat_views_share_store_actions_rows_and_opening() {
    let bench = configure();
    let frame = makeBrowserFrame(&bench);
    let _ = render(&bench, &frame);
    let tree = render(&bench, &frame);
    assert!(bFindText(&tree, "Workspaces").is_object());
    assert!(bFindText(&tree, "alpha").is_object());
    assert!(bFindText(&tree, "alpha-s").is_undefined());
    assert!(has_call(&frame, "retainAccountKeys", None));

    let alpha = bFindRoleText(&tree, "treeitem", "alpha");
    let _ = bClick(&alpha);
    let expanded = render(&bench, &frame);
    assert!(bFindText(&expanded, "alpha-s").is_object());
    let session = bFindRoleText(&expanded, "treeitem", "alpha-s");
    let _ = bClick(&session);
    assert!(has_call(&frame, "open", Some("alpha-s")));

    let options_button = bFind(&expanded, "aria-label", &JsValue::from_str("View options"));
    let _ = bClick(&options_button);
    let options_open = render(&bench, &frame);
    let menu = bFind(
        &options_open,
        "selectedIds",
        &bProp(&bFindKind(&options_open, "Menu"), "selectedIds"),
    );
    bSelect(&menu, "flat");
    let flat_bench = configure();
    let flat = render(&flat_bench, &frame);
    assert_eq!(
        property(&bStore(&frame), "groupBy").as_string().as_deref(),
        Some("flat")
    );
    assert!(bFindText(&flat, "Sessions").is_object());
    assert!(bFindText(&flat, "alpha-s").is_object());
    assert!(bFindText(&flat, "beta-s").is_object());
    assert!(bFindText(&flat, "alpha").is_undefined());

    JsFuture::from(bTick(0.0)).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn search_is_bounded_debounced_cancellable_and_rail_controls_preserve_owner_actions() {
    let bench = configure();
    let frame = makeBrowserFrame(&bench);
    let initial = render(&bench, &frame);
    let input = bFind(
        &initial,
        "placeholder",
        &JsValue::from_str("Search sessions..."),
    );
    bChange(&input, "alpha");
    let loading = render(&bench, &frame);
    assert!(bFindText(&loading, "alpha-s").is_object());
    assert!(bFindText(&loading, "Searching session history…").is_object());
    JsFuture::from(bTick(300.0)).await.unwrap();
    JsFuture::from(bTick(0.0)).await.unwrap();
    let ready = render(&bench, &frame);
    assert!(bFindText(&ready, "content match").is_object());
    assert!(bText(&ready).contains("Showing the first 20 results"));
    let beta = bFindRoleText(&ready, "treeitem", "beta-s");
    let _ = bClick(&beta);
    assert!(has_call(&frame, "open", Some("beta-s")));

    bSetSearchMode(&frame, "fail");
    let ready_input = bFind(
        &ready,
        "placeholder",
        &JsValue::from_str("Search sessions..."),
    );
    bChange(&ready_input, "missing");
    let _ = render(&bench, &frame);
    JsFuture::from(bTick(300.0)).await.unwrap();
    JsFuture::from(bTick(0.0)).await.unwrap();
    let failed = render(&bench, &frame);
    assert!(
        bFindText(
            &failed,
            "Content search is temporarily unavailable. Showing name matches."
        )
        .is_object()
    );

    let failed_input = bFind(
        &failed,
        "placeholder",
        &JsValue::from_str("Search sessions..."),
    );
    let oversized = format!("{}\0😀tail", "a".repeat(499));
    bChange(&failed_input, &oversized);
    let bounded = render(&bench, &frame);
    let bounded_input = bFind(
        &bounded,
        "placeholder",
        &JsValue::from_str("Search sessions..."),
    );
    assert_eq!(
        bProp(&bounded_input, "value").as_string().as_deref(),
        Some("a".repeat(499).as_str())
    );
    let escape = bKey(&bounded_input, "Escape");
    assert_eq!(property(&escape, "prevented"), JsValue::FALSE);
    let cleared = render(&bench, &frame);
    assert!(bFindText(&cleared, "Workspaces").is_object());

    let rail_bench = configure();
    let rail_frame = makeBrowserFrame(&rail_bench);
    bSetWide(&rail_frame, false);
    let rail = render(&rail_bench, &rail_frame);
    assert!(
        bProp(&rail, "className")
            .as_string()
            .is_some_and(|value| value.contains("rail"))
    );
    assert!(bFindText(&rail, "alpha-s").is_undefined());
    let rail_search = bFind(&rail, "aria-label", &JsValue::from_str("Search sessions"));
    let _ = bClick(&rail_search);
    assert!(has_call(&rail_frame, "expandSidebar", None));

    let add = bFind(&rail, "aria-label", &JsValue::from_str("Add workspace"));
    let _ = bClick(&add);
    let _ = render(&rail_bench, &rail_frame);
    let flow = render(&rail_bench, &rail_frame);
    assert!(bText(&flow).contains("directory-flow"));
    assert_eq!(
        bCalls(&rail_frame)
            .iter()
            .filter(|call| Array::from(call).get(0).as_string().as_deref() == Some("expandSidebar"))
            .count(),
        1
    );

    let no_flow_bench = configure();
    let no_flow_frame = makeBrowserFrame(&no_flow_bench);
    bSetWide(&no_flow_frame, false);
    bSetDirectory(&no_flow_frame, false);
    let no_flow = render(&no_flow_bench, &no_flow_frame);
    assert!(bFind(&no_flow, "aria-label", &JsValue::from_str("Add workspace")).is_undefined());
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn workspace_and_session_rename_dialogs_preserve_validation_dispatch_and_failure_state() {
    let bench = configure();
    let frame = makeBrowserFrame(&bench);
    let initial = render(&bench, &frame);
    let workspace_actions = bFind(
        &initial,
        "aria-label",
        &JsValue::from_str("Workspace actions for alpha"),
    );
    let _ = bClick(&workspace_actions);
    let menu_tree = render(&bench, &frame);
    bSelect(
        &bMenuByAnchorAria(&menu_tree, "Workspace actions for alpha"),
        "rename",
    );
    let dialog = render(&bench, &frame);
    let workspace_modal = bModalByTitle(&dialog, "Rename workspace");
    assert_eq!(bProp(&workspace_modal, "open"), JsValue::TRUE);
    let input = bFind(&dialog, "aria-label", &JsValue::from_str("Workspace name"));
    assert_eq!(bProp(&input, "value").as_string().as_deref(), Some("alpha"));
    bChange(&input, " beta ");
    let duplicate = render(&bench, &frame);
    assert!(bText(&duplicate).contains("A workspace named “beta” already exists."));
    let duplicate_confirm = bFindKindText(&duplicate, "Button", "Rename");
    assert_eq!(bProp(&duplicate_confirm, "disabled"), JsValue::TRUE);

    let duplicate_input = bFind(
        &duplicate,
        "aria-label",
        &JsValue::from_str("Workspace name"),
    );
    bChange(&duplicate_input, "Gamma");
    let valid = render(&bench, &frame);
    let confirm = bFindKindText(&valid, "Button", "Rename");
    assert_eq!(bProp(&confirm, "disabled"), JsValue::FALSE);
    let _ = bClick(&confirm);
    assert!(has_call(&frame, "renameWorkspace", Some("alpha")));
    JsFuture::from(bTick(0.0)).await.unwrap();
    let closed = render(&bench, &frame);
    assert_eq!(
        bProp(&bModalByTitle(&closed, "Rename workspace"), "open"),
        JsValue::FALSE
    );

    let failure_bench = configure();
    let failure_frame = makeBrowserFrame(&failure_bench);
    bSetMode(&failure_frame, "renameMode", "text");
    let failure_initial = render(&failure_bench, &failure_frame);
    let _ = bClick(&bFind(
        &failure_initial,
        "aria-label",
        &JsValue::from_str("Workspace actions for alpha"),
    ));
    let failure_menu = render(&failure_bench, &failure_frame);
    bSelect(
        &bMenuByAnchorAria(&failure_menu, "Workspace actions for alpha"),
        "rename",
    );
    let failure_dialog = render(&failure_bench, &failure_frame);
    bChange(
        &bFind(
            &failure_dialog,
            "aria-label",
            &JsValue::from_str("Workspace name"),
        ),
        "Other",
    );
    let failure_valid = render(&failure_bench, &failure_frame);
    let _ = bClick(&bFindKindText(&failure_valid, "Button", "Rename"));
    JsFuture::from(bTick(0.0)).await.unwrap();
    let failed = render(&failure_bench, &failure_frame);
    assert!(bText(&failed).contains("rename denied"));
    assert_eq!(
        bProp(&bModalByTitle(&failed, "Rename workspace"), "open"),
        JsValue::TRUE
    );

    let session_bench = configure();
    let session_frame = makeBrowserFrame(&session_bench);
    bSetExpanded(&session_frame, "alpha", true);
    bSetMode(&session_frame, "sessionRenameMode", "fail");
    let session_initial = render(&session_bench, &session_frame);
    let _ = bClick(&bFind(
        &session_initial,
        "aria-label",
        &JsValue::from_str("Session actions for alpha-s"),
    ));
    let session_menu = render(&session_bench, &session_frame);
    bSelect(
        &bMenuByAnchorAria(&session_menu, "Session actions for alpha-s"),
        "rename",
    );
    let session_dialog = render(&session_bench, &session_frame);
    let session_input = bFind(
        &session_dialog,
        "aria-label",
        &JsValue::from_str("Session name"),
    );
    assert_eq!(
        bProp(&session_input, "value").as_string().as_deref(),
        Some("alpha-s")
    );
    let session_confirm = bFindKindText(
        &bModalByTitle(&session_dialog, "Rename session"),
        "Button",
        "Rename",
    );
    assert_eq!(bProp(&session_confirm, "disabled"), JsValue::FALSE);
    let _ = bClick(&session_confirm);
    JsFuture::from(bTick(0.0)).await.unwrap();
    let session_failed = render(&session_bench, &session_frame);
    assert!(bText(&session_failed).contains("session rename failed"));
    assert!(has_call(&session_frame, "renameSession", Some("alpha-s")));
}

#[wasm_bindgen_test(async)]
async fn delete_waits_for_workspace_echo_and_surfaces_retryable_failure() {
    let bench = configure();
    let frame = makeBrowserFrame(&bench);
    let initial = render(&bench, &frame);
    let _ = bClick(&bFind(
        &initial,
        "aria-label",
        &JsValue::from_str("Workspace actions for alpha"),
    ));
    let menu_tree = render(&bench, &frame);
    bSelect(
        &bMenuByAnchorAria(&menu_tree, "Workspace actions for alpha"),
        "delete",
    );
    let dialog = render(&bench, &frame);
    let modal = bModalByTitle(&dialog, "Delete workspace");
    assert_eq!(bProp(&modal, "open"), JsValue::TRUE);
    assert!(
        bProp(&modal, "description")
            .as_string()
            .is_some_and(|value| value.contains("folder and session logs will be kept"))
    );
    let confirm = bFindKindText(&dialog, "Button", "Delete workspace");
    let _ = bClick(&confirm);
    let pending = render(&bench, &frame);
    assert!(bFindText(&pending, "Deleting workspace…").is_object());
    assert_eq!(
        bProp(
            &bFindKindText(&pending, "Button", "Delete workspace"),
            "disabled"
        ),
        JsValue::TRUE
    );
    JsFuture::from(bTick(0.0)).await.unwrap();
    let awaiting_echo = render(&bench, &frame);
    assert_eq!(
        bProp(&bModalByTitle(&awaiting_echo, "Delete workspace"), "open"),
        JsValue::TRUE
    );
    bRemoveWorkspace(&frame, "alpha");
    let _ = render(&bench, &frame);
    let closed = render(&bench, &frame);
    assert_eq!(
        bProp(&bModalByTitle(&closed, "Delete workspace"), "open"),
        JsValue::FALSE
    );

    let failure_bench = configure();
    let failure_frame = makeBrowserFrame(&failure_bench);
    bSetMode(&failure_frame, "deleteMode", "fail");
    let failure_initial = render(&failure_bench, &failure_frame);
    let _ = bClick(&bFind(
        &failure_initial,
        "aria-label",
        &JsValue::from_str("Workspace actions for alpha"),
    ));
    let failure_menu = render(&failure_bench, &failure_frame);
    bSelect(
        &bMenuByAnchorAria(&failure_menu, "Workspace actions for alpha"),
        "delete",
    );
    let failure_dialog = render(&failure_bench, &failure_frame);
    let _ = bClick(&bFindKindText(
        &failure_dialog,
        "Button",
        "Delete workspace",
    ));
    JsFuture::from(bTick(0.0)).await.unwrap();
    let failed = render(&failure_bench, &failure_frame);
    assert!(bText(&failed).contains("delete failed"));
    assert_eq!(
        bProp(&bModalByTitle(&failed, "Delete workspace"), "open"),
        JsValue::TRUE
    );
    let _ = bClick(&bFindKindText(
        &bModalByTitle(&failed, "Delete workspace"),
        "Button",
        "Cancel",
    ));
    let cancelled = render(&failure_bench, &failure_frame);
    assert_eq!(
        bProp(&bModalByTitle(&cancelled, "Delete workspace"), "open"),
        JsValue::FALSE
    );
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn session_workspace_and_flat_drag_paths_resolve_markers_accounts_and_host_anchors() {
    let sessions = js_sys::JSON::parse(
        r#"{"ids":["one","two","three"],"byId":{"one":{"id":"one","displayTitle":"one","running":false,"blank":false,"updatedAt":3},"two":{"id":"two","displayTitle":"two","running":false,"blank":false,"updatedAt":2},"three":{"id":"three","displayTitle":"three","running":false,"blank":false,"updatedAt":1}},"phase":"ready","subagentsByParent":{},"jobsBySession":{}}"#,
    )
    .unwrap();
    let workspace_rows = js_sys::JSON::parse(
        r#"[{"workspaceId":"alpha","path":"/projects/alpha","title":"alpha","sessionIds":["one","two","three"],"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}]"#,
    )
    .unwrap();
    let bench = configure();
    let frame = makeBrowserFrame(&bench);
    bSetOrderBy(&frame, "manual");
    bSetExpanded(&frame, "alpha", true);
    bSetData(&frame, &sessions, &workspace_rows);
    let _ = render(&bench, &frame);
    let initial = render(&bench, &frame);
    let one = bFindRoleText(&initial, "treeitem", "one");
    let started = bDrag(&one, "onDragStart", 0.0);
    assert_eq!(
        property(&property(&started, "dataTransfer"), "effectAllowed")
            .as_string()
            .as_deref(),
        Some("move")
    );
    let active = render(&bench, &frame);
    assert_eq!(property(&bEmit(&bench, "drop"), "prevented"), JsValue::TRUE);
    let three = bFindRoleText(&active, "treeitem", "three");
    assert!(
        !three.is_undefined(),
        "active grouped tree: {}",
        bText(&active)
    );
    bSetRect(&three, 200.0, 34.0);
    let dropped = bDrag(&three, "onDrop", 205.0);
    assert_eq!(property(&dropped, "prevented"), JsValue::TRUE);
    assert!(bCalls(&frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("insertSessionBefore")
            && call.get(1).as_string().as_deref() == Some("alpha")
            && call.get(2).as_string().as_deref() == Some("one")
            && call.get(3).as_string().as_deref() == Some("three")
    }));
    let store = bStore(&frame);
    let order = property(&property(&store, "sessionOrderByAccount"), "alpha");
    assert_eq!(
        Array::from(&order)
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        vec!["two", "one", "three"]
    );

    let workspace_bench = configure();
    let workspace_frame = makeBrowserFrame(&workspace_bench);
    let empty_sessions = js_sys::JSON::parse(
        r#"{"ids":[],"byId":{},"phase":"ready","subagentsByParent":{},"jobsBySession":{}}"#,
    )
    .unwrap();
    let three_workspaces = js_sys::JSON::parse(
        r#"[{"workspaceId":"alpha","path":"/projects/alpha","title":"alpha","sessionIds":[],"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"},{"workspaceId":"beta","path":"/projects/beta","title":"beta","sessionIds":[],"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"},{"workspaceId":"tail","path":"/projects/tail","title":"tail","sessionIds":[],"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}]"#,
    )
    .unwrap();
    bSetData(&workspace_frame, &empty_sessions, &three_workspaces);
    let workspace_initial = render(&workspace_bench, &workspace_frame);
    let tail = bFindRoleText(&workspace_initial, "treeitem", "tail");
    let _ = bDrag(&tail, "onDragStart", 0.0);
    let workspace_active = render(&workspace_bench, &workspace_frame);
    let beta_section = bFindClassText(&workspace_active, "groupSection", "beta");
    assert!(
        !beta_section.is_undefined(),
        "active workspace tree: {}\n{}",
        bText(&workspace_active),
        bSummary(&workspace_active)
    );
    bSetRect(&beta_section, 100.0, 34.0);
    let _ = bDrag(&beta_section, "onDrop", 105.0);
    assert!(bCalls(&workspace_frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("insertWorkspaceBefore")
            && call.get(1).as_string().as_deref() == Some("tail")
            && call.get(2).as_string().as_deref() == Some("beta")
    }));

    let flat_bench = configure();
    let flat_frame = makeBrowserFrame(&flat_bench);
    bSetOrderBy(&flat_frame, "manual");
    bSetData(&flat_frame, &sessions, &workspace_rows);
    Reflect::set(
        &bStore(&flat_frame),
        &JsValue::from_str("groupBy"),
        &JsValue::from_str("flat"),
    )
    .unwrap();
    let _ = render(&flat_bench, &flat_frame);
    let flat_initial = render(&flat_bench, &flat_frame);
    let flat_one = bFindRoleText(&flat_initial, "treeitem", "one");
    let _ = bDrag(&flat_one, "onDragStart", 0.0);
    let flat_active = render(&flat_bench, &flat_frame);
    let flat_three = bFindRoleText(&flat_active, "treeitem", "three");
    assert!(
        !flat_three.is_undefined(),
        "active flat tree: {}",
        bText(&flat_active)
    );
    bSetRect(&flat_three, 200.0, 34.0);
    let _ = bDrag(&flat_three, "onDrop", 230.0);
    let flat_order = property(
        &property(&bStore(&flat_frame), "sessionOrderByAccount"),
        "__flat_session_order__",
    );
    assert_eq!(
        Array::from(&flat_order)
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        vec!["two", "three", "one"]
    );
    assert!(!has_call(&flat_frame, "insertSessionBefore", None));
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn expansion_overflow_row_verbs_and_ungrouped_posture_match_the_owner_contract() {
    let bench = configure();
    let frame = makeBrowserFrame(&bench);
    let ids = (1..=7).map(|index| format!("s{index}")).collect::<Vec<_>>();
    let summaries = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            format!(
                "\"{id}\":{{\"id\":\"{id}\",\"displayTitle\":\"{id}\",\"running\":false,\"blank\":false,\"updatedAt\":{}}}",
                7 - index
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let sessions = js_sys::JSON::parse(&format!(
        "{{\"ids\":[{}],\"byId\":{{{summaries}}},\"phase\":\"ready\",\"current\":\"s1\",\"subagentsByParent\":{{}},\"jobsBySession\":{{}}}}",
        ids.iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(",")
    ))
    .unwrap();
    let workspaces = js_sys::JSON::parse(&format!(
        "[{{\"workspaceId\":\"alpha\",\"path\":\"/projects/alpha\",\"title\":\"alpha\",\"sessionIds\":[{}],\"createdAt\":\"2026-01-01T00:00:00.000Z\",\"updatedAt\":\"2026-01-01T00:00:00.000Z\"}}]",
        ids.iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(",")
    ))
    .unwrap();
    bSetData(&frame, &sessions, &workspaces);
    let _ = render(&bench, &frame);
    let expanded = render(&bench, &frame);
    assert!(bFindText(&expanded, "s1").is_object());
    assert!(bFindText(&expanded, "s5").is_object());
    assert!(bFindText(&expanded, "s6").is_undefined());
    let show_more = bFindKindText(&expanded, "button", "Show 2 more sessions");
    let _ = bClick(&show_more);
    let all = render(&bench, &frame);
    assert!(bFindText(&all, "s6").is_object());
    assert!(bFindText(&all, "s7").is_object());
    let _ = bClick(&bFindRoleText(&all, "treeitem", "alpha"));
    let collapsed = render(&bench, &frame);
    assert!(bFindText(&collapsed, "s1").is_undefined());
    let _ = bClick(&bFindRoleText(&collapsed, "treeitem", "alpha"));
    let five_again = render(&bench, &frame);
    assert!(bFindText(&five_again, "s5").is_object());
    assert!(bFindText(&five_again, "s6").is_undefined());

    let _ = bClick(&bFind(
        &five_again,
        "aria-label",
        &JsValue::from_str("New session in alpha"),
    ));
    assert!(has_call(&frame, "startSession", Some("alpha")));
    let action_open = render(&bench, &frame);
    let _ = bClick(&bFind(
        &action_open,
        "aria-label",
        &JsValue::from_str("Session actions for s1"),
    ));
    let menu_tree = render(&bench, &frame);
    let menu = bMenuByAnchorAria(&menu_tree, "Session actions for s1");
    bSelect(&menu, "fork");
    assert!(has_call(&frame, "forkSession", Some("s1")));
    let menu_closed = render(&bench, &frame);
    let _ = bClick(&bFind(
        &menu_closed,
        "aria-label",
        &JsValue::from_str("Session actions for s1"),
    ));
    let archive_tree = render(&bench, &frame);
    bSelect(
        &bMenuByAnchorAria(&archive_tree, "Session actions for s1"),
        "archive",
    );
    assert!(has_call(&frame, "archiveSession", Some("s1")));

    let ungrouped_bench = configure();
    let ungrouped_frame = makeBrowserFrame(&ungrouped_bench);
    let loose_sessions = js_sys::JSON::parse(
        r#"{"ids":["loose"],"byId":{"loose":{"id":"loose","displayTitle":"loose","running":false,"blank":false,"updatedAt":1}},"phase":"ready","current":"loose","subagentsByParent":{},"jobsBySession":{}}"#,
    )
    .unwrap();
    let no_workspaces = js_sys::JSON::parse("[]").unwrap();
    bSetData(&ungrouped_frame, &loose_sessions, &no_workspaces);
    let _ = render(&ungrouped_bench, &ungrouped_frame);
    let ungrouped = render(&ungrouped_bench, &ungrouped_frame);
    assert!(bFindText(&ungrouped, "loose").is_object());
    assert!(
        bFind(
            &ungrouped,
            "aria-label",
            &JsValue::from_str("Workspace actions for Ungrouped")
        )
        .is_undefined()
    );
    let _ = bClick(&bFind(
        &ungrouped,
        "aria-label",
        &JsValue::from_str("New session in Ungrouped"),
    ));
    assert!(!has_call(&ungrouped_frame, "startSession", None));
}
