//! Complete Client Cordis UI assembly over browser services and Rust state.

use std::{
    any::Any,
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use js_sys::{Array, Function, Map, Object, Promise, Reflect, Set};
use parking_lot::Mutex;
use seekdeep_cordis_dynamic_types::{CordisDynamicPluginId, DynamicCordisInventoryRow};
use seekdeep_identity::SessionId;
use serde::Serialize;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    CORDIS_LOCALE_NAMESPACE, CordisInventory, CordisInventorySnapshot, CordisRunCardRegistry,
    CordisRunCardStore, UI_CORDIS_INJECT, chinese_locale, cordis_trigger_candidates,
    cordis_trigger_pick, cordis_ui_components, english_locale,
};

const PLUGIN_NAME: &str = "client-ui-cordis";
const STYLES: &str = include_str!("../data/styles.css");
type SnapshotCache<T> = Arc<Mutex<Option<(Arc<T>, JsValue)>>>;

/// Builds the Client plugin descriptor consumed by the browser Cordis Loader.
///
/// # Errors
///
/// Returns JavaScript descriptor-construction failures.
#[wasm_bindgen(js_name = clientUiCordisPlugin)]
#[allow(clippy::needless_pass_by_value)]
pub fn client_ui_cordis_plugin(react: JsValue, primitives: JsValue) -> Result<JsValue, JsValue> {
    let apply = Closure::wrap(Box::new(move |ctx: JsValue| -> Result<(), JsValue> {
        apply_client_ui_cordis(ctx, react.clone(), primitives.clone())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let plugin = Object::new();
    set(&plugin, "name", &JsValue::from_str(PLUGIN_NAME))?;
    let inject = Array::new();
    for dependency in UI_CORDIS_INJECT {
        inject.push(&JsValue::from_str(dependency));
    }
    set(&plugin, "inject", &inject.into())?;
    set(&plugin, "apply", &apply.into_js_value())?;
    Ok(plugin.into())
}

/// Mounts every Cordis browser card, panel, event, and `@pluginId` contribution.
///
/// # Errors
///
/// Returns missing-Service, registration, DOM, or JavaScript construction failures.
#[wasm_bindgen(js_name = applyClientUiCordis)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_cordis(
    ctx: JsValue,
    react: JsValue,
    primitives: JsValue,
) -> Result<(), JsValue> {
    let slots = required_service(&ctx, "slots")?;
    let locale = required_service(&ctx, "locale")?;
    let input_triggers = required_service(&ctx, "inputTriggers")?;
    let remote = required_service(&ctx, "remote")?;
    let namespace = required_service(&ctx, "remote.dynamicCordisRunner")?;
    let runner = required_service(&ctx, "dynamicCordisRunner")?;
    let components = cordis_ui_components(react, primitives)?;

    own_locale(&ctx, &locale)?;
    own_styles(&ctx)?;

    let inventory = CordisInventory::new();
    let inventory_js = inventory_observable(&inventory)?;
    let refresh = refresh_function(&inventory, &namespace);
    reconcile_on_inventory(&ctx, &inventory, &runner)?;
    subscribe_remote_events(&ctx, &remote, &inventory, &refresh)?;
    subscribe_connection_reset(&ctx, &inventory, &refresh)?;

    register_slots(
        &slots,
        &components,
        &inventory,
        &inventory_js,
        &runner,
        &namespace,
        &refresh,
    )?;
    register_input_source(&ctx, &input_triggers, &inventory, &inventory_js, &refresh)?;
    refresh.call0(&JsValue::UNDEFINED)?;
    Ok(())
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() {
        Err(js_sys::Error::new(&format!("ui-cordis requires Client Service {name:?}")).into())
    } else {
        Ok(service)
    }
}

fn own_locale(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let dictionaries = Object::new();
    set(&dictionaries, "en", &to_js_json(english_locale())?)?;
    set(&dictionaries, "zh", &to_js_json(chinese_locale())?)?;
    let locale = locale.clone();
    own_effect(
        ctx,
        "ui-cordis: dictionaries",
        Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            call_method(
                &locale,
                "register",
                &[
                    JsValue::from_str(CORDIS_LOCALE_NAMESPACE),
                    dictionaries.clone().into(),
                ],
            )
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

fn own_styles(ctx: &JsValue) -> Result<(), JsValue> {
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let document = required(&js_sys::global(), "document")?;
        let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
        call_method(
            &style,
            "setAttribute",
            &[
                JsValue::from_str("data-plugin"),
                JsValue::from_str(PLUGIN_NAME),
            ],
        )?;
        Reflect::set(
            &style,
            &JsValue::from_str("textContent"),
            &JsValue::from_str(STYLES),
        )?;
        let head = required(&document, "head")?;
        call_method(&head, "appendChild", std::slice::from_ref(&style))?;
        let disposer = Closure::wrap(Box::new(move || {
            let _ = call_method(&style, "remove", &[]);
        }) as Box<dyn FnMut()>);
        Ok(disposer.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    own_effect(ctx, "ui-cordis: styles", installer.into_js_value())
}

fn own_effect(ctx: &JsValue, label: &str, installer: JsValue) -> Result<(), JsValue> {
    call_method(ctx, "effect", &[installer, JsValue::from_str(label)])?;
    Ok(())
}

fn inventory_observable(inventory: &Arc<CordisInventory>) -> Result<JsValue, JsValue> {
    let cache: SnapshotCache<CordisInventorySnapshot> = Arc::new(Mutex::new(None));
    let getter_inventory = inventory.clone();
    let getter = Closure::wrap(Box::new(move || {
        let snapshot = getter_inventory.snapshot();
        cached_snapshot(&cache, snapshot.clone(), || inventory_snapshot(&snapshot))
            .unwrap_or_else(|error| error)
    }) as Box<dyn FnMut() -> JsValue>);
    let subscriber = inventory.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let subscription = subscriber.subscribe(Arc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    let observable = Object::new();
    set(&observable, "getSnapshot", &getter.into_js_value())?;
    set(&observable, "subscribe", &subscribe.into_js_value())?;
    Ok(observable.into())
}

fn inventory_snapshot(snapshot: &CordisInventorySnapshot) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "rows", &to_js_json(&snapshot.rows)?)?;
    let removed = Set::new(&JsValue::UNDEFINED);
    for plugin_id in &snapshot.removed {
        removed.add(&JsValue::from_str(plugin_id.as_str()));
    }
    set(&value, "removed", &removed.into())?;
    set(&value, "read", &JsValue::from_bool(snapshot.read))?;
    if let Some(error) = &snapshot.error {
        set(&value, "error", &JsValue::from_str(error))?;
    }
    Ok(value.into())
}

fn cached_snapshot<T: Any + Send + Sync>(
    cache: &SnapshotCache<T>,
    snapshot: Arc<T>,
    build: impl FnOnce() -> Result<JsValue, JsValue>,
) -> Result<JsValue, JsValue> {
    let mut cache = cache.lock();
    if let Some((current, value)) = &*cache
        && Arc::ptr_eq(current, &snapshot)
    {
        return Ok(value.clone());
    }
    let value = build()?;
    *cache = Some((snapshot, value.clone()));
    Ok(value)
}

fn refresh_function(inventory: &Arc<CordisInventory>, namespace: &JsValue) -> Function {
    let inventory = inventory.clone();
    let namespace = namespace.clone();
    Closure::wrap(Box::new(move || {
        let Some(ticket) = inventory.begin_refresh() else {
            return;
        };
        let returned = call_method(&namespace, "inventory", &[]);
        let inventory = inventory.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = match returned {
                Ok(returned) => JsFuture::from(Promise::resolve(&returned)).await,
                Err(error) => Err(error),
            };
            match result.and_then(|answered| remote_value(&answered)) {
                Ok(value) => {
                    match serde_wasm_bindgen::from_value::<Vec<DynamicCordisInventoryRow>>(value) {
                        Ok(rows) => {
                            inventory.resolve(ticket, rows);
                        }
                        Err(error) => {
                            report_inventory_error(&error.to_string());
                            inventory.reject(ticket, Some(error.to_string()));
                        }
                    }
                }
                Err(error) => {
                    report_inventory_error(&js_error_text(&error));
                    inventory.reject(ticket, inventory_error_message(&error));
                }
            }
        });
    }) as Box<dyn FnMut()>)
    .into_js_value()
    .unchecked_into()
}

fn remote_value(answered: &JsValue) -> Result<JsValue, JsValue> {
    match Reflect::get(answered, &JsValue::from_str("ok"))?.as_bool() {
        Some(true) => Reflect::get(answered, &JsValue::from_str("value")),
        Some(false) => {
            let error = Reflect::get(answered, &JsValue::from_str("error"))?;
            let code = Reflect::get(&error, &JsValue::from_str("code"))
                .ok()
                .and_then(|value| value.as_string())
                .unwrap_or_else(|| "remote-error".to_owned());
            let message = Reflect::get(&error, &JsValue::from_str("message"))
                .ok()
                .and_then(|value| value.as_string())
                .unwrap_or_else(|| format!("{error:?}"));
            Err(js_sys::Error::new(&format!("{code}: {message}")).into())
        }
        None => {
            Err(js_sys::Error::new("dynamicCordisRunner returned a malformed RemoteResult").into())
        }
    }
}

fn report_inventory_error(message: &str) {
    web_sys::console::error_2(
        &JsValue::from_str("[ui-cordis] reading the Cordis inventory failed:"),
        &JsValue::from_str(message),
    );
}

fn reconcile_on_inventory(
    ctx: &JsValue,
    inventory: &Arc<CordisInventory>,
    runner: &JsValue,
) -> Result<(), JsValue> {
    let weak = Arc::downgrade(inventory);
    let runner = runner.clone();
    let subscription = inventory.subscribe(Arc::new(move || {
        let Some(inventory) = weak.upgrade() else {
            return;
        };
        let snapshot = inventory.snapshot();
        if !snapshot.read {
            return;
        }
        if let Ok(rows) = to_js_json(&snapshot.rows) {
            let _ = call_method(&runner, "reconcileApprovals", &[rows]);
        }
    }));
    let disposer = Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>);
    own_effect(
        ctx,
        "ui-cordis: reconcile pending approvals",
        Closure::wrap(Box::new(move || disposer.as_ref().clone()) as Box<dyn FnMut() -> JsValue>)
            .into_js_value(),
    )
}

fn subscribe_remote_events(
    _ctx: &JsValue,
    remote: &JsValue,
    inventory: &Arc<CordisInventory>,
    refresh: &Function,
) -> Result<(), JsValue> {
    for event in ["cordis/dynamic-package", "cordis/dynamic-retract"] {
        let refresh = refresh.clone();
        let listener = Closure::wrap(Box::new(move || {
            let _ = refresh.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>);
        call_method(
            remote,
            "$on",
            &[JsValue::from_str(event), listener.into_js_value()],
        )?;
    }
    let weak = Arc::downgrade(inventory);
    let refresh_request = refresh.clone();
    let request = Closure::wrap(Box::new(move |request: JsValue| {
        let Some(inventory) = weak.upgrade() else {
            return;
        };
        let plugin_id = Reflect::get(&request, &JsValue::from_str("pluginId"))
            .ok()
            .and_then(|value| value.as_string());
        if plugin_id.is_some_and(|plugin_id| {
            !inventory
                .snapshot()
                .rows
                .iter()
                .any(|row| row.plugin_id.as_str() == plugin_id)
        }) {
            let _ = refresh_request.call0(&JsValue::UNDEFINED);
        }
    }) as Box<dyn FnMut(JsValue)>);
    call_method(
        remote,
        "$on",
        &[
            JsValue::from_str("cordis/request-run"),
            request.into_js_value(),
        ],
    )?;
    let refresh_resolved = refresh.clone();
    let resolved = Closure::wrap(Box::new(move || {
        let _ = refresh_resolved.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>);
    call_method(
        remote,
        "$on",
        &[
            JsValue::from_str("cordis/request-run-resolved"),
            resolved.into_js_value(),
        ],
    )?;
    Ok(())
}

fn subscribe_connection_reset(
    ctx: &JsValue,
    inventory: &Arc<CordisInventory>,
    refresh: &Function,
) -> Result<(), JsValue> {
    let inventory = inventory.clone();
    let refresh = refresh.clone();
    let listener = Closure::wrap(Box::new(move || {
        inventory.reset();
        let _ = refresh.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>);
    call_method(
        ctx,
        "on",
        &[
            JsValue::from_str("connection/reset"),
            listener.into_js_value(),
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn register_slots(
    slots: &JsValue,
    components: &JsValue,
    inventory: &Arc<CordisInventory>,
    inventory_js: &JsValue,
    runner: &JsValue,
    namespace: &JsValue,
    refresh: &Function,
) -> Result<(), JsValue> {
    let loaded = observable_alias(runner)?;
    register_panel_slot(
        slots,
        required(components, "CordisPanel")?,
        inventory,
        inventory_js,
        runner,
        namespace,
        refresh,
        &loaded,
    )?;
    register_define_slot(
        slots,
        required(components, "CordisDefineRow")?,
        inventory_js,
        &loaded,
    )?;
    register_run_slot(
        slots,
        required(components, "CordisRunRow")?,
        inventory_js,
        runner,
        &loaded,
    )?;
    register_action_slots(slots, required(components, "CordisActionRow")?)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn register_panel_slot(
    slots: &JsValue,
    component: JsValue,
    inventory: &Arc<CordisInventory>,
    inventory_js: &JsValue,
    runner: &JsValue,
    namespace: &JsValue,
    refresh: &Function,
    loaded: &JsValue,
) -> Result<(), JsValue> {
    let options = Object::new();
    set(
        &options,
        "name",
        &JsValue::from_str("sidebar.footer.action"),
    )?;
    set(&options, "id", &JsValue::from_str("cordis-panel"))?;
    set(
        &options,
        "locale",
        &JsValue::from_str(CORDIS_LOCALE_NAMESPACE),
    )?;
    let inventory_js = inventory_js.clone();
    let runner = runner.clone();
    let namespace = namespace.clone();
    let refresh = refresh.clone();
    let loaded = loaded.clone();
    let weak_inventory = Arc::downgrade(inventory);
    let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let hooks = object(&[
            ("inventory", inventory_js.clone()),
            ("activeRuns", required(&runner, "activeRuns")?),
            ("runErrors", required(&runner, "lastRunError")?),
            ("loaded", loaded.clone()),
            ("renderFailures", required(&runner, "renderFailures")?),
        ])?;
        let face = Object::new();
        set(&face, "hooks", &hooks)?;
        let on_approve: JsValue = runner_method(&runner, "approve")?.into();
        let on_decline: JsValue = runner_method(&runner, "decline")?.into();
        let on_run: JsValue = runner_method(&runner, "startUserRun")?.into();
        set(&face, "onApprove", &on_approve)?;
        set(&face, "onDecline", &on_decline)?;
        set(&face, "onRun", &on_run)?;
        set(
            &face,
            "onStop",
            &panel_action(&namespace, "stopFromPanel", weak_inventory.clone(), false),
        )?;
        set(
            &face,
            "onRemove",
            &panel_action(
                &namespace,
                "undefineFromPanel",
                weak_inventory.clone(),
                true,
            ),
        )?;
        set(&face, "onRefresh", &refresh)?;
        Ok(face.into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&options, "inject", &inject.into_js_value())?;
    inject_registration(slots, "sidebar.footer.action", options, component)
}

fn register_define_slot(
    slots: &JsValue,
    component: JsValue,
    inventory: &JsValue,
    loaded: &JsValue,
) -> Result<(), JsValue> {
    let options = Object::new();
    set(&options, "name", &JsValue::from_str("tool.call.toolview"))?;
    set(&options, "key", &JsValue::from_str("cordis_define"))?;
    set(
        &options,
        "locale",
        &JsValue::from_str(CORDIS_LOCALE_NAMESPACE),
    )?;
    let inventory = inventory.clone();
    let loaded = loaded.clone();
    let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let hooks = object(&[("inventory", inventory.clone()), ("loaded", loaded.clone())])?;
        object(&[("hooks", hooks.into())]).map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&options, "inject", &inject.into_js_value())?;
    inject_registration(slots, "tool.call.toolview", options, component)
}

fn register_run_slot(
    slots: &JsValue,
    component: JsValue,
    inventory: &JsValue,
    runner: &JsValue,
    loaded: &JsValue,
) -> Result<(), JsValue> {
    let options = Object::new();
    set(&options, "name", &JsValue::from_str("tool.call.toolview"))?;
    set(&options, "key", &JsValue::from_str("cordis_run"))?;
    set(
        &options,
        "locale",
        &JsValue::from_str(CORDIS_LOCALE_NAMESPACE),
    )?;
    let declaration = object(&[
        ("kind", JsValue::from_str("keyed")),
        ("scope", JsValue::from_str("session")),
    ])?;
    let children = Object::new();
    set(&children, "tool.view.cordis", &declaration)?;
    set(&options, "children", &children)?;
    let registry = Arc::new(CordisRunCardRegistry::default());
    let inventory = inventory.clone();
    let runner = runner.clone();
    let loaded = loaded.clone();
    let inject = Closure::wrap(
        Box::new(move |session_id: String| -> Result<JsValue, JsValue> {
            let store = registry.for_session(SessionId::new(session_id));
            let hooks = object(&[
                ("inventory", inventory.clone()),
                ("loaded", loaded.clone()),
                ("runCards", run_card_observable(&store)?),
                ("activeRuns", required(&runner, "activeRuns")?),
            ])?;
            let observe_store = store;
            let observe = Closure::wrap(Box::new(move |pointer: JsValue| -> Result<(), JsValue> {
                let key = string(&pointer, "key")?;
                observe_store.observe(crate::CordisRunCardPointer {
                    key: crate::CordisToolViewKey::new(key),
                    call_id: string(&pointer, "callId")?,
                    seq: integer(&pointer, "seq")?,
                    plugin_run_id: seekdeep_cordis_dynamic_types::CordisDynamicPluginRunId::new(
                        string(&pointer, "pluginRunId")?,
                    ),
                });
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            object(&[
                ("hooks", hooks.into()),
                ("onObserveRunCard", observe.into_js_value()),
            ])
            .map(Into::into)
        }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>,
    );
    set(&options, "inject", &inject.into_js_value())?;
    inject_registration(slots, "tool.call.toolview", options, component)
}

fn register_action_slots(slots: &JsValue, component: JsValue) -> Result<(), JsValue> {
    let registrar = slots.clone();
    let factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let registrations = Array::new();
        for key in ["cordis_stop", "cordis_undefine"] {
            let options = object(&[
                ("name", JsValue::from_str("tool.call.toolview")),
                ("key", JsValue::from_str(key)),
                ("locale", JsValue::from_str(CORDIS_LOCALE_NAMESPACE)),
            ])?;
            registrations.push(&call_method(
                &registrar,
                "register",
                &[options.into(), component.clone()],
            )?);
        }
        Ok(registrations.into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        slots,
        "inject",
        &[
            JsValue::from_str("tool.call.toolview"),
            factory.into_js_value(),
        ],
    )?;
    Ok(())
}

fn inject_registration(
    slots: &JsValue,
    slot: &str,
    options: Object,
    component: JsValue,
) -> Result<(), JsValue> {
    let registrar = slots.clone();
    let factory = Closure::wrap(Box::new(move || {
        call_method(
            &registrar,
            "register",
            &[options.clone().into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        slots,
        "inject",
        &[JsValue::from_str(slot), factory.into_js_value()],
    )?;
    Ok(())
}

fn observable_alias(source: &JsValue) -> Result<JsValue, JsValue> {
    object(&[
        ("getSnapshot", required(source, "getSnapshot")?),
        ("subscribe", required(source, "subscribe")?),
    ])
    .map(Into::into)
}

fn run_card_observable(store: &Arc<CordisRunCardStore>) -> Result<JsValue, JsValue> {
    let cache: SnapshotCache<BTreeMap<crate::CordisToolViewKey, crate::CordisRunCardPointer>> =
        Arc::new(Mutex::new(None));
    let getter_store = store.clone();
    let getter = Closure::wrap(Box::new(move || {
        let snapshot = getter_store.snapshot();
        cached_snapshot(&cache, snapshot.clone(), || {
            let output = Map::new();
            for (key, pointer) in snapshot.iter() {
                let value = object(&[
                    ("key", JsValue::from_str(key.as_str())),
                    ("callId", JsValue::from_str(&pointer.call_id)),
                    ("seq", js_number(pointer.seq)),
                    (
                        "pluginRunId",
                        JsValue::from_str(pointer.plugin_run_id.as_str()),
                    ),
                ])?;
                output.set(&JsValue::from_str(key.as_str()), &value);
            }
            Ok(output.into())
        })
        .unwrap_or_else(|error| error)
    }) as Box<dyn FnMut() -> JsValue>);
    let subscriber = store.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let subscription = subscriber.subscribe(Arc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    object(&[
        ("getSnapshot", getter.into_js_value()),
        ("subscribe", subscribe.into_js_value()),
    ])
    .map(Into::into)
}

fn runner_method(runner: &JsValue, name: &str) -> Result<Function, JsValue> {
    let method = required(runner, name)?.dyn_into::<Function>()?;
    let this = runner.clone();
    match name {
        "approve" => Ok(
            Closure::wrap(Box::new(move |first: JsValue, second: JsValue| {
                method.call2(&this, &first, &second)
            })
                as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>)
            .into_js_value()
            .unchecked_into(),
        ),
        "decline" | "startUserRun" => {
            Ok(
                Closure::wrap(Box::new(move |value: JsValue| method.call1(&this, &value))
                    as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
                .into_js_value()
                .unchecked_into(),
            )
        }
        _ => {
            Err(js_sys::Error::new(&format!("unsupported ui-cordis runner method {name:?}")).into())
        }
    }
}

fn panel_action(
    namespace: &JsValue,
    method: &'static str,
    inventory: Weak<CordisInventory>,
    retire_on_success: bool,
) -> Function {
    let namespace = namespace.clone();
    Closure::wrap(
        Box::new(move |session_id: String, plugin_id: String| -> Promise {
            let namespace = namespace.clone();
            let inventory = inventory.clone();
            future_to_promise(async move {
                let returned = call_method(
                    &namespace,
                    method,
                    &[
                        JsValue::from_str(&session_id),
                        JsValue::from_str(&plugin_id),
                    ],
                )?;
                let answered = JsFuture::from(Promise::resolve(&returned)).await?;
                let value = match remote_value(&answered) {
                    Ok(value) => value,
                    Err(error) => {
                        return action_result(false, Some(js_error_text(&error)));
                    }
                };
                let ok = Reflect::get(&value, &JsValue::from_str("ok"))?
                    .as_bool()
                    .ok_or_else(|| js_sys::Error::new("Cordis panel result has no boolean ok"))?;
                if method == "stopFromPanel" {
                    if ok
                        || Reflect::get(&value, &JsValue::from_str("reason"))?
                            .as_string()
                            .as_deref()
                            == Some("not-running")
                    {
                        return action_result(true, None);
                    }
                } else if ok {
                    if retire_on_success && let Some(inventory) = inventory.upgrade() {
                        inventory.retire(&CordisDynamicPluginId::new(plugin_id));
                    }
                    return action_result(true, None);
                }
                let message = Reflect::get(&value, &JsValue::from_str("message"))?
                    .as_string()
                    .unwrap_or_else(|| "operation failed".to_owned());
                action_result(false, Some(message))
            })
        }) as Box<dyn FnMut(String, String) -> Promise>,
    )
    .into_js_value()
    .unchecked_into()
}

fn action_result(ok: bool, message: Option<String>) -> Result<JsValue, JsValue> {
    let result = Object::new();
    set(&result, "ok", &JsValue::from_bool(ok))?;
    if let Some(message) = message {
        set(&result, "message", &JsValue::from_str(&message))?;
    }
    Ok(result.into())
}

fn register_input_source(
    ctx: &JsValue,
    input_triggers: &JsValue,
    inventory: &Arc<CordisInventory>,
    inventory_js: &JsValue,
    refresh: &Function,
) -> Result<(), JsValue> {
    let source = Object::new();
    set(&source, "trigger", &JsValue::from_str("@"))?;
    set(&source, "name", &JsValue::from_str("cordis"))?;
    set(&source, "order", &JsValue::from_f64(1.0))?;

    let candidates_inventory = inventory.clone();
    let candidates = Closure::wrap(Box::new(
        move |session: JsValue, request: JsValue| -> Result<Promise, JsValue> {
            let session_id = SessionId::new(string(&session, "sessionId")?);
            let query = string(&request, "query")?;
            let candidates = cordis_trigger_candidates(
                &candidates_inventory.snapshot().rows,
                &session_id,
                &query,
            );
            let rows = Array::new();
            for candidate in candidates {
                let value = Object::new();
                set(&value, "name", &JsValue::from_str(&candidate.name))?;
                if let Some(description) = candidate.description {
                    set(&value, "description", &JsValue::from_str(&description))?;
                }
                rows.push(&value);
            }
            let rows: JsValue = rows.into();
            Ok(Promise::resolve(&rows))
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<Promise, JsValue>>);
    set(&source, "candidates", &candidates.into_js_value())?;
    set(&source, "warm", refresh)?;

    let lexicon_inventory = inventory.clone();
    let lexicon = Closure::wrap(
        Box::new(move |session: JsValue| -> Result<JsValue, JsValue> {
            let session_id = SessionId::new(string(&session, "sessionId")?);
            let output = Array::new();
            for candidate in
                cordis_trigger_candidates(&lexicon_inventory.snapshot().rows, &session_id, "")
            {
                output.push(&JsValue::from_str(&candidate.name));
            }
            Ok(output.into())
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    set(&source, "lexicon", &lexicon.into_js_value())?;

    let subscribe = required(inventory_js, "subscribe")?.dyn_into::<Function>()?;
    let subscribe_lexicon = Closure::wrap(Box::new(
        move |_session: JsValue, listener: Function| -> Result<JsValue, JsValue> {
            subscribe.call1(&JsValue::UNDEFINED, &listener)
        },
    )
        as Box<dyn FnMut(JsValue, Function) -> Result<JsValue, JsValue>>);
    set(
        &source,
        "subscribeLexicon",
        &subscribe_lexicon.into_js_value(),
    )?;

    let on_pick = Closure::wrap(
        Box::new(move |selection: JsValue| -> Result<JsValue, JsValue> {
            let candidate = required(&selection, "candidate")?;
            let candidate = crate::CordisTriggerCandidate {
                name: string(&candidate, "name")?,
                description: None,
            };
            object(&[("text", JsValue::from_str(&cordis_trigger_pick(&candidate)))]).map(Into::into)
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    set(&source, "onPick", &on_pick.into_js_value())?;

    let input_triggers = input_triggers.clone();
    own_effect(
        ctx,
        "ui-cordis: @pluginId source",
        Closure::wrap(Box::new(move || {
            call_method(&input_triggers, "registerSource", &[source.clone().into()])
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

fn required(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_sys::Error::new(&format!("ui-cordis requires {key:?}")).into())
    } else {
        Ok(property)
    }
}

fn string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    required(value, key)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("ui-cordis {key:?} is not a string")).into())
}

fn integer(value: &JsValue, key: &str) -> Result<u64, JsValue> {
    let number = required(value, key)?
        .as_f64()
        .ok_or_else(|| js_sys::Error::new(&format!("ui-cordis {key:?} is not a number")))?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return Err(js_sys::Error::new(&format!(
            "ui-cordis {key:?} is not a non-negative integer"
        ))
        .into());
    }
    number.to_string().parse().map_err(|_| {
        js_sys::Error::new(&format!(
            "ui-cordis {key:?} is outside the Rust sequence range"
        ))
        .into()
    })
}

fn object(properties: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let output = Object::new();
    for (key, value) in properties {
        set(&output, key, value)?;
    }
    Ok(output)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set {key:?}")).into())
    }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = required(value, name)?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_number(value: u64) -> JsValue {
    JsValue::from_f64(
        value
            .to_string()
            .parse()
            .expect("u64 decimal text is a finite JavaScript number"),
    )
}

fn js_error_text(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .or_else(|| js_sys::JsString::from(error.clone()).as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}

fn inventory_error_message(error: &JsValue) -> Option<String> {
    error
        .dyn_ref::<js_sys::Error>()
        .map(|error| String::from(error.message()))
}

fn to_js_json(value: &impl Serialize) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}
