//! Live React parity for compiled Workspace browser rows.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Promise, Reflect};
use seekdeep_client_ui_workspace::{
    configure_client_ui_workspace, project_row_item_component, search_result_item_component,
    session_node_item_component,
};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten=values=>values.flat(Infinity).filter(value=>value!==null&&value!==undefined&&value!==false)
let cached
export function makeWorkspaceRowsBench(){
  if(cached){cached.reset();return cached}
  const states=[],styles=[];let si=0
  const Fragment=Symbol('Fragment')
  const React={Fragment,createElement(kind,supplied,...children){const flat=flatten(children);const props={...(supplied??{})};if(flat.length===1)props.children=flat[0];else if(flat.length>1)props.children=flat;return{kind,props,children:flat}},useState(initial){const at=si++;if(!(at in states))states[at]=typeof initial==='function'?initial():initial;return[states[at],value=>{states[at]=typeof value==='function'?value(states[at]):value}]},useCallback(callback){return callback},useEffect(run){run()},useMemo(factory){return factory()},useRef(initial){return{current:initial}}}
  function resolve(value){if(Array.isArray(value))return flatten(value.map(resolve));if(value===null||value===undefined||value===false||typeof value!=='object')return value;if(!('kind'in value))return value;if(typeof value.kind==='function')return resolve(value.kind(value.props));if(value.kind===Fragment)return{kind:'Fragment',props:value.props,children:flatten(value.children.map(resolve))};return{...value,children:flatten(value.children.map(resolve))}}
  const copy={
    'group.ungrouped':'Ungrouped','session.new':'New Session','rename':'Rename','delete.workspace':'Delete workspace',
    'menu.fork':'Fork session','menu.archiveSession':'Archive session','actions.workspace.aria':'Workspace actions for {name}',
    'actions.session.aria':'Session actions for {name}','actions.newSession.aria':'New session in {name}',
    'status.running':'Running','status.subagentsRunning.one':'{n} subagent running','status.subagentsRunning.other':'{n} subagents running',
    'status.idle':'Idle','status.waitingApproval':'Waiting for approval','status.planReview':'Plan awaiting review',
    'status.waitingAnswer':'Waiting for answer','status.completed':'Completed','hover.created':'Created {time}',
    'hover.copied':'Copied','date.ymd':'{y}-{m}-{d}','time.now':'now','time.minutes':'{n}min','time.hours':'{n}h',
    'time.days':'{n}d','time.months':'{n}mo','time.years':'{n}y','time.ago':'{t} ago',copy:'Copy',
    'menu.addWorkspace':'Add workspace…','picker.loading':'Loading workspaces…','folderError.title':'Couldn’t open folder',
    'folderError.retry':'Choose again',close:'Close',cancel:'Cancel',
  }
  const t=(key,vars={})=>Object.entries(vars).reduce((text,[name,value])=>text.replaceAll(`{${name}}`,String(value)),copy[key]??key)
  globalThis.document={head:{appendChild(node){styles.push(node);return node}},createElement(){return{setAttribute(){},textContent:''}},querySelector(){return null}}
  const primitives={}
  for(const name of ['Button','HoverCard','IconArchiveOutline20','IconBranchOutline16','IconCloseFill14','IconEditOutline16','IconEllipsisOutline16','IconFolderClose16','IconFolderOpen16','IconPersonalizationOutline16','IconPlusOutline16','IconProjectAddOutline16','IconSearchOutline16','IconTrashOutline16','IconTriangleRightFill14','Menu','Modal','StateDot','Tooltip'])primitives[name]=name
  cached={React,primitives,t,styles,render(component,props){si=0;return resolve(React.createElement(component,props))},reset(){states.length=0;styles.length=0}}
  return cached
}
export function makeProjectFrame(bench,group,options={}){const calls=[];const drag={active:!!options.dragActive,marker:options.marker??null,start(){calls.push(['dragStart'])},end(){calls.push(['dragEnd'])}};const props={group,t:bench.t,onToggle(){calls.push(['toggle'])},onCreate(){calls.push(['create'])},...(options.actions===false?{}:{actions:{rename(){calls.push(['rename'])},delete(){calls.push(['delete'])}}}),...(options.drag?{drag}:{})};return{props,calls,drag}}
export function makeSearchFrame(bench,result,currentId){const calls=[];return{props:{result,currentId,t:bench.t,onOpen(id){calls.push(['open',id])}},calls}}
export function makeSessionFrame(bench,node,options={}){const calls=[];const drag={active:!!options.dragActive,marker:options.marker??null,start(){calls.push(['dragStart'])},hover(half){calls.push(['hover',half])},drop(half){calls.push(['drop',half])},end(){calls.push(['dragEnd'])}};const props={node,currentId:options.currentId,now:options.now??0,flat:!!options.flat,t:bench.t,onOpen(id){calls.push(['open',id])},onRename(id,title){calls.push(['rename',id,title])},onFork(id){calls.push(['fork',id])},onArchive(id){calls.push(['archive',id])},...(options.drag?{drag}:{})};return{props,calls,drag}}
export function rowsRender(bench,component,props){return bench.render(component,props)}
function walk(root,out=[]){if(!root||typeof root!=='object')return out;if(Array.isArray(root)){root.forEach(value=>walk(value,out));return out}if('kind'in root)out.push(root);(root.children??[]).forEach(value=>walk(value,out));return out}
function textOf(root){const parts=[];const visit=value=>{if(typeof value==='string'||typeof value==='number')parts.push(String(value));else if(Array.isArray(value))value.forEach(visit);else if(value&&typeof value==='object')(value.children??[]).forEach(visit)};visit(root);return parts.join('')}
export function rowsFindKind(root,kind){return walk(root).find(node=>node.kind===kind)}
export function rowsFindKinds(root,kind){return walk(root).filter(node=>node.kind===kind)}
export function rowsFindText(root,text){return walk(root).find(node=>textOf(node)===text)}
export function rowsFindAria(root,label){return walk(root).find(node=>node.props?.['aria-label']===label)}
export function rowsProp(node,key){return node?.props?.[key]}
export function rowsClick(node){const event={stopped:false,stopPropagation(){this.stopped=true}};node.props.onClick?.(event);return event}
export function rowsSelect(menu,id){return menu.props.onSelect(id)}
export function rowsClose(menu){return menu.props.onClose()}
export function rowsCalls(frame){return frame.calls}
export function rowsSetDrag(frame,active,marker){frame.drag.active=active;frame.drag.marker=marker}
export function rowsDrag(row,kind,clientY=0){const event={clientY,currentTarget:{getBoundingClientRect(){return{top:100,height:34}}},prevented:false,preventDefault(){this.prevented=true},dataTransfer:{effectAllowed:'',dropEffect:'',payload:[],setData(...args){this.payload.push(args)}}};row.props[kind]?.(event);return event}
export function rowsTick(){return new Promise(resolve=>setTimeout(resolve,0))}
"#)]
extern "C" {
    fn makeWorkspaceRowsBench() -> JsValue;
    fn makeProjectFrame(bench: &JsValue, group: &JsValue, options: &JsValue) -> JsValue;
    fn makeSearchFrame(bench: &JsValue, result: &JsValue, current_id: &str) -> JsValue;
    fn makeSessionFrame(bench: &JsValue, node: &JsValue, options: &JsValue) -> JsValue;
    fn rowsRender(bench: &JsValue, component: &JsValue, props: &JsValue) -> JsValue;
    fn rowsFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn rowsFindKinds(root: &JsValue, kind: &str) -> Array;
    fn rowsFindText(root: &JsValue, text: &str) -> JsValue;
    fn rowsFindAria(root: &JsValue, label: &str) -> JsValue;
    fn rowsProp(node: &JsValue, key: &str) -> JsValue;
    fn rowsClick(node: &JsValue) -> JsValue;
    fn rowsSelect(menu: &JsValue, id: &str) -> JsValue;
    fn rowsClose(menu: &JsValue) -> JsValue;
    fn rowsCalls(frame: &JsValue) -> Array;
    fn rowsSetDrag(frame: &JsValue, active: bool, marker: &JsValue);
    fn rowsDrag(row: &JsValue, kind: &str, client_y: f64) -> JsValue;
    fn rowsTick() -> Promise;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn json(source: &str) -> JsValue {
    js_sys::JSON::parse(source).unwrap()
}

fn configure() -> JsValue {
    let bench = makeWorkspaceRowsBench();
    configure_client_ui_workspace(property(&bench, "React"), property(&bench, "primitives"))
        .unwrap();
    bench
}

fn has_call(frame: &JsValue, name: &str, argument: Option<&str>) -> bool {
    rowsCalls(frame).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some(name)
            && argument.is_none_or(|argument| call.get(1).as_string().as_deref() == Some(argument))
    })
}

#[wasm_bindgen_test]
fn project_row_preserves_actions_hover_payload_active_state_and_ungrouped_posture() {
    let bench = configure();
    let group = json(
        r#"{"key":"project","workspaceId":"project","cwd":"/projects/project","createdAt":0,"label":"Project","sessionCount":1,"expanded":true,"containsCurrent":true,"sessions":[]}"#,
    );
    let frame = makeProjectFrame(&bench, &group, &json(r#"{"drag":true}"#));
    let tree = rowsRender(
        &bench,
        &project_row_item_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(
        property(&tree, "kind").as_string().as_deref(),
        Some("HoverCard")
    );
    assert_eq!(
        rowsProp(&tree, "copyText").as_string().as_deref(),
        Some("/projects/project")
    );
    assert!(rowsFindText(&rowsProp(&tree, "content"), "Project").is_object());
    assert!(rowsFindText(&rowsProp(&tree, "content"), "/projects/project").is_object());
    let row = rowsProp(&tree, "anchor");
    assert_eq!(rowsProp(&row, "aria-expanded"), JsValue::TRUE);
    assert_eq!(rowsProp(&row, "draggable"), JsValue::TRUE);
    assert!(rowsProp(&rowsFindKind(&row, "IconFolderOpen16"), "className").is_undefined());
    assert!(
        rowsProp(&rowsFindKind(&row, "span"), "className")
            .as_string()
            .is_some_and(|value| value.contains("folderActive"))
    );

    let create = rowsFindAria(&row, "New session in Project");
    assert_eq!(rowsClick(&create).as_string(), None);
    assert!(has_call(&frame, "create", None));
    assert!(!has_call(&frame, "toggle", None));
    let _ = rowsClick(&row);
    assert!(has_call(&frame, "toggle", None));
    let drag_start = rowsDrag(&row, "onDragStart", 0.0);
    assert_eq!(
        property(&property(&drag_start, "dataTransfer"), "effectAllowed")
            .as_string()
            .as_deref(),
        Some("move")
    );
    let _ = rowsDrag(&row, "onDragEnd", 0.0);
    assert!(has_call(&frame, "dragStart", None));
    assert!(has_call(&frame, "dragEnd", None));

    let menu = rowsFindKind(&row, "Menu");
    let items = Array::from(&rowsProp(&menu, "items"));
    assert_eq!(items.length(), 2);
    assert_eq!(property(&items.get(1), "danger"), JsValue::TRUE);
    rowsSelect(&menu, "rename");
    rowsSelect(&menu, "future-item");
    rowsSelect(&menu, "delete");
    assert!(has_call(&frame, "rename", None));
    assert!(has_call(&frame, "delete", None));

    let anchor = rowsProp(&menu, "anchor");
    assert_eq!(property(&rowsClick(&anchor), "stopped"), JsValue::TRUE);
    let opened = rowsRender(
        &bench,
        &project_row_item_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(rowsProp(&opened, "disabled"), JsValue::TRUE);
    assert_eq!(
        rowsProp(&rowsFindKind(&rowsProp(&opened, "anchor"), "Menu"), "open"),
        JsValue::TRUE
    );

    let ungrouped = json(
        r#"{"key":"","label":"ignored","sessionCount":0,"expanded":false,"containsCurrent":false,"sessions":[]}"#,
    );
    let ungrouped_frame = makeProjectFrame(&bench, &ungrouped, &json(r#"{"actions":false}"#));
    let ungrouped_tree = rowsRender(
        &bench,
        &project_row_item_component().unwrap(),
        &property(&ungrouped_frame, "props"),
    );
    assert_eq!(
        property(&ungrouped_tree, "kind").as_string().as_deref(),
        Some("div")
    );
    assert!(rowsFindText(&ungrouped_tree, "Ungrouped").is_object());
    assert!(rowsFindKind(&ungrouped_tree, "Menu").is_undefined());
    let styles = Array::from(&property(&bench, "styles"));
    assert_eq!(styles.length(), 3);
    assert!(
        property(&styles.get(1), "textContent")
            .as_string()
            .is_some_and(|value| value.contains(".seekdeep-workspace-projectRow"))
    );
}

#[wasm_bindgen_test]
fn search_rows_preserve_selection_context_snippet_status_precedence_and_open_identity() {
    let bench = configure();
    let result = json(
        r#"{"id":"result","title":"Needs input","workspace":"Workspace context","pendingInteraction":"question","running":true,"runningSubagentCount":1,"completed":false,"snippet":"matching excerpt"}"#,
    );
    let frame = makeSearchFrame(&bench, &result, "result");
    let tree = rowsRender(
        &bench,
        &search_result_item_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(rowsProp(&tree, "aria-selected"), JsValue::TRUE);
    assert_eq!(
        rowsProp(&rowsFindKind(&tree, "StateDot"), "state")
            .as_string()
            .as_deref(),
        Some("warning")
    );
    assert!(rowsFindText(&tree, "Waiting for answer").is_object());
    assert!(rowsFindText(&tree, "1 subagent running").is_object());
    assert!(rowsFindText(&tree, "Workspace context").is_object());
    assert!(rowsFindText(&tree, "matching excerpt").is_object());
    assert!(rowsProp(&tree, "draggable").is_undefined());
    let _ = rowsClick(&tree);
    assert!(has_call(&frame, "open", Some("result")));

    let completed = json(
        r#"{"id":"done","title":"Done","workspace":"Workspace","running":false,"runningSubagentCount":0,"completed":true}"#,
    );
    let completed_frame = makeSearchFrame(&bench, &completed, "other");
    let done = rowsRender(
        &bench,
        &search_result_item_component().unwrap(),
        &property(&completed_frame, "props"),
    );
    assert_eq!(
        rowsProp(&rowsFindKind(&done, "StateDot"), "state")
            .as_string()
            .as_deref(),
        Some("done")
    );
}

#[wasm_bindgen_test]
fn session_rows_preserve_blank_flat_status_time_hover_and_menu_contracts() {
    let bench = configure();
    let blank = json(
        r#"{"id":"blank","title":"ignored","blank":true,"running":false,"runningSubagentCount":0,"completed":false,"updatedAt":0}"#,
    );
    let blank_frame = makeSessionFrame(
        &bench,
        &blank,
        &json(r#"{"currentId":"blank","flat":true}"#),
    );
    let blank_tree = rowsRender(
        &bench,
        &session_node_item_component().unwrap(),
        &property(&blank_frame, "props"),
    );
    let blank_row = rowsProp(&blank_tree, "anchor");
    assert!(
        rowsProp(&blank_row, "className")
            .as_string()
            .is_some_and(|value| value.contains("flatSessionRowWithoutStatus"))
    );
    assert!(rowsFindText(&blank_row, "New Session").is_object());
    assert!(rowsFindKind(&blank_row, "Menu").is_undefined());
    assert!(rowsFindText(&blank_row, "now").is_undefined());
    assert!(rowsProp(&blank_tree, "copyText").is_undefined());
    assert!(rowsFindText(&rowsProp(&blank_tree, "content"), "Idle").is_object());
    assert!(rowsFindText(&rowsProp(&blank_tree, "content"), "now").is_undefined());

    let session = json(
        r#"{"id":"owner","title":"Delegating","blank":false,"pendingInteraction":"approval","running":true,"runningSubagentCount":2,"completed":true,"updatedAt":0}"#,
    );
    let frame = makeSessionFrame(
        &bench,
        &session,
        &json(r#"{"currentId":"owner","now":60000}"#),
    );
    let tree = rowsRender(
        &bench,
        &session_node_item_component().unwrap(),
        &property(&frame, "props"),
    );
    let row = rowsProp(&tree, "anchor");
    assert_eq!(rowsProp(&row, "aria-selected"), JsValue::TRUE);
    assert_eq!(
        rowsProp(&rowsFindKind(&row, "StateDot"), "state")
            .as_string()
            .as_deref(),
        Some("warning")
    );
    assert!(rowsFindText(&row, "Waiting for approval").is_object());
    assert!(rowsFindText(&row, "2 subagents running").is_object());
    assert!(rowsFindText(&row, "1min").is_object());
    let hover = rowsProp(&tree, "content");
    assert!(rowsFindText(&hover, "1min ago").is_object());
    assert_eq!(rowsFindKinds(&hover, "StateDot").length(), 2);

    let menu = rowsFindKind(&row, "Menu");
    assert_eq!(Array::from(&rowsProp(&menu, "items")).length(), 3);
    assert!(property(&Array::from(&rowsProp(&menu, "items")).get(2), "danger").is_undefined());
    let anchor = rowsProp(&menu, "anchor");
    assert_eq!(property(&rowsClick(&anchor), "stopped"), JsValue::TRUE);
    let opened = rowsRender(
        &bench,
        &session_node_item_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(rowsProp(&opened, "disabled"), JsValue::TRUE);
    let opened_menu = rowsFindKind(&rowsProp(&opened, "anchor"), "Menu");
    assert_eq!(rowsProp(&opened_menu, "open"), JsValue::TRUE);
    rowsSelect(&opened_menu, "rename");
    rowsSelect(&opened_menu, "fork");
    rowsSelect(&opened_menu, "archive");
    rowsClose(&opened_menu);
    assert!(has_call(&frame, "rename", Some("owner")));
    assert!(has_call(&frame, "fork", Some("owner")));
    assert!(has_call(&frame, "archive", Some("owner")));
    assert!(!has_call(&frame, "open", None));
    let _ = rowsClick(&rowsProp(&opened, "anchor"));
    assert!(has_call(&frame, "open", Some("owner")));
}

#[wasm_bindgen_test(async)]
async fn session_drag_wiring_gates_hit_testing_reports_halves_and_suppresses_hover() {
    let bench = configure();
    let node = json(
        r#"{"id":"drag","title":"Drag me","blank":false,"running":false,"runningSubagentCount":0,"completed":false,"updatedAt":0}"#,
    );
    let frame = makeSessionFrame(&bench, &node, &json(r#"{"drag":true}"#));
    let tree = rowsRender(
        &bench,
        &session_node_item_component().unwrap(),
        &property(&frame, "props"),
    );
    let row = rowsProp(&tree, "anchor");
    assert_eq!(rowsProp(&row, "draggable"), JsValue::TRUE);
    let start = rowsDrag(&row, "onDragStart", 0.0);
    assert_eq!(
        property(&property(&start, "dataTransfer"), "effectAllowed")
            .as_string()
            .as_deref(),
        Some("move")
    );
    let inactive_over = rowsDrag(&row, "onDragOver", 105.0);
    assert_eq!(property(&inactive_over, "prevented"), JsValue::FALSE);
    let _ = rowsDrag(&row, "onDrop", 130.0);
    let _ = rowsDrag(&row, "onDragEnd", 0.0);
    assert!(has_call(&frame, "dragStart", None));
    assert!(has_call(&frame, "dragEnd", None));
    assert!(!has_call(&frame, "hover", None));
    assert!(!has_call(&frame, "drop", None));

    rowsSetDrag(&frame, true, &JsValue::from_str("before"));
    let active = rowsRender(
        &bench,
        &session_node_item_component().unwrap(),
        &property(&frame, "props"),
    );
    assert_eq!(rowsProp(&active, "disabled"), JsValue::TRUE);
    let active_row = rowsProp(&active, "anchor");
    assert!(
        rowsProp(&active_row, "className")
            .as_string()
            .is_some_and(|value| value.contains("dropBefore"))
    );
    let before = rowsDrag(&active_row, "onDragOver", 105.0);
    assert_eq!(property(&before, "prevented"), JsValue::TRUE);
    assert_eq!(
        property(&property(&before, "dataTransfer"), "dropEffect")
            .as_string()
            .as_deref(),
        Some("move")
    );
    let _ = rowsDrag(&active_row, "onDragOver", 130.0);
    let _ = rowsDrag(&active_row, "onDrop", 130.0);
    assert!(has_call(&frame, "hover", Some("before")));
    assert!(has_call(&frame, "hover", Some("after")));
    assert!(has_call(&frame, "drop", Some("after")));

    JsFuture::from(rowsTick()).await.unwrap();
}
