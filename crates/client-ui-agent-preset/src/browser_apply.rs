//! Browser Cordis and Slot assembly for Agent preset surfaces.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, JSON, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    AGENT_PRESET_SETTINGS_NS, agent_preset_label_component, agent_preset_row_component,
    agent_preset_seat_component, agent_preset_section_component,
    browser::{call_method, object, optional, required},
    create_agent_preset_seat_controller, create_agent_preset_section_controller,
    create_agent_preset_settings_controller,
};

const NS: &str = "settings.agentPreset";
const INJECT: &[&str] = &["slots", "locale", "connection", "remote"];
const LOCALES: &str = include_str!("../data/locales.json");

/// Mounts the default row, management section, scoped hero seat, and header label.
///
/// # Errors
///
/// Returns missing-service, generated API, effect, scope, or Slot registration failures.
#[wasm_bindgen(js_name = applyClientUiAgentPreset)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn apply_client_ui_agent_preset(ctx: JsValue) -> Result<(), JsValue> {
    let slots = required(&ctx, "slots", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    let remote = required(&ctx, "remote", "Client Context")?;
    let connection = call_method(&ctx, "get", &[JsValue::from_str("connection")])?;
    let api = required(&connection, "api", "Connection handle")?;
    let settings_face = create_agent_preset_settings_controller(api.clone())?;
    let roster_readers = Rc::new(RefCell::new(Vec::<Function>::new()));
    let changed_settings = settings_face.clone();
    let changed_readers = roster_readers.clone();
    let roster_changed = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&changed_settings, "load", &[])?;
        for reader in changed_readers.borrow().iter() {
            reader.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let section_face = create_agent_preset_section_controller(
        api.clone(),
        Some(roster_changed.into_js_value().unchecked_into()),
    )?;

    own_locales(&ctx, &locale)?;
    own_root_refresh(&ctx, &remote, &settings_face, &section_face)?;

    let creator_draft = Rc::new(RefCell::new(None::<Function>));
    install_scoped_surfaces(
        &ctx,
        settings_face.clone(),
        roster_readers,
        creator_draft.clone(),
    )?;

    let row_face = remap_hook(&settings_face, "agentPresetSettings", "agentPreset")?;
    inject_registration(
        &slots,
        "settings.general.item",
        object(&[
            ("name", JsValue::from_str("settings.general.item")),
            ("id", JsValue::from_str("agent-preset")),
            ("order", JsValue::from_f64(-25.0)),
            ("locale", JsValue::from_str(NS)),
            ("inject", constant_inject(row_face)),
        ])?,
        agent_preset_row_component()?,
    )?;

    let translate =
        call_method(&locale, "bind", &[JsValue::from_str(NS)])?.dyn_into::<Function>()?;
    let label = Closure::wrap(Box::new(move || {
        translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("nav"))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let injected_section = section_face;
    let injected_creator = creator_draft;
    let section_inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let source = injected_section.clone().dyn_into::<Object>()?;
        let output = Object::assign(&Object::new(), &source);
        if let Some(creator) = injected_creator.borrow().as_ref() {
            Reflect::set(&output, &JsValue::from_str("startCreatorDraft"), creator)?;
        }
        Ok(output.into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    inject_registration(
        &slots,
        "settings.section",
        object(&[
            ("name", JsValue::from_str("settings.section")),
            ("id", JsValue::from_str("agent-presets")),
            ("order", JsValue::from_f64(20.0)),
            ("label", label.into_js_value()),
            ("locale", JsValue::from_str(NS)),
            ("inject", section_inject.into_js_value()),
        ])?,
        agent_preset_section_component()?,
    )
}

/// Exact root Client service dependencies.
#[wasm_bindgen(js_name = agentPresetInject)]
pub fn agent_preset_inject() -> Array {
    let output = Array::new();
    for dependency in INJECT {
        output.push(&JsValue::from_str(dependency));
    }
    output
}

fn own_locales(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let encoded = JSON::parse(LOCALES)?;
    let dictionaries: JsValue = object(&[
        ("zh", required(&encoded, "zh", "locale dictionaries")?),
        ("en", required(&encoded, "en", "locale dictionaries")?),
    ])?
    .into();
    let locale = locale.clone();
    let install = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(NS), dictionaries.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            install.into_js_value(),
            JsValue::from_str("ui-agent-preset: settings row dictionaries"),
        ],
    )?;
    Ok(())
}

fn own_root_refresh(
    ctx: &JsValue,
    remote: &JsValue,
    settings_face: &JsValue,
    section_face: &JsValue,
) -> Result<(), JsValue> {
    let remote = remote.clone();
    let context = ctx.clone();
    let settings = settings_face.clone();
    let section = section_face.clone();
    let install = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let refresh_settings = settings.clone();
        let refresh_section = section.clone();
        let refresh = Rc::new(move || -> Result<(), JsValue> {
            call_method(&refresh_settings, "load", &[])?;
            let hooks = required(&refresh_section, "hooks", "section face")?;
            let store = required(&hooks, "agentPresetSection", "section hooks")?;
            let snapshot = call_method(&store, "getSnapshot", &[])?;
            if required(&snapshot, "status", "section snapshot")?
                .as_string()
                .as_deref()
                != Some("idle")
            {
                call_method(&refresh_section, "load", &[])?;
            }
            Ok(())
        });
        let settings_refresh = refresh.clone();
        let settings_listener = Closure::wrap(Box::new(move |namespace: String| {
            if namespace == AGENT_PRESET_SETTINGS_NS {
                let _ = settings_refresh();
            }
        }) as Box<dyn FnMut(String)>);
        let off_settings = call_method(
            &remote,
            "$on",
            &[
                JsValue::from_str("settings/document-updated"),
                settings_listener.into_js_value(),
            ],
        )?
        .dyn_into::<Function>()?;
        let reset_refresh = refresh;
        let reset_listener = Closure::wrap(Box::new(move || {
            let _ = reset_refresh();
        }) as Box<dyn FnMut()>);
        let off_reset = call_method(
            &context,
            "on",
            &[
                JsValue::from_str("connection/reset"),
                reset_listener.into_js_value(),
            ],
        )?
        .dyn_into::<Function>()?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = off_settings.call0(&JsValue::UNDEFINED);
            let _ = off_reset.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            install.into_js_value(),
            JsValue::from_str("ui-agent-preset: settings refresh"),
        ],
    )?;
    Ok(())
}

fn install_scoped_surfaces(
    ctx: &JsValue,
    settings_face: JsValue,
    roster_readers: Rc<RefCell<Vec<Function>>>,
    creator_draft: Rc<RefCell<Option<Function>>>,
) -> Result<(), JsValue> {
    let callback = Closure::wrap(Box::new(move |scope: JsValue| -> Result<(), JsValue> {
        install_scope(
            &scope,
            &settings_face,
            roster_readers.clone(),
            creator_draft.clone(),
        )
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let dependencies = Array::new();
    for dependency in ["slots", "conversation", "sessions", "workspaces"] {
        dependencies.push(&JsValue::from_str(dependency));
    }
    call_method(
        ctx,
        "inject",
        &[dependencies.into(), callback.into_js_value()],
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)] // Scoped lifecycle owns Session listeners, creator handoff, and two Slot rows.
fn install_scope(
    scope: &JsValue,
    settings_face: &JsValue,
    roster_readers: Rc<RefCell<Vec<Function>>>,
    creator_draft: Rc<RefCell<Option<Function>>>,
) -> Result<(), JsValue> {
    let connection = call_method(scope, "get", &[JsValue::from_str("connection")])?;
    let api = required(&connection, "api", "Connection handle")?;
    let sessions = required(scope, "sessions", "scoped Context")?;
    let session_list = required(&sessions, "list", "Sessions service")?;
    let current_list = session_list.clone();
    let current = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = call_method(&current_list, "getSnapshot", &[])?;
        let Some(id) = optional(&snapshot, "current")?.and_then(|id| id.as_string()) else {
            return Ok(JsValue::UNDEFINED);
        };
        let by_id = required(&snapshot, "byId", "Sessions snapshot")?;
        Reflect::get(&by_id, &JsValue::from_str(&id))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let applied_sessions = sessions.clone();
    let applied = Closure::wrap(Box::new(move |session: String, preset: String| {
        let _ = call_method(
            &applied_sessions,
            "noteAgentPreset",
            &[JsValue::from_str(&session), JsValue::from_str(&preset)],
        );
    }) as Box<dyn FnMut(String, String)>);
    let seat_face = create_agent_preset_seat_controller(
        api,
        current.into_js_value().unchecked_into(),
        Some(applied.into_js_value().unchecked_into()),
    )?;
    let label_face = remap_hook(settings_face, "agentPresetSettings", "agentPresets")?;
    let effect_scope = scope.clone();
    let effect_sessions = sessions;
    let effect_list = session_list;
    let effect_seat = seat_face.clone();
    let effect_readers = roster_readers;
    let effect_creator = creator_draft;
    let effect_label = label_face;
    let install = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let apply_seat = effect_seat.clone();
        let list_listener = Closure::wrap(Box::new(move || {
            let _ = call_method(&apply_seat, "apply", &[]);
        }) as Box<dyn FnMut()>);
        let stop_list = call_method(&effect_list, "subscribe", &[list_listener.into_js_value()])?
            .dyn_into::<Function>()?;
        let remote = required(&effect_scope, "remote", "scoped Context")?;
        let reload_seat = effect_seat.clone();
        let settings_listener = Closure::wrap(Box::new(move |namespace: String| {
            if namespace == AGENT_PRESET_SETTINGS_NS {
                let _ = call_method(&reload_seat, "load", &[]);
            }
        }) as Box<dyn FnMut(String)>);
        let stop_settings = call_method(
            &remote,
            "$on",
            &[
                JsValue::from_str("settings/document-updated"),
                settings_listener.into_js_value(),
            ],
        )?
        .dyn_into::<Function>()?;
        let echo_sessions = effect_sessions.clone();
        let selected_listener = Closure::wrap(Box::new(move |session: String, preset: String| {
            let _ = call_method(
                &echo_sessions,
                "noteAgentPreset",
                &[JsValue::from_str(&session), JsValue::from_str(&preset)],
            );
        }) as Box<dyn FnMut(String, String)>);
        let stop_selected = call_method(
            &remote,
            "$on",
            &[
                JsValue::from_str("agent-preset/selected"),
                selected_listener.into_js_value(),
            ],
        )?
        .dyn_into::<Function>()?;
        let read_seat = effect_seat.clone();
        let reader = Closure::wrap(Box::new(move || {
            let _ = call_method(&read_seat, "load", &[]);
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into::<Function>();
        effect_readers.borrow_mut().push(reader.clone());
        let creator_seat = effect_seat.clone();
        let workspaces = required(&effect_scope, "workspaces", "scoped Context")?;
        let creator = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(
                &creator_seat,
                "stage",
                &[JsValue::from_str("cordis"), JsValue::TRUE],
            )?;
            call_method(&workspaces, "startSession", &[])?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value()
        .unchecked_into::<Function>();
        *effect_creator.borrow_mut() = Some(creator.clone());
        let slots = required(&effect_scope, "slots", "scoped Context")?;
        let chip = call_method(
            &slots,
            "register",
            &[
                object(&[
                    ("name", JsValue::from_str("conversation.hero.agentPreset")),
                    ("locale", JsValue::from_str(NS)),
                    ("inject", constant_inject(effect_seat.clone())),
                ])?
                .into(),
                agent_preset_seat_component()?,
            ],
        )?
        .dyn_into::<Function>()?;
        let label = call_method(
            &slots,
            "register",
            &[
                object(&[
                    (
                        "name",
                        JsValue::from_str("conversation.session.header.actions"),
                    ),
                    ("id", JsValue::from_str("agent-preset")),
                    ("order", JsValue::from_f64(-10.0)),
                    ("locale", JsValue::from_str(NS)),
                    ("inject", constant_inject(effect_label.clone())),
                ])?
                .into(),
                agent_preset_label_component()?,
            ],
        )?
        .dyn_into::<Function>()?;
        let cleanup_readers = effect_readers.clone();
        let cleanup_creator = effect_creator.clone();
        Ok(Closure::wrap(Box::new(move || {
            let _ = stop_list.call0(&JsValue::UNDEFINED);
            let _ = stop_settings.call0(&JsValue::UNDEFINED);
            let _ = stop_selected.call0(&JsValue::UNDEFINED);
            cleanup_readers
                .borrow_mut()
                .retain(|candidate| !Object::is(candidate, &reader));
            *cleanup_creator.borrow_mut() = None;
            let _ = chip.call0(&JsValue::UNDEFINED);
            let _ = label.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        scope,
        "effect",
        &[
            install.into_js_value(),
            JsValue::from_str("ui-agent-preset: new-session chip and header label"),
        ],
    )?;
    Ok(())
}

fn remap_hook(face: &JsValue, from: &str, to: &str) -> Result<JsValue, JsValue> {
    let source = face.clone().dyn_into::<Object>()?;
    let output = Object::assign(&Object::new(), &source);
    let hooks = required(face, "hooks", "controller face")?;
    let mapped = object(&[(to, required(&hooks, from, "controller hooks")?)])?;
    Reflect::set(&output, &JsValue::from_str("hooks"), &mapped)?;
    Ok(output.into())
}

fn constant_inject(face: JsValue) -> JsValue {
    Closure::wrap(Box::new(move || face.clone()) as Box<dyn FnMut() -> JsValue>).into_js_value()
}

fn inject_registration(
    slots: &JsValue,
    declaration: &str,
    options: Object,
    component: JsValue,
) -> Result<(), JsValue> {
    let slots_owner = slots.clone();
    let install = Closure::wrap(Box::new(move || {
        call_method(
            &slots_owner,
            "register",
            &[options.clone().into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        slots,
        "inject",
        &[JsValue::from_str(declaration), install.into_js_value()],
    )?;
    Ok(())
}
