//! Live compiled Tool rows, recursive dispatch, details, and plugin assembly.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, JSON, Object, Reflect};
use seekdeep_client_ui_tool::{
    apply_client_ui_tool, ask_question_row_component, bash_row_component, configure_client_ui_tool,
    file_mutation_row_component, generic_tool_card_component, read_row_component,
    search_row_component, todo_row_component, tool_call_tree_component, tool_details_component,
    tool_inject_browser, tool_row_component, web_row_component,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten = values => values.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)

function makeEngine() {
  const states = []
  const memos = []
  let stateCursor = 0
  let memoCursor = 0
  const Fragment = Symbol('Fragment')
  const React = {
    Fragment,
    createElement(kind, supplied, ...children) {
      const flat = flatten(children)
      const props = { ...(supplied ?? {}) }
      if (flat.length === 1) props.children = flat[0]
      else if (flat.length > 1) props.children = flat
      return { kind, props, children: flat }
    },
    memo(component) { return component },
    useMemo(factory, deps) {
      const index = memoCursor++
      const previous = memos[index]
      const same = previous !== undefined && previous.deps.length === deps.length
        && deps.every((value, at) => Object.is(value, previous.deps[at]))
      if (!same) memos[index] = { value: factory(), deps: [...deps] }
      return memos[index].value
    },
    useState(initial) {
      const index = stateCursor++
      if (!(index in states)) states[index] = typeof initial === 'function' ? initial() : initial
      return [states[index], update => {
        states[index] = typeof update === 'function' ? update(states[index]) : update
      }]
    },
  }
  function resolve(value) {
    if (Array.isArray(value)) return flatten(value.map(resolve))
    if (value === null || value === undefined || value === false || typeof value !== 'object') return value
    if (!('kind' in value)) return value
    if (typeof value.kind === 'function') return resolve(value.kind(value.props))
    if (value.kind === Fragment) return { kind: 'Fragment', props: value.props, children: flatten(value.children.map(resolve)) }
    return { ...value, children: flatten(value.children.map(resolve)) }
  }
  return {
    React,
    render(component, props) {
      stateCursor = 0
      memoCursor = 0
      return resolve(React.createElement(component, props))
    },
    reset() { states.length = 0; memos.length = 0 },
  }
}

function primitive(engine, name) {
  return props => engine.React.createElement(name, props)
}

export function makeToolHarness() {
  const styles = []
  globalThis.document = {
    head: { appendChild(node) { styles.push(node); return node } },
    createElement(kind) { return { kind, attributes: {}, textContent: '', setAttribute(key, value) { this.attributes[key] = value } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
  }
  const engine = makeEngine()
  const simple = name => primitive(engine, name)
  const DisclosureRow = props => {
    const interactive = props.expandable === true
    const row = engine.React.createElement('div', {
      className: props.rowClassName,
      role: interactive ? 'button' : undefined,
      tabIndex: interactive ? 0 : undefined,
      'data-expandable': interactive || undefined,
      'aria-expanded': interactive ? props.open : undefined,
      onClick: interactive && props.expandOnRowClick ? props.onToggle : undefined,
      onKeyDown: interactive ? event => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault()
          props.onToggle()
        }
      } : undefined,
    },
    engine.React.createElement('span', { className: props.leadingClassName }, props.icon),
    engine.React.createElement('span', { className: props.titleClassName }, props.title),
    props.collapsedContent)
    return engine.React.createElement('section', {}, row, props.open ? props.children : null)
  }
  const primitives = {
    CodeBlock: props => engine.React.createElement('CodeBlock', { ...props, 'data-code': true }, props.code),
    DiffBlock: props => engine.React.createElement('DiffBlock', { ...props, 'data-diff': true }),
    DisclosureRow,
    ReadBlock: props => engine.React.createElement('ReadBlock', { ...props, 'data-read': true }),
    SearchBlock: props => engine.React.createElement('SearchBlock', { ...props, 'data-search': true }),
    StateDot: props => engine.React.createElement('StateDot', { ...props, 'data-state-dot': props.state }),
    TerminalBlock: props => engine.React.createElement('TerminalBlock', { ...props, 'data-terminal': true }, props.command, props.output),
    WebBlock: props => engine.React.createElement('WebBlock', { ...props, 'data-web': true }),
    IconApiOutline14: simple('IconApiOutline14'),
    IconBrowseOutline16: simple('IconBrowseOutline16'),
    IconChecklistOutline14: simple('IconChecklistOutline14'),
    IconChevronDownOutline14: simple('IconChevronDownOutline14'),
    IconCodeOutline16: simple('IconCodeOutline16'),
    IconEditOutline16: simple('IconEditOutline16'),
    IconGlobeOutline14: simple('IconGlobeOutline14'),
    IconInspectOutline12: simple('IconInspectOutline12'),
    IconQuestionOutline14: simple('IconQuestionOutline14'),
    IconSearchOutline16: simple('IconSearchOutline16'),
    IconSparkle16: simple('IconSparkle16'),
  }
  const dictionaries = {
    'ask.rowTitle': 'Ask question', 'ask.waiting': 'waiting', 'ask.cancelled': 'cancelled',
    'ask.interrupted': 'interrupted', 'ask.answered': '{answered}/{total} answered',
    'todo.rowTitle': 'Update to-do list', 'todo.completed': '{done}/{total} completed',
    'row.running': 'Running', 'row.failed': 'Failed', 'row.stopped': 'Stopped',
    'bash.running': 'Running', 'bash.failed': 'Failed', 'bash.stopped': 'Stopped',
    'details.running': 'Running…', 'terminal.signal': 'signal {signal}',
    'terminal.exitCode': 'exit {code}', 'terminal.running': 'Running', 'terminal.failed': 'Failed',
    'terminal.done': 'Done', 'terminal.noOutput': 'No output', 'terminal.collapseAria': 'Collapse output',
    'terminal.expandAria': 'Show {n}', 'terminal.expandRest': 'Show {n}',
    copy: 'Copy', copied: 'Copied', collapse: 'Collapse',
  }
  const t = (key, values = {}) => String(dictionaries[key] ?? key).replace(/\{([^}]+)\}/g, (_, name) => String(values[name]))
  return { ...engine, primitives, styles, t }
}

function walk(root, output = []) {
  if (root === null || root === undefined || typeof root !== 'object') return output
  if (Array.isArray(root)) { for (const value of root) walk(value, output); return output }
  if ('kind' in root) output.push(root)
  for (const child of root.children ?? []) walk(child, output)
  return output
}

export function toolRender(bench, component, props) { return bench.render(component, props) }
export function toolFind(root, key, value) {
  return walk(root).find(node => value === undefined ? key in node.props : Object.is(node.props[key], value))
}
export function toolFindKind(root, kind) { return walk(root).find(node => node.kind === kind) }
export function toolCount(root, key) { return walk(root).filter(node => key in node.props).length }
export function toolText(root) {
  const parts = []
  const visit = value => {
    if (typeof value === 'string' || typeof value === 'number') parts.push(String(value))
    else if (Array.isArray(value)) value.forEach(visit)
    else if (value && typeof value === 'object') (value.children ?? []).forEach(visit)
  }
  visit(root)
  return parts.join('')
}
export function toolProp(value, key) { return value?.props?.[key] }
export function toolProperty(value, key) { return value?.[key] }
export function toolClick(value) { value.props.onClick({ stopPropagation() { this.stopped = true } }) }
export function toolKey(value, key) { const event = { key, prevented: false, stopped: false, preventDefault() { this.prevented = true }, stopPropagation() { this.stopped = true } }; value.props.onKeyDown(event); return event }
export function toolCall(value, ...args) { return value(...args) }
export function toolStyles(bench) { return bench.styles }
export function toolReset(bench) { bench.reset() }

export function makeGenericProps(bench, block, cwd) {
  const opened = [], inspected = []
  return { callId: block.callId, toolName: 'kind' in block ? (block.call?.name ?? '') : block.name,
    block, cwd, openFile: path => opened.push(path), inspect: () => inspected.push(block.callId),
    t: bench.t, sessionId: 's1', useSessions: selector => selector({ byId: { s1: { cwd } } }),
    opened, inspected }
}

export function makeTreeProps(bench, block, selectedCallId) {
  const owners = [], keys = [], inspected = []
  const props = {
    node: { key: `tool:${block.callId}`, kind: 'tool-call', data: { root: block } },
    selectedCallId, cwd: '/workspace', openFile() {}, inspectCall(id) { inspected.push(id) }, t: bench.t,
    renderSlot(name, owner, options) { keys.push([name, options.entryKey]); owners.push(owner); return options.fallback },
    owners, keys, inspected,
  }
  return props
}

export function makeApplyBench() {
  const entries = [], plugins = [], injections = [], disposers = []
  const declared = new Set(['conversation.chat.node', 'conversation.details.tool'])
  const slots = {
    register(options, component) {
      const row = { options, component, disposed: false }
      entries.push(row)
      for (const key of Object.keys(options.children ?? {})) declared.add(key)
      const dispose = () => {
        row.disposed = true
        const at = entries.indexOf(row)
        if (at >= 0) entries.splice(at, 1)
        for (const key of Object.keys(options.children ?? {})) declared.delete(key)
      }
      disposers.push(dispose)
      return dispose
    },
    inject(name, install) {
      injections.push(name)
      if (!declared.has(name)) throw new Error(`undeclared ${name}`)
      const installed = install()
      const owned = typeof installed === 'function' ? [installed] : [...installed]
      const dispose = () => { for (const off of [...owned].reverse()) off() }
      disposers.push(dispose)
      return dispose
    },
  }
  const ctx = { slots, plugin(plugin) { plugins.push(plugin); plugin.apply(ctx); return () => {} } }
  return { ctx, entries, plugins, injections, dispose() { for (const off of [...disposers].reverse()) off() } }
}
export function applyEntries(bench, name) { return bench.entries.filter(row => row.options.name === name) }
export function applyPluginNames(bench) { return bench.plugins.map(plugin => plugin.name) }
export function applyInjections(bench) { return bench.injections }
export function applyDispose(bench) { bench.dispose() }
"#)]
extern "C" {
    fn makeToolHarness() -> JsValue;
    fn toolRender(bench: &JsValue, component: &JsValue, props: &JsValue) -> JsValue;
    fn toolFind(root: &JsValue, key: &str, value: &JsValue) -> JsValue;
    fn toolFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn toolCount(root: &JsValue, key: &str) -> u32;
    fn toolText(root: &JsValue) -> String;
    fn toolProp(value: &JsValue, key: &str) -> JsValue;
    fn toolProperty(value: &JsValue, key: &str) -> JsValue;
    fn toolClick(value: &JsValue);
    fn toolKey(value: &JsValue, key: &str) -> JsValue;
    #[wasm_bindgen(variadic)]
    fn toolCall(value: &JsValue, arguments: &Array) -> JsValue;
    fn toolStyles(bench: &JsValue) -> Array;
    fn toolReset(bench: &JsValue);
    fn makeGenericProps(bench: &JsValue, block: &JsValue, cwd: &JsValue) -> JsValue;
    fn makeTreeProps(bench: &JsValue, block: &JsValue, selected: &JsValue) -> JsValue;
    fn makeApplyBench() -> JsValue;
    fn applyEntries(bench: &JsValue, name: &str) -> Array;
    fn applyPluginNames(bench: &JsValue) -> Array;
    fn applyInjections(bench: &JsValue) -> Array;
    fn applyDispose(bench: &JsValue);
}

fn set(target: &Object, key: &str, value: &JsValue) {
    Reflect::set(target, &JsValue::from_str(key), value).unwrap();
}

fn property(target: &JsValue, key: &str) -> JsValue {
    Reflect::get(target, &JsValue::from_str(key)).unwrap()
}

fn parse(value: &str) -> JsValue {
    JSON::parse(value).unwrap()
}

fn running(name: &str, args: &str, call_view: &str) -> JsValue {
    let value = Object::new();
    set(&value, "callId", &JsValue::from_str("c1"));
    set(&value, "name", &JsValue::from_str(name));
    set(&value, "argsRaw", &JsValue::from_str(args));
    set(&value, "turn", &JsValue::from_f64(1.0));
    set(&value, "step", &JsValue::from_f64(1.0));
    set(&value, "time", &JsValue::from_f64(1_000.0));
    set(
        &value,
        "callView",
        &if call_view.is_empty() {
            JsValue::NULL
        } else {
            parse(call_view)
        },
    );
    set(&value, "subCalls", &Array::new().into());
    value.into()
}

fn settled(
    name: Option<&str>,
    args: &str,
    content: &str,
    is_error: bool,
    call_view: &str,
    result_view: &str,
    error: Option<(&str, &str)>,
) -> JsValue {
    let value = Object::new();
    set(&value, "kind", &JsValue::from_str("tool-result"));
    set(&value, "seq", &JsValue::from_f64(2.0));
    set(&value, "time", &JsValue::from_f64(2_000.0));
    set(&value, "callId", &JsValue::from_str("c1"));
    let call = name.map_or(JsValue::NULL, |name| {
        let call = Object::new();
        set(&call, "name", &JsValue::from_str(name));
        set(&call, "argsRaw", &JsValue::from_str(args));
        call.into()
    });
    set(&value, "call", &call);
    set(&value, "callTime", &JsValue::from_f64(1_000.0));
    let blocks = Array::new();
    if !content.is_empty() {
        let block = Object::new();
        set(&block, "type", &JsValue::from_str("text"));
        set(&block, "text", &JsValue::from_str(content));
        blocks.push(&block);
    }
    set(&value, "content", &blocks.into());
    set(&value, "isError", &JsValue::from_bool(is_error));
    set(
        &value,
        "callView",
        &if call_view.is_empty() {
            JsValue::NULL
        } else {
            parse(call_view)
        },
    );
    set(
        &value,
        "resultView",
        &if result_view.is_empty() {
            JsValue::NULL
        } else {
            parse(result_view)
        },
    );
    if let Some((name, code)) = error {
        let row = Object::new();
        set(&row, "name", &JsValue::from_str(name));
        set(&row, "code", &JsValue::from_str(code));
        set(&value, "error", &row.into());
    }
    set(&value, "subCalls", &Array::new().into());
    value.into()
}

fn configure() -> JsValue {
    let bench = makeToolHarness();
    configure_client_ui_tool(property(&bench, "React"), property(&bench, "primitives")).unwrap();
    bench
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One renderer ledger covers the complete built-in branch matrix.
fn compiled_rows_cover_state_cards_copy_and_specialized_summaries() {
    let bench = configure();
    assert_eq!(toolStyles(&bench).length(), 4);
    let terminal = settled(
        Some("bash"),
        r#"{"command":"ls -la","description":"List files"}"#,
        "a.ts  b.ts\n",
        false,
        r#"{"card":"terminal","title":"ls -la","description":"List files"}"#,
        r#"{"card":"terminal","output":"a.ts  b.ts\n","exitCode":0}"#,
        None,
    );
    let props = makeGenericProps(&bench, &terminal, &JsValue::from_str("/workspace"));
    let generic = generic_tool_card_component().unwrap();
    let collapsed = toolRender(&bench, &generic, &props);
    assert!(toolText(&collapsed).contains("Bash"));
    assert!(toolText(&collapsed).contains("List files"));
    assert!(toolFind(&collapsed, "data-terminal", &JsValue::TRUE).is_undefined());
    let disclosure = toolFind(&collapsed, "data-expandable", &JsValue::TRUE);
    assert_eq!(toolProp(&disclosure, "aria-expanded"), JsValue::FALSE);
    toolClick(&disclosure);
    let expanded = toolRender(&bench, &generic, &props);
    let card = toolFind(&expanded, "data-terminal", &JsValue::TRUE);
    assert!(!card.is_undefined());
    assert_eq!(
        toolProp(&card, "command").as_string().as_deref(),
        Some("ls -la")
    );
    assert_eq!(
        toolProp(&card, "cwd").as_string().as_deref(),
        Some("/workspace")
    );
    assert_eq!(toolProp(&card, "maxLines").as_f64(), Some(f64::INFINITY));
    let labels = toolProp(&card, "labels");
    let exit_label = property(&labels, "exitCode")
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(
        exit_label
            .call1(&JsValue::UNDEFINED, &JsValue::from_f64(2.0))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("exit 2")
    );
    let inspect = toolFindKind(&expanded, "button");
    assert!(toolText(&expanded).contains("Inspect"));
    toolClick(&inspect);
    assert_eq!(
        property(&props, "inspected")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        1
    );

    let failed = settled(
        Some("bash"),
        r#"{"command":"false","description":"Fail"}"#,
        "boom\ndetail",
        false,
        r#"{"card":"terminal","title":"false","description":"Fail"}"#,
        r#"{"card":"terminal","output":"boom\ndetail","exitCode":2}"#,
        None,
    );
    let failed_props = makeGenericProps(&bench, &failed, &JsValue::UNDEFINED);
    let failed_tree = toolRender(&bench, &generic, &failed_props);
    assert_eq!(
        toolProp(
            &toolFind(&failed_tree, "data-state", &JsValue::from_str("error")),
            "data-state"
        )
        .as_string()
        .as_deref(),
        Some("error")
    );

    toolReset(&bench);
    let ask = ask_question_row_component().unwrap();
    let ask_running = running("ask_user_question", r#"{"questions":[{"id":"a"}]}"#, "");
    let ask_props = makeGenericProps(&bench, &ask_running, &JsValue::UNDEFINED);
    let ask_tree = toolRender(&bench, &ask, &ask_props);
    assert!(toolText(&ask_tree).contains("Ask question"));
    assert!(toolText(&ask_tree).contains("waiting"));
    let ask_done = settled(
        Some("ask_user_question"),
        r#"{"questions":[{"id":"a"},{"id":"b"},{"id":"c"}]}"#,
        r#"{"answers":[{"selected":["x"]},{"selected":[],"custom":"free"},{"selected":[]}]}"#,
        false,
        "",
        "",
        None,
    );
    let ask_done_props = makeGenericProps(&bench, &ask_done, &JsValue::UNDEFINED);
    assert!(toolText(&toolRender(&bench, &ask, &ask_done_props)).contains("2/3 answered"));
    let ask_cancelled = settled(
        Some("ask_user_question"),
        "{}",
        "",
        true,
        "",
        "",
        Some(("UserQuestionError", "ASK_CANCELLED")),
    );
    let ask_cancelled_props = makeGenericProps(&bench, &ask_cancelled, &JsValue::UNDEFINED);
    assert!(toolText(&toolRender(&bench, &ask, &ask_cancelled_props)).contains("cancelled"));

    toolReset(&bench);
    let todo = running(
        "todo_write",
        r#"{"todos":[{"content":"done","status":"completed"},{"content":"first","status":"in_progress"},{"content":"second","status":"in_progress"}]}"#,
        "",
    );
    let todo_props = makeGenericProps(&bench, &todo, &JsValue::UNDEFINED);
    let todo_tree = toolRender(&bench, &todo_row_component().unwrap(), &todo_props);
    assert!(toolText(&todo_tree).contains("1/3 completed · first"));
    assert!(toolText(&todo_tree).contains("+1"));

    toolReset(&bench);
    let search = settled(
        Some("grep"),
        r#"{"pattern":"needle"}"#,
        "Full result at spill://grep",
        false,
        "",
        r#"{"card":"search","shape":"matches","files":[{"path":"a.rs","matches":[{"lineNumber":4,"line":"needle"}]}],"truncated":true,"total":10,"title":"10 matches"}"#,
        None,
    );
    let search_props = makeGenericProps(&bench, &search, &JsValue::UNDEFINED);
    let search_component = search_row_component().unwrap();
    let search_collapsed = toolRender(&bench, &search_component, &search_props);
    assert!(toolText(&search_collapsed).contains("10 matches"));
    toolClick(&toolFind(
        &search_collapsed,
        "data-expandable",
        &JsValue::TRUE,
    ));
    let search_expanded = toolRender(&bench, &search_component, &search_props);
    assert!(!toolFind(&search_expanded, "data-search", &JsValue::TRUE).is_undefined());
    assert!(toolText(&search_expanded).contains("Full result at spill://grep"));

    toolReset(&bench);
    let read = settled(
        Some("read"),
        r#"{"path":"/workspace/src/lib.rs"}"#,
        "raw",
        false,
        "",
        r#"{"card":"read","path":"/workspace/src/lib.rs","lines":[{"number":1,"text":"fn main() {}"}],"totalLines":1,"lang":"rust"}"#,
        None,
    );
    let read_props = makeGenericProps(&bench, &read, &JsValue::from_str("/workspace"));
    let read_component = read_row_component().unwrap();
    let read_collapsed = toolRender(&bench, &read_component, &read_props);
    assert!(toolText(&read_collapsed).contains("src/lib.rs"));
    toolClick(&toolFind(
        &read_collapsed,
        "data-expandable",
        &JsValue::TRUE,
    ));
    assert!(
        !toolFind(
            &toolRender(&bench, &read_component, &read_props),
            "data-read",
            &JsValue::TRUE
        )
        .is_undefined()
    );

    toolReset(&bench);
    let mutation = settled(
        Some("edit"),
        r#"{"file_path":"src/lib.rs"}"#,
        "updated",
        false,
        r#"{"card":"diff","diffs":[{"path":"src/lib.rs","oldText":"a","newText":"b"}]}"#,
        r#"{"card":"diff","diffs":[{"path":"src/lib.rs","oldText":"a","newText":"b"}]}"#,
        None,
    );
    let mutation_props = makeGenericProps(&bench, &mutation, &JsValue::from_str("/workspace"));
    let mutation_component = file_mutation_row_component().unwrap();
    let mutation_collapsed = toolRender(&bench, &mutation_component, &mutation_props);
    toolClick(&toolFind(
        &mutation_collapsed,
        "data-expandable",
        &JsValue::TRUE,
    ));
    assert!(
        !toolFind(
            &toolRender(&bench, &mutation_component, &mutation_props),
            "data-diff",
            &JsValue::TRUE
        )
        .is_undefined()
    );

    toolReset(&bench);
    let web = settled(
        Some("web_fetch"),
        r#"{"url":"https://example.test"}"#,
        "body",
        false,
        "",
        r#"{"card":"web","kind":"fetch","url":"https://example.test","statusCode":200,"truncated":false}"#,
        None,
    );
    let web_props = makeGenericProps(&bench, &web, &JsValue::UNDEFINED);
    let web_component = web_row_component().unwrap();
    let web_collapsed = toolRender(&bench, &web_component, &web_props);
    toolClick(&toolFind(&web_collapsed, "data-expandable", &JsValue::TRUE));
    assert!(
        !toolFind(
            &toolRender(&bench, &web_component, &web_props),
            "data-web",
            &JsValue::TRUE
        )
        .is_undefined()
    );

    toolReset(&bench);
    let bash_props = makeGenericProps(&bench, &terminal, &JsValue::from_str("/workspace"));
    let bash_tree = toolRender(&bench, &bash_row_component().unwrap(), &bash_props);
    assert!(!toolFind(&bash_tree, "data-sample", &JsValue::from_str("bash")).is_undefined());
}

#[wasm_bindgen_test]
fn compiled_tool_row_preserves_file_gesture_and_keyboard_expansion() {
    let bench = configure();
    let row = tool_row_component().unwrap();
    let open_calls = Array::new();
    let recorded_calls = open_calls.clone();
    let open_callback = Closure::wrap(Box::new(move |path: String| {
        recorded_calls.push(&JsValue::from_str(&path));
    }) as Box<dyn FnMut(String)>);
    let props = Object::new();
    set(&props, "t", &property(&bench, "t"));
    set(&props, "variant", &JsValue::from_str("read"));
    set(&props, "toolName", &JsValue::from_str("read"));
    set(&props, "icon", &JsValue::from_str("icon"));
    set(&props, "title", &JsValue::from_str("Read"));
    set(&props, "summary", &JsValue::from_str("src/a.rs"));
    set(&props, "body", &JsValue::from_str("input"));
    set(&props, "state", &JsValue::from_str("ok"));
    set(&props, "filePath", &JsValue::from_str("src/a.rs"));
    set(&props, "onOpenFile", &open_callback.into_js_value());
    let collapsed = toolRender(&bench, &row, props.as_ref());
    let file = toolFindKind(&collapsed, "button");
    toolClick(&file);
    assert_eq!(open_calls.get(0).as_string().as_deref(), Some("src/a.rs"));
    let disclosure = toolFind(&collapsed, "data-expandable", &JsValue::TRUE);
    let ignored = toolKey(&disclosure, "Tab");
    assert_eq!(property(&ignored, "prevented"), JsValue::FALSE);
    let enter = toolKey(&disclosure, "Enter");
    assert_eq!(property(&enter, "prevented"), JsValue::TRUE);
    let expanded = toolRender(&bench, &row, props.as_ref());
    assert_eq!(
        toolProp(
            &toolFind(&expanded, "data-expandable", &JsValue::TRUE),
            "aria-expanded"
        ),
        JsValue::TRUE
    );
    assert!(toolText(&expanded).contains("INinput"));
}

#[wasm_bindgen_test]
fn compiled_tree_keeps_recursive_dispatch_original_blocks_and_memoized_owners() {
    let bench = configure();
    let root = settled(
        Some("run_code"),
        r#"{"code":"return 1"}"#,
        "",
        false,
        "",
        "",
        None,
    );
    let child = settled(Some("read"), r#"{"path":"a.rs"}"#, "", false, "", "", None);
    Reflect::set(
        &child,
        &JsValue::from_str("callId"),
        &JsValue::from_str("child"),
    )
    .unwrap();
    let children = Array::of1(&child);
    Reflect::set(&root, &JsValue::from_str("subCalls"), &children.into()).unwrap();
    let props = makeTreeProps(&bench, &root, &JsValue::from_str("child"));
    let tree = tool_call_tree_component().unwrap();
    let first = toolRender(&bench, &tree, &props);
    assert_eq!(toolCount(&first, "data-chat-call-id"), 2);
    assert_eq!(toolCount(&first, "data-subcalls"), 1);
    let selected = toolFind(&first, "data-selected", &JsValue::TRUE);
    assert_eq!(
        toolProp(&selected, "data-chat-call-id")
            .as_string()
            .as_deref(),
        Some("child")
    );
    let owners = property(&props, "owners").dyn_into::<Array>().unwrap();
    assert!(Object::is(&property(&owners.get(0), "block"), &root));
    assert!(Object::is(&property(&owners.get(1), "block"), &child));
    let first_owner = owners.get(0);
    toolRender(&bench, &tree, &props);
    let owners = property(&props, "owners").dyn_into::<Array>().unwrap();
    assert!(Object::is(&first_owner, &owners.get(2)));
    let inspect = property(&first_owner, "inspect");
    toolCall(&inspect, &Array::new());
    assert_eq!(
        property(&props, "inspected")
            .dyn_into::<Array>()
            .unwrap()
            .get(0)
            .as_string()
            .as_deref(),
        Some("c1")
    );
    let keys = property(&props, "keys").dyn_into::<Array>().unwrap();
    assert_eq!(
        property(&keys.get(0), "1").as_string().as_deref(),
        Some("run_code")
    );
    assert_eq!(
        property(&keys.get(1), "1").as_string().as_deref(),
        Some("read")
    );
}

#[wasm_bindgen_test]
fn compiled_details_selects_structured_cards_and_generic_fallbacks() {
    let bench = configure();
    let details = tool_details_component().unwrap();
    let running_call = running("mystery", "{}", "");
    let running_props = makeGenericProps(&bench, &running_call, &JsValue::UNDEFINED);
    assert!(toolText(&toolRender(&bench, &details, &running_props)).contains("Running…"));

    let diff = settled(
        Some("edit"),
        "{}",
        "updated",
        false,
        "",
        r#"{"card":"diff","diffs":[{"path":"a","oldText":null,"newText":"b"}]}"#,
        None,
    );
    let diff_props = makeGenericProps(&bench, &diff, &JsValue::UNDEFINED);
    assert!(
        !toolFind(
            &toolRender(&bench, &details, &diff_props),
            "data-diff",
            &JsValue::TRUE
        )
        .is_undefined()
    );

    let web = settled(
        Some("web_fetch"),
        "{}",
        "raw fetched body",
        false,
        "",
        r#"{"card":"web","kind":"fetch","url":"https://example.test","statusCode":200,"truncated":false}"#,
        None,
    );
    let web_props = makeGenericProps(&bench, &web, &JsValue::UNDEFINED);
    let web_tree = toolRender(&bench, &details, &web_props);
    assert!(!toolFind(&web_tree, "data-web", &JsValue::TRUE).is_undefined());
    assert!(toolText(&web_tree).contains("raw fetched body"));

    let failed = settled(Some("x"), "{}", "boom", true, "", "", None);
    let failed_props = makeGenericProps(&bench, &failed, &JsValue::UNDEFINED);
    let failed_tree = toolRender(&bench, &details, &failed_props);
    let pre = toolFindKind(&failed_tree, "pre");
    assert_eq!(toolProp(&pre, "data-error"), JsValue::TRUE);
    assert_eq!(toolText(&failed_tree), "boom");
}

#[wasm_bindgen_test]
fn compiled_apply_registers_every_surface_and_disposes_contributions() {
    configure();
    let inject = tool_inject_browser();
    assert_eq!(inject.length(), 1);
    assert_eq!(inject.get(0).as_string().as_deref(), Some("slots"));
    let bench = makeApplyBench();
    apply_client_ui_tool(property(&bench, "ctx")).unwrap();
    let chat = applyEntries(&bench, "conversation.chat.node");
    assert_eq!(chat.length(), 1);
    assert_eq!(
        property(&property(&chat.get(0), "options"), "key")
            .as_string()
            .as_deref(),
        Some("tool-call")
    );
    assert_eq!(
        applyEntries(&bench, "conversation.details.tool").length(),
        1
    );
    let atomic = applyEntries(&bench, "tool.call.toolview");
    assert_eq!(atomic.length(), 10);
    let mut keys = (0..atomic.length())
        .map(|index| {
            property(&property(&atomic.get(index), "options"), "key")
                .as_string()
                .unwrap()
        })
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        [
            "ask_user_question",
            "bash",
            "edit",
            "glob",
            "grep",
            "read",
            "todo_write",
            "web_fetch",
            "web_search",
            "write",
        ]
    );
    let plugins = applyPluginNames(&bench);
    assert_eq!(plugins.length(), 7);
    let injections = applyInjections(&bench);
    assert_eq!(injections.length(), 9);
    applyDispose(&bench);
    assert_eq!(applyEntries(&bench, "conversation.chat.node").length(), 0);
    assert_eq!(
        applyEntries(&bench, "conversation.details.tool").length(),
        0
    );
    assert_eq!(applyEntries(&bench, "tool.call.toolview").length(), 0);
}
