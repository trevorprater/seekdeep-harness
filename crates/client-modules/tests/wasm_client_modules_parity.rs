//! Browser-executed Client module table, DOM transport, and enrollment parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_modules::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

fn reset_globals() {
    let global = Object::from(js_sys::global());
    for name in ["__ModuleLoader__", "__SEEKDEEP_MODULES__"] {
        let _ = Reflect::delete_property(&global, &JsValue::from_str(name));
    }
    Function::new_no_args(
        "document.querySelectorAll('style,script').forEach(node => node.remove())",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap();
}

fn row(id: &str) -> BootModuleRow {
    BootModuleRow {
        id: ClientModuleId::new(id),
        url: format!("/plugins/{id}/client.js?rev=0"),
        rev: "0".to_owned(),
    }
}

fn modules_value(rows: &[BootModuleRow]) -> JsValue {
    serde_wasm_bindgen::to_value(rows).unwrap()
}

fn scripted_transport() -> JsValue {
    Function::new_no_args(
        r#"
const bundles = new Map();
const fetched = [];
const gates = new Map();
const loadBundle = async url => {
  fetched.push(url);
  const id = /\/plugins\/(.+)\/client\.js/.exec(url)?.[1];
  const gate = id === undefined ? undefined : gates.get(id);
  if (gate !== undefined) await gate.promise;
  const factory = id === undefined ? undefined : bundles.get(id);
  if (factory !== null && factory !== undefined) globalThis.__ModuleLoader__.load({ id, factory });
};
return {
  loadBundle,
  fetched,
  set(id, factory) { bundles.set(id, factory); },
  gate(id) {
    let release;
    const promise = new Promise(resolve => { release = resolve; });
    gates.set(id, { promise, release });
  },
  release(id) { gates.get(id)?.release(); },
};
"#,
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap()
}

fn field<T: JsCast>(value: &JsValue, name: &str) -> T {
    Reflect::get(value, &JsValue::from_str(name))
        .unwrap()
        .dyn_into::<T>()
        .unwrap_or_else(|_| panic!("{name} has the wrong JavaScript type"))
}

#[wasm_bindgen_test]
fn boot_parser_and_enrollment_use_seekdeep_kernel_slots() {
    reset_globals();
    let manifest = parse_boot_manifest_js(
        Function::new_no_args(
            "return { rev: 'g', entries: [{ id: 'a', url: '/a.js', rev: '1', inject: ['x'], immediately: true }] };",
        )
        .call0(&JsValue::UNDEFINED)
        .unwrap(),
    )
    .unwrap();
    let modules: Array = field(&manifest, "modules");
    let plugins: Array = field(&manifest, "plugins");
    assert_eq!(modules.length(), 1);
    assert_eq!(
        Reflect::get(&plugins.get(0), &JsValue::from_str("immediately"))
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let error = parse_boot_manifest_js(JsValue::NULL).unwrap_err();
    assert!(
        Reflect::get(&error, &JsValue::from_str("message"))
            .unwrap()
            .as_string()
            .unwrap()
            .contains("__SEEKDEEP_BOOT__")
    );

    let plugin = client_modules_plugin().unwrap();
    let apply: Function = field(&plugin, "apply");
    let ctx = Function::new_no_args(
        "return { reflect: { provide(name, value) { globalThis.__providedModules = [name, value]; } } };",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    let missing = apply.call1(&plugin, &ctx).unwrap_err();
    assert!(
        Reflect::get(&missing, &JsValue::from_str("message"))
            .unwrap()
            .as_string()
            .unwrap()
            .contains("__SEEKDEEP_MODULES__ missing")
    );
    let marker = Object::new();
    Reflect::set(
        &js_sys::global(),
        &JsValue::from_str("__SEEKDEEP_MODULES__"),
        &marker,
    )
    .unwrap();
    apply.call1(&plugin, &ctx).unwrap();
    let provided = Reflect::get(&js_sys::global(), &JsValue::from_str("__providedModules"))
        .unwrap()
        .dyn_into::<Array>()
        .unwrap();
    assert_eq!(provided.get(0).as_string().as_deref(), Some("modules"));
    assert!(Object::is(&provided.get(1), &marker));
}

#[wasm_bindgen_test]
async fn lazy_table_executes_factories_requires_statics_invalidation_and_styles() {
    reset_globals();
    let transport = scripted_transport();
    let set_factory: Function = field(&transport, "set");
    set_factory
        .call2(
            &transport,
            &JsValue::from_str("b"),
            &Function::new_with_args("require", "return { helper: 'from-b' }"),
        )
        .unwrap();
    set_factory
        .call2(
            &transport,
            &JsValue::from_str("a"),
            &Function::new_with_args(
                "require",
                r#"
const dep = require('b/client');
const react = require('react');
const shell = require('app-shell');
const loose = document.createElement('style');
document.head.append(loose);
const tagged = document.createElement('style');
tagged.dataset.plugin = 'a';
tagged.dataset.pluginCss = 'sheet-1';
document.head.append(tagged);
return { got: dep.helper, react, shell };
"#,
            ),
        )
        .unwrap();
    let statics = Object::new();
    let react = Object::new();
    Reflect::set(&statics, &JsValue::from_str("react"), &react).unwrap();
    let system = WasmClientModuleSystem::new(
        modules_value(&[row("a"), row("b")]),
        statics.into(),
        Some(field(&transport, "loadBundle")),
    )
    .unwrap();
    let shell = Object::new();
    system
        .register_static("app-shell".to_owned(), shell.clone().into())
        .unwrap();
    system.prefetch("b".to_owned()).await.unwrap();
    assert_eq!(
        field::<Array>(&transport, "fetched").length(),
        1,
        "prefetch only registers"
    );
    assert_eq!(system.load_cache().size(), 0);
    let first = system
        .import_module("a".to_owned(), String::new(), Object::new().into())
        .await
        .unwrap();
    let second = system
        .import_module("a".to_owned(), String::new(), Object::new().into())
        .await
        .unwrap();
    assert!(Object::is(&first, &second));
    assert_eq!(
        Reflect::get(&first, &JsValue::from_str("got"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("from-b")
    );
    assert!(Object::is(
        &Reflect::get(&first, &JsValue::from_str("react")).unwrap(),
        &react
    ));
    assert!(Object::is(
        &Reflect::get(&first, &JsValue::from_str("shell")).unwrap(),
        &shell
    ));
    let record = system.load_cache().get(&JsValue::from_str("a"));
    let styles: Array = field(&record, "styles");
    assert_eq!(styles.length(), 2);
    assert_eq!(styles.get(0).as_string().as_deref(), Some("a"));
    assert_eq!(styles.get(1).as_string().as_deref(), Some("sheet-1"));
    let edges: js_sys::Set = field(&record, "edges");
    assert!(edges.has(&JsValue::from_str("b/client")));

    let generation = Function::new_no_args(
        "let generation = 0; return require => ({ generation: ++generation });",
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    system.invalidate("a".to_owned());
    set_factory
        .call2(&transport, &JsValue::from_str("a"), &generation)
        .unwrap();
    let reloaded = system
        .import_module("a".to_owned(), String::new(), Object::new().into())
        .await
        .unwrap();
    assert_eq!(
        Reflect::get(&reloaded, &JsValue::from_str("generation"))
            .unwrap()
            .as_f64(),
        Some(1.0)
    );
}

#[wasm_bindgen_test]
async fn concurrent_arrival_failures_cycles_and_double_boot_are_loud() {
    reset_globals();
    let transport = scripted_transport();
    let set_factory: Function = field(&transport, "set");
    set_factory
        .call2(
            &transport,
            &JsValue::from_str("a"),
            &Function::new_with_args("require", "return { marker: 'a' }"),
        )
        .unwrap();
    let gate: Function = field(&transport, "gate");
    gate.call1(&transport, &JsValue::from_str("a")).unwrap();
    let system = WasmClientModuleSystem::new(
        modules_value(&[row("a")]),
        Object::new().into(),
        Some(field(&transport, "loadBundle")),
    )
    .unwrap();
    let first = system.import_module("a".to_owned(), String::new(), Object::new().into());
    let second = system.import_module("a".to_owned(), String::new(), Object::new().into());
    let release: Function = field(&transport, "release");
    release.call1(&transport, &JsValue::from_str("a")).unwrap();
    let (first, second) = futures::future::join(first, second).await;
    assert!(Object::is(&first.unwrap(), &second.unwrap()));
    assert_eq!(field::<Array>(&transport, "fetched").length(), 1);

    let double_boot =
        match WasmClientModuleSystem::new(modules_value(&[]), Object::new().into(), None) {
            Ok(_) => panic!("double boot unexpectedly succeeded"),
            Err(error) => error,
        };
    assert!(
        Reflect::get(&double_boot, &JsValue::from_str("message"))
            .unwrap()
            .as_string()
            .unwrap()
            .contains("already installed (double boot?)")
    );
    reset_globals();
    let missing = scripted_transport();
    field::<Function>(&missing, "set")
        .call2(&missing, &JsValue::from_str("missing"), &JsValue::NULL)
        .unwrap();
    let missing_system = WasmClientModuleSystem::new(
        modules_value(&[row("missing")]),
        Object::new().into(),
        Some(field(&missing, "loadBundle")),
    )
    .unwrap();
    assert!(
        missing_system
            .import_module("missing".to_owned(), String::new(), Object::new().into())
            .await
            .unwrap_err()
            .is_object()
    );
}

#[wasm_bindgen_test]
async fn default_dom_transport_removes_success_and_failure_scripts() {
    reset_globals();
    let controls = Function::new_no_args(
        r#"
const original = document.head.appendChild.bind(document.head);
let fail = false;
document.head.appendChild = node => {
  const result = original(node);
  queueMicrotask(() => {
    if (fail) node.onerror?.(new Event('error'));
    else {
      globalThis.__ModuleLoader__.load({ id: 'dee', factory: () => ({ marker: 'via-script' }) });
      node.onload?.(new Event('load'));
    }
  });
  return result;
};
return {
  fail() { fail = true; },
  restore() { document.head.appendChild = original; },
};
"#,
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    let system =
        WasmClientModuleSystem::new(modules_value(&[row("dee")]), Object::new().into(), None)
            .unwrap();
    let value = system
        .import_module("dee".to_owned(), String::new(), Object::new().into())
        .await
        .unwrap();
    assert_eq!(
        Reflect::get(&value, &JsValue::from_str("marker"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("via-script")
    );
    assert_eq!(
        Function::new_no_args("return document.querySelectorAll('script').length")
            .call0(&JsValue::UNDEFINED)
            .unwrap()
            .as_f64(),
        Some(0.0)
    );
    field::<Function>(&controls, "restore")
        .call0(&controls)
        .unwrap();

    reset_globals();
    let controls = Function::new_no_args(
        r#"
const original = document.head.appendChild.bind(document.head);
document.head.appendChild = node => {
  const result = original(node);
  queueMicrotask(() => node.onerror?.(new Event('error')));
  return result;
};
return { restore() { document.head.appendChild = original; } };
"#,
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    let failed =
        WasmClientModuleSystem::new(modules_value(&[row("dee")]), Object::new().into(), None)
            .unwrap();
    let error = failed.prefetch("dee".to_owned()).await.unwrap_err();
    assert!(
        Reflect::get(&error, &JsValue::from_str("message"))
            .unwrap()
            .as_string()
            .unwrap()
            .contains("bundle script /plugins/dee/client.js?rev=0 failed to load")
    );
    assert_eq!(
        Function::new_no_args("return document.querySelectorAll('script').length")
            .call0(&JsValue::UNDEFINED)
            .unwrap()
            .as_f64(),
        Some(0.0)
    );
    field::<Function>(&controls, "restore")
        .call0(&controls)
        .unwrap();
}
