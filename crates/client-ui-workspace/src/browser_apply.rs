//! Browser Cordis, Store, locale, and Slot assembly for Workspace surfaces.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    FLAT_SESSION_ORDER_KEY, WORKSPACE_LOCALES, WORKSPACE_NS, WORKSPACE_VIEW_PERSIST_KEY,
    browser::{call, object, required},
    workspace_browser_component, workspace_picker_component,
};

const INJECT: &[&str] = &["slots", "sessions", "workspaces", "locale"];

thread_local! {
    static DEFINE_STORE: RefCell<Option<Function>> = const { RefCell::new(None) };
}

/// Configures the runtime `defineStore` factory used by the browser registration.
#[wasm_bindgen(js_name = configureClientUiWorkspaceApply)]
pub fn configure_client_ui_workspace_apply(define_store: Function) {
    DEFINE_STORE.with(|configured| *configured.borrow_mut() = Some(define_store));
}

fn configured_define_store() -> Result<Function, JsValue> {
    DEFINE_STORE.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-workspace apply was not configured").into()
        })
    })
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = workspaceInject)]
pub fn workspace_inject() -> Array {
    INJECT
        .iter()
        .map(|value| JsValue::from_str(value))
        .collect()
}

fn set(value: &JsValue, key: &str, entry: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(value, &JsValue::from_str(key), entry)? {
        Ok(())
    } else {
        Err(js_sys::TypeError::new(&format!("Workspace Store draft rejected {key}")).into())
    }
}

fn retained_accounts(source: &JsValue, retained: &Array) -> Result<Object, JsValue> {
    let output = Object::new();
    for entry in Object::entries(&source.clone().dyn_into::<Object>()?).iter() {
        let entry = Array::from(&entry);
        let Some(key) = entry.get(0).as_string() else {
            continue;
        };
        if retained
            .iter()
            .any(|candidate| candidate.as_string().as_deref() == Some(key.as_str()))
        {
            Reflect::set(&output, &JsValue::from_str(&key), &entry.get(1))?;
        }
    }
    Ok(output)
}

fn create_workspace_view_store_browser() -> Result<JsValue, JsValue> {
    let init = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        Ok(object(&[
            ("groupBy", JsValue::from_str("workspace")),
            ("orderBy", JsValue::from_str("updated")),
            ("groupExpansion", Object::new().into()),
            ("sessionOrderByAccount", Object::new().into()),
            ("sessionUpdatedAtByAccount", Object::new().into()),
        ])?
        .into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let actions = Object::new();
    let set_group =
        Closure::wrap(
            Box::new(move |draft: JsValue, mode: JsValue| set(&draft, "groupBy", &mode))
                as Box<dyn FnMut(JsValue, JsValue) -> Result<(), JsValue>>,
        );
    Reflect::set(
        &actions,
        &JsValue::from_str("setGroupBy"),
        &set_group.into_js_value(),
    )?;
    let set_order =
        Closure::wrap(
            Box::new(move |draft: JsValue, mode: JsValue| set(&draft, "orderBy", &mode))
                as Box<dyn FnMut(JsValue, JsValue) -> Result<(), JsValue>>,
        );
    Reflect::set(
        &actions,
        &JsValue::from_str("setOrderBy"),
        &set_order.into_js_value(),
    )?;
    let set_expanded = Closure::wrap(Box::new(
        move |draft: JsValue, key: String, expanded: bool| -> Result<(), JsValue> {
            let values = required(&draft, "groupExpansion", "Workspace Store draft")?;
            set(&values, &key, &JsValue::from_bool(expanded))
        },
    )
        as Box<dyn FnMut(JsValue, String, bool) -> Result<(), JsValue>>);
    Reflect::set(
        &actions,
        &JsValue::from_str("setGroupExpanded"),
        &set_expanded.into_js_value(),
    )?;
    let retain = Closure::wrap(Box::new(
        move |draft: JsValue, keys: JsValue| -> Result<(), JsValue> {
            let keys = Array::from(&keys);
            for field in [
                "groupExpansion",
                "sessionOrderByAccount",
                "sessionUpdatedAtByAccount",
            ] {
                let source = required(&draft, field, "Workspace Store draft")?;
                set(&draft, field, &retained_accounts(&source, &keys)?.into())?;
            }
            Ok(())
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<(), JsValue>>);
    Reflect::set(
        &actions,
        &JsValue::from_str("retainAccountKeys"),
        &retain.into_js_value(),
    )?;
    let sync = Closure::wrap(Box::new(
        move |draft: JsValue,
              account: String,
              order: JsValue,
              updated_at: JsValue|
              -> Result<(), JsValue> {
            let orders = required(&draft, "sessionOrderByAccount", "Workspace Store draft")?;
            let timestamps =
                required(&draft, "sessionUpdatedAtByAccount", "Workspace Store draft")?;
            set(&orders, &account, &order)?;
            set(&timestamps, &account, &updated_at)
        },
    )
        as Box<dyn FnMut(JsValue, String, JsValue, JsValue) -> Result<(), JsValue>>);
    Reflect::set(
        &actions,
        &JsValue::from_str("syncSessionOrderAccount"),
        &sync.into_js_value(),
    )?;
    let set_session_order = Closure::wrap(Box::new(
        move |draft: JsValue, account: String, order: JsValue| -> Result<(), JsValue> {
            let orders = required(&draft, "sessionOrderByAccount", "Workspace Store draft")?;
            set(&orders, &account, &order)
        },
    )
        as Box<dyn FnMut(JsValue, String, JsValue) -> Result<(), JsValue>>);
    Reflect::set(
        &actions,
        &JsValue::from_str("setSessionOrder"),
        &set_session_order.into_js_value(),
    )?;
    let declaration = object(&[
        ("init", init.into_js_value()),
        ("persist", JsValue::from_str(WORKSPACE_VIEW_PERSIST_KEY)),
        ("actions", actions.into()),
    ])?;
    configured_define_store()?.call1(&JsValue::UNDEFINED, &declaration)
}

fn locale_dictionaries() -> Result<JsValue, JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, chinese, english) in WORKSPACE_LOCALES {
        Reflect::set(&zh, &JsValue::from_str(key), &JsValue::from_str(chinese))?;
        Reflect::set(&en, &JsValue::from_str(key), &JsValue::from_str(english))?;
    }
    Ok(object(&[("zh", zh.into()), ("en", en.into())])?.into())
}

fn own_locales(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let locale = locale.clone();
    let dictionaries = locale_dictionaries()?;
    let install = Closure::wrap(Box::new(move || {
        call(
            &locale,
            "register",
            &[JsValue::from_str(WORKSPACE_NS), dictionaries.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call(
        ctx,
        "effect",
        &[
            install.into_js_value(),
            JsValue::from_str("ui-workspace: dictionaries"),
        ],
    )?;
    Ok(())
}

fn result_value(result: &JsValue) -> Result<JsValue, JsValue> {
    if required(result, "ok", "Client RPC result")?.as_bool() == Some(true) {
        return required(result, "value", "successful Client RPC result");
    }
    let error = required(result, "error", "failed Client RPC result")?;
    let message = required(&error, "message", "Client RPC error")?
        .as_string()
        .unwrap_or_default();
    Err(js_sys::Error::new(&message).into())
}

fn map_rpc_result(result: &JsValue) -> Result<Promise, JsValue> {
    let success = Closure::wrap(
        Box::new(move |result: JsValue| match result_value(&result) {
            Ok(value) => value,
            Err(error) => wasm_bindgen::throw_val(error),
        }) as Box<dyn FnMut(JsValue) -> JsValue>,
    );
    call(
        Promise::resolve(result).as_ref(),
        "then",
        &[success.into_js_value()],
    )?
    .dyn_into()
}

fn flow_source(slots: &JsValue, hole: &'static str) -> Result<JsValue, JsValue> {
    let entries_slots = slots.clone();
    let get_snapshot = Closure::wrap(Box::new(move || -> Result<bool, JsValue> {
        Ok(Array::from(&call(
            &entries_slots,
            "entries",
            &[JsValue::from_str(hole)],
        )?)
        .length()
            > 0)
    }) as Box<dyn FnMut() -> Result<bool, JsValue>>);
    let subscribe_slots = slots.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: JsValue| {
        call(
            &subscribe_slots,
            "subscribe",
            &[JsValue::from_str(hole), listener],
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    Ok(object(&[
        ("getSnapshot", get_snapshot.into_js_value()),
        ("subscribe", subscribe.into_js_value()),
    ])?
    .into())
}

#[allow(clippy::too_many_lines)]
fn browser_inject(
    sessions: &JsValue,
    workspaces: JsValue,
    flow: JsValue,
) -> Result<JsValue, JsValue> {
    let start_workspaces = workspaces.clone();
    let start = Closure::wrap(Box::new(move |workspace_id: JsValue| {
        call(&start_workspaces, "startSession", &[workspace_id])
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let open_sessions = sessions.clone();
    let open = Closure::wrap(Box::new(move |session_id: JsValue| {
        call(&open_sessions, "open", &[session_id])
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let search_sessions = sessions.clone();
    let search = Closure::wrap(Box::new(move |query: JsValue, signal: JsValue| -> Promise {
        match call(&search_sessions, "search", &[query, signal])
            .and_then(|result| map_rpc_result(&result))
        {
            Ok(promise) => promise,
            Err(error) => Promise::reject(&error),
        }
    }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>);
    let rename_sessions = sessions.clone();
    let rename_session = Closure::wrap(Box::new(
        move |session_id: String, title: JsValue| -> Promise {
            let operation = (|| -> Result<Promise, JsValue> {
                let binding = call(
                    &rename_sessions,
                    "binding",
                    &[JsValue::from_str(&session_id)],
                )?;
                if binding.is_null() || binding.is_undefined() {
                    return Err(
                        js_sys::Error::new(&format!("unknown session \"{session_id}\"")).into(),
                    );
                }
                let session = required(&binding, "session", "Session binding")?;
                map_rpc_result(&call(&session, "rename", &[title])?)
            })();
            match operation {
                Ok(promise) => promise,
                Err(error) => Promise::reject(&error),
            }
        },
    ) as Box<dyn FnMut(String, JsValue) -> Promise>);
    let fork_sessions = sessions.clone();
    let fork = Closure::wrap(Box::new(move |session_id: String| -> Result<(), JsValue> {
        let request = object(&[
            ("sessionId", JsValue::from_str(&session_id)),
            ("increaseTitle", JsValue::TRUE),
        ])?;
        let promise = Promise::resolve(&call(&fork_sessions, "fork", &[request.into()])?);
        let open_sessions = fork_sessions.clone();
        let success = Closure::wrap(Box::new(move |child: JsValue| {
            let _ = call(&open_sessions, "open", &[child]);
        }) as Box<dyn FnMut(JsValue)>);
        let failure =
            Closure::wrap(Box::new(move |_reason: JsValue| {}) as Box<dyn FnMut(JsValue)>);
        let _ = promise.then2(&success, &failure);
        drop(success.into_js_value());
        drop(failure.into_js_value());
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let rename_workspaces = workspaces.clone();
    let rename_workspace = Closure::wrap(Box::new(move |id: JsValue, title: JsValue| {
        call(&rename_workspaces, "rename", &[id, title])
    })
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    let delete_workspaces = workspaces.clone();
    let delete_workspace =
        Closure::wrap(
            Box::new(move |id: JsValue| call(&delete_workspaces, "delete", &[id]))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
    let insert_workspaces = workspaces.clone();
    let insert_workspace = Closure::wrap(Box::new(move |id: JsValue, before: JsValue| {
        call(&insert_workspaces, "insertBefore", &[id, before])
    })
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    let archive_workspaces = workspaces.clone();
    let archive = Closure::wrap(Box::new(move |id: JsValue| {
        call(&archive_workspaces, "archiveSession", &[id])
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let session_order_workspaces = workspaces.clone();
    let insert_session = Closure::wrap(Box::new(
        move |workspace_id: JsValue, session_id: JsValue, before: JsValue| {
            call(
                &session_order_workspaces,
                "insertSessionBefore",
                &[workspace_id, session_id, before],
            )
        },
    )
        as Box<dyn FnMut(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>);
    let create_workspaces = workspaces;
    let create =
        Closure::wrap(
            Box::new(move |input: JsValue| call(&create_workspaces, "create", &[input]))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
    let limit = required(sessions, "searchResultLimit", "Sessions service")?;
    Ok(object(&[
        ("startSession", start.into_js_value()),
        ("open", open.into_js_value()),
        ("searchSessions", search.into_js_value()),
        ("searchResultLimit", limit),
        ("renameSession", rename_session.into_js_value()),
        ("forkSession", fork.into_js_value()),
        ("renameWorkspace", rename_workspace.into_js_value()),
        ("deleteWorkspace", delete_workspace.into_js_value()),
        ("insertWorkspaceBefore", insert_workspace.into_js_value()),
        ("archiveSession", archive.into_js_value()),
        ("insertSessionBefore", insert_session.into_js_value()),
        ("createWorkspace", create.into_js_value()),
        ("hooks", object(&[("directoryFlow", flow)])?.into()),
    ])?
    .into())
}

fn picker_inject(workspaces: JsValue, flow: JsValue) -> Result<JsValue, JsValue> {
    let create = Closure::wrap(
        Box::new(move |input: JsValue| call(&workspaces, "create", &[input]))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    Ok(object(&[
        ("createWorkspace", create.into_js_value()),
        ("hooks", object(&[("directoryFlow", flow)])?.into()),
    ])?
    .into())
}

fn child_declaration(name: &str) -> Result<Object, JsValue> {
    object(&[(
        name,
        object(&[
            ("kind", JsValue::from_str("single")),
            ("scope", JsValue::from_str("root")),
        ])?
        .into(),
    )])
}

fn inject_registration(
    slots: &JsValue,
    declaration: &'static str,
    options: Object,
    component: JsValue,
    fresh_store: bool,
) -> Result<(), JsValue> {
    let slots_owner = slots.clone();
    let install = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let active_options = Object::assign(&Object::new(), &options);
        if fresh_store {
            Reflect::set(
                &active_options,
                &JsValue::from_str("store"),
                &create_workspace_view_store_browser()?,
            )?;
        }
        call(
            &slots_owner,
            "register",
            &[active_options.into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call(
        slots,
        "inject",
        &[JsValue::from_str(declaration), install.into_js_value()],
    )?;
    Ok(())
}

/// Applies the Workspace browser and conversation picker registrations.
///
/// # Errors
///
/// Returns missing service, Store, locale, Slot, or component failures.
#[wasm_bindgen(js_name = applyClientUiWorkspace)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_workspace(ctx: JsValue) -> Result<(), JsValue> {
    let slots = required(&ctx, "slots", "Client Context")?;
    let sessions = required(&ctx, "sessions", "Client Context")?;
    let workspaces = required(&ctx, "workspaces", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    own_locales(&ctx, &locale)?;
    let browser_flow = flow_source(&slots, "sidebar.workspaces.directoryFlow")?;
    let picker_flow = flow_source(&slots, "conversation.hero.workspace.directoryFlow")?;
    let browser_face = browser_inject(&sessions, workspaces.clone(), browser_flow)?;
    let browser_injector =
        Closure::wrap(Box::new(move || browser_face.clone()) as Box<dyn FnMut() -> JsValue>);
    inject_registration(
        &slots,
        "sidebar.workspaces",
        object(&[
            ("name", JsValue::from_str("sidebar.workspaces")),
            (
                "children",
                child_declaration("sidebar.workspaces.directoryFlow")?.into(),
            ),
            ("inject", browser_injector.into_js_value()),
            ("locale", JsValue::from_str(WORKSPACE_NS)),
        ])?,
        workspace_browser_component()?,
        true,
    )?;
    let picker_face = picker_inject(workspaces, picker_flow)?;
    let picker_injector =
        Closure::wrap(Box::new(move || picker_face.clone()) as Box<dyn FnMut() -> JsValue>);
    inject_registration(
        &slots,
        "conversation.hero.workspace",
        object(&[
            ("name", JsValue::from_str("conversation.hero.workspace")),
            (
                "children",
                child_declaration("conversation.hero.workspace.directoryFlow")?.into(),
            ),
            ("inject", picker_injector.into_js_value()),
            ("locale", JsValue::from_str(WORKSPACE_NS)),
        ])?,
        workspace_picker_component()?,
        false,
    )
}

/// Browser-visible source name for the flat list order account.
#[wasm_bindgen(js_name = flatSessionOrderKey)]
pub fn flat_session_order_key() -> String {
    FLAT_SESSION_ORDER_KEY.to_owned()
}
