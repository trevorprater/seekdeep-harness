//! Live WASM execution for locale service, Store, Slot, and Language-row assembly.

#![cfg(target_arch = "wasm32")]

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_locale::{apply_client_locale, configure_client_locale, locale_inject};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn apply_provides_locale_and_registers_language_row_through_declared_slot() {
    let global = js_sys::global();
    let styles = Rc::new(RefCell::new(0_usize));
    let document = fake_document(styles.clone());
    set(&global, "document", &document);
    let (react, open_state) = fake_react();
    configure_client_locale(react, fake_primitives(), fake_runtime()).unwrap();

    let registered = Rc::new(RefCell::new(None::<(JsValue, JsValue)>));
    let slots = Object::new();
    let install_locale = Closure::wrap(Box::new(|_locale: JsValue| -> Function { disposer() })
        as Box<dyn FnMut(JsValue) -> Function>);
    set(&slots, "installLocale", &install_locale.into_js_value());
    let register_capture = registered.clone();
    let register = Closure::wrap(
        Box::new(move |options: JsValue, component: JsValue| -> Function {
            *register_capture.borrow_mut() = Some((options, component));
            disposer()
        }) as Box<dyn FnMut(JsValue, JsValue) -> Function>,
    );
    set(&slots, "register", &register.into_js_value());
    let inject = Closure::wrap(
        Box::new(move |_name: String, factory: Function| -> Function {
            factory.call0(&JsValue::UNDEFINED).unwrap();
            disposer()
        }) as Box<dyn FnMut(String, Function) -> Function>,
    );
    set(&slots, "inject", &inject.into_js_value());

    let scope = Object::new();
    let get_snapshot = Closure::wrap(Box::new(|| -> JsValue {
        let snapshot = Object::new();
        set(&snapshot, "value", &Object::new());
        snapshot.into()
    }) as Box<dyn FnMut() -> JsValue>);
    set(&scope, "getSnapshot", &get_snapshot.into_js_value());
    let subscribe = Closure::wrap(Box::new(|_listener: Function| -> Function { disposer() })
        as Box<dyn FnMut(Function) -> Function>);
    set(&scope, "subscribe", &subscribe.into_js_value());
    let writes = Rc::new(RefCell::new(Vec::<String>::new()));
    let write_capture = writes.clone();
    let set_preference = Closure::wrap(Box::new(move |_field: String, value: String| {
        write_capture.borrow_mut().push(value);
    }) as Box<dyn FnMut(String, String)>);
    set(&scope, "set", &set_preference.into_js_value());
    let settings_scope = Object::new();
    let bound_scope = scope.clone();
    let bind =
        Closure::wrap(
            Box::new(move |_options: JsValue| -> JsValue { bound_scope.clone().into() })
                as Box<dyn FnMut(JsValue) -> JsValue>,
        );
    set(&settings_scope, "bind", &bind.into_js_value());

    let services = Object::new();
    set(&services, "slots", &slots);
    set(&services, "settingsScope", &settings_scope);
    set(&services, "connection", &Object::new());
    set(&services, "remote", &Object::new());
    let ctx = Object::new();
    let get_services = services;
    let get_service = Closure::wrap(Box::new(move |name: String| -> JsValue {
        Reflect::get(&get_services, &JsValue::from_str(&name)).unwrap()
    }) as Box<dyn FnMut(String) -> JsValue>);
    set(&ctx, "get", &get_service.into_js_value());
    let provided = Rc::new(RefCell::new(None::<JsValue>));
    let provided_capture = provided.clone();
    let provide = Closure::wrap(Box::new(move |name: String, value: JsValue| {
        if name == "locale" {
            *provided_capture.borrow_mut() = Some(value);
        }
    }) as Box<dyn FnMut(String, JsValue)>);
    set(&ctx, "provide", &provide.into_js_value());
    let event_listener = Rc::new(RefCell::new(None::<Function>));
    let listener_capture = event_listener.clone();
    let on = Closure::wrap(
        Box::new(move |name: String, listener: Function| -> Function {
            if name == "locale/change" {
                *listener_capture.borrow_mut() = Some(listener);
            }
            disposer()
        }) as Box<dyn FnMut(String, Function) -> Function>,
    );
    set(&ctx, "on", &on.into_js_value());
    let event_dispatch = event_listener;
    let emit = Closure::wrap(Box::new(move |_name: String, value: JsValue| {
        if let Some(listener) = event_dispatch.borrow().as_ref() {
            listener.call1(&JsValue::UNDEFINED, &value).unwrap();
        }
    }) as Box<dyn FnMut(String, JsValue)>);
    set(&ctx, "emit", &emit.into_js_value());
    let effect = Closure::wrap(Box::new(|installer: Function, _label: String| -> JsValue {
        installer.call0(&JsValue::UNDEFINED).unwrap()
    }) as Box<dyn FnMut(Function, String) -> JsValue>);
    set(&ctx, "effect", &effect.into_js_value());

    apply_client_locale(ctx.into()).unwrap();
    assert_eq!(
        locale_inject().join(","),
        "slots,connection,remote,settingsScope"
    );
    assert_eq!(*styles.borrow(), 1);
    let locale = provided.borrow().clone().unwrap();
    let snapshot = call(&locale, "getSnapshot", &[]);
    assert!(Object::is_frozen(&Object::from(snapshot.clone())));
    assert_eq!(get(&snapshot, "active").as_string().unwrap(), "zh");
    let t = call(&locale, "bind", &[JsValue::from_str("settings.locale")])
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(
        t.call1(&JsValue::UNDEFINED, &JsValue::from_str("language.title"))
            .unwrap()
            .as_string()
            .unwrap(),
        "语言"
    );

    let (options, component) = registered.borrow().clone().unwrap();
    assert_eq!(get(&options, "id").as_string().unwrap(), "language");
    assert_eq!(
        get(&options, "locale").as_string().unwrap(),
        "settings.locale"
    );
    let mirror = Rc::new(RefCell::new(None::<JsValue>));
    let mirror_capture = mirror.clone();
    let actions = Object::new();
    let sync = Closure::wrap(
        Box::new(move |active: String, locales: JsValue, revision: f64| {
            let value = Object::new();
            set(&value, "active", &JsValue::from_str(&active));
            set(&value, "options", &locales);
            set(&value, "revision", &JsValue::from_f64(revision));
            *mirror_capture.borrow_mut() = Some(value.into());
        }) as Box<dyn FnMut(String, JsValue, f64)>,
    );
    set(&actions, "sync", &sync.into_js_value());
    let face = get(&options, "inject")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &actions)
        .unwrap();
    assert_eq!(
        get(&mirror.borrow().clone().unwrap(), "active")
            .as_string()
            .unwrap(),
        "zh"
    );
    assert!(!Object::is(
        &get(&mirror.borrow().clone().unwrap(), "options"),
        &get(&snapshot, "locales")
    ));
    call(&face, "setLocale", &[JsValue::from_str("en")]);
    assert_eq!(writes.borrow().as_slice(), ["en"]);

    let props = Object::new();
    let mirror_for_hook = mirror.clone();
    let use_store = Closure::wrap(Box::new(move |selector: Function| -> JsValue {
        selector
            .call1(
                &JsValue::UNDEFINED,
                &mirror_for_hook.borrow().clone().unwrap(),
            )
            .unwrap()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    set(&props, "useStore", &use_store.into_js_value());
    let translate = Closure::wrap(Box::new(|_key: String| -> String { "Language".into() })
        as Box<dyn FnMut(String) -> String>);
    set(&props, "t", &translate.into_js_value());
    let selected = Rc::new(RefCell::new(None::<String>));
    let selected_capture = selected.clone();
    let set_locale = Closure::wrap(Box::new(move |id: String| {
        *selected_capture.borrow_mut() = Some(id);
    }) as Box<dyn FnMut(String)>);
    set(&props, "setLocale", &set_locale.into_js_value());
    let tree = component
        .clone()
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &props)
        .unwrap();
    assert_eq!(get(&tree, "kind").as_string().unwrap(), "div");
    let children = Array::from(&get(&tree, "children"));
    let menu = children.get(1);
    assert_eq!(get(&menu, "kind").as_string().unwrap(), "Menu");
    let menu_props = get(&menu, "props");
    assert_eq!(get(&menu_props, "selectedId").as_string().unwrap(), "en");
    let anchor = get(&menu_props, "anchor");
    let anchor_props = get(&anchor, "props");
    assert_eq!(get(&anchor_props, "aria-expanded").as_bool(), Some(false));
    get(&anchor_props, "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert!(open_state.get());
    let opened = component
        .clone()
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &props)
        .unwrap();
    let opened_menu = Array::from(&get(&opened, "children")).get(1);
    let opened_props = get(&opened_menu, "props");
    assert_eq!(get(&opened_props, "open").as_bool(), Some(true));
    get(&opened_props, "onClose")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert!(!open_state.get());
    get(&menu_props, "onSelect")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("zh"))
        .unwrap();
    assert_eq!(selected.borrow().as_deref(), Some("zh"));
    set(
        &Object::from(mirror.borrow().clone().unwrap()),
        "active",
        &JsValue::from_str("fr"),
    );
    let unknown = component
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &props)
        .unwrap();
    let unknown_menu = Array::from(&get(&unknown, "children")).get(1);
    let unknown_anchor = get(&get(&unknown_menu, "props"), "anchor");
    assert_eq!(
        Array::from(&get(&unknown_anchor, "children"))
            .get(0)
            .as_string()
            .as_deref(),
        Some("fr")
    );
    Reflect::delete_property(&global, &JsValue::from_str("document")).unwrap();
}

fn fake_document(styles: Rc<RefCell<usize>>) -> JsValue {
    let document = Object::new();
    let create = Closure::wrap(Box::new(|_kind: String| -> JsValue {
        let style = Object::new();
        let attribute = Closure::wrap(
            Box::new(|_name: String, _value: String| {}) as Box<dyn FnMut(String, String)>
        );
        set(&style, "setAttribute", &attribute.into_js_value());
        style.into()
    }) as Box<dyn FnMut(String) -> JsValue>);
    set(&document, "createElement", &create.into_js_value());
    let head = Object::new();
    let append = Closure::wrap(Box::new(move |_style: JsValue| {
        *styles.borrow_mut() += 1;
    }) as Box<dyn FnMut(JsValue)>);
    set(&head, "appendChild", &append.into_js_value());
    set(&document, "head", &head);
    document.into()
}

fn fake_react() -> (JsValue, Rc<Cell<bool>>) {
    let react = Object::new();
    let create = Function::new_with_args(
        "kind,props",
        "return {kind,props:props||{},children:Array.prototype.slice.call(arguments,2)}",
    );
    set(&react, "createElement", &create);
    let open = Rc::new(Cell::new(false));
    let state_open = open.clone();
    let state = Closure::wrap(Box::new(move |_initial: JsValue| -> Array {
        let values = Array::new();
        values.push(&JsValue::from_bool(state_open.get()));
        let setter_state = state_open.clone();
        let setter = Closure::wrap(Box::new(move |next: JsValue| {
            let value = next
                .dyn_ref::<Function>()
                .and_then(|function| {
                    function
                        .call1(&JsValue::UNDEFINED, &JsValue::from_bool(setter_state.get()))
                        .ok()
                })
                .unwrap_or(next)
                .as_bool()
                .unwrap_or(false);
            setter_state.set(value);
        }) as Box<dyn FnMut(JsValue)>);
        values.push(&setter.into_js_value());
        values
    }) as Box<dyn FnMut(JsValue) -> Array>);
    set(&react, "useState", &state.into_js_value());
    (react.into(), open)
}

fn fake_primitives() -> JsValue {
    let primitives = Object::new();
    set(&primitives, "Menu", &JsValue::from_str("Menu"));
    set(
        &primitives,
        "IconChevronDownOutline14",
        &JsValue::from_str("Chevron"),
    );
    primitives.into()
}

fn fake_runtime() -> JsValue {
    let runtime = Object::new();
    let define = Closure::wrap(Box::new(|declaration: JsValue| -> JsValue {
        let handle = Object::new();
        set(&handle, "declaration", &declaration);
        handle.into()
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set(&runtime, "defineStore", &define.into_js_value());
    runtime.into()
}

fn disposer() -> Function {
    Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn call(value: &JsValue, method: &str, arguments: &[JsValue]) -> JsValue {
    let function = get(value, method).dyn_into::<Function>().unwrap();
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function.apply(value, &args).unwrap()
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn set(value: &Object, key: &str, item: &JsValue) {
    Reflect::set(value, &JsValue::from_str(key), item).unwrap();
}
