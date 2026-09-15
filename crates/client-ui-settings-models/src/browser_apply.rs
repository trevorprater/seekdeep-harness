//! Browser plugin assembly and mutation-owning Models settings surfaces.

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Function, JSON, Map, Object, Promise, Reflect, Set};
use serde_json::Value;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
    ApiKeyFailureKey, SettingsPathOp,
    api_key::trim_ecmascript_whitespace,
    api_key_failure,
    browser::{
        call_async, call_method, class, create_element, css, object, optional, rejection_text,
        required, required_function, tag, translated,
    },
    browser_components::{
        BrowserModules, configured_components, configured_modules,
        deepseek_models_editor_component, editor_footer_component, model_list_editor_component,
    },
    browser_store::{
        create_models_settings_controller, create_welcome_notice_controller,
        refresh_models_if_loaded, refresh_welcome_if_loaded,
    },
    derive_key_ref, path_ops, provider_copy, route_valid, trim_api_key, validate_models,
};

const NS: &str = "settings.models";
const INJECT: &[&str] = &["slots", "locale", "connection", "remote"];
const LOCALES: &str = include_str!("../data/models-locales.json");

/// Registers Models settings, welcome, and official-provider onboarding surfaces.
///
/// # Errors
///
/// Returns for missing services, malformed browser modules, locale/effect failures, or Slot
/// registration failures.
#[wasm_bindgen(js_name = applyClientUiSettingsModels)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)] // Three source registrations share one controller graph.
pub fn apply_client_ui_settings_models(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let components = configured_components()?;
    let locale = required(&ctx, "locale", "Client Context")?;
    let slots = required(&ctx, "slots", "Client Context")?;
    let remote = required(&ctx, "remote", "Client Context")?;
    let connection = call_method(&ctx, "get", &[JsValue::from_str("connection")])?;
    let api = required(&connection, "api", "Connection handle")?;
    let dictionaries = JSON::parse(LOCALES)?;
    let own_locale = locale.clone();
    let install_locale = Closure::wrap(Box::new(move || {
        call_method(
            &own_locale,
            "register",
            &[JsValue::from_str(NS), dictionaries.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &ctx,
        "effect",
        &[
            install_locale.into_js_value(),
            JsValue::from_str("ui-settings-models: copy dictionaries"),
        ],
    )?;

    let controller = create_models_settings_controller(api.clone())?;
    let use_snapshot = modules.bind_snapshot_selector.call1(
        &JsValue::UNDEFINED,
        &required(&controller, "store", "Models controller")?,
    )?;
    let translate = call_method(&locale, "bind", &[JsValue::from_str(NS)])?;
    let welcome = create_welcome_notice_controller(
        api.clone(),
        if required(&connection, "isLoopback", "Connection handle")?
            .as_bool()
            .unwrap_or(false)
        {
            "host".to_owned()
        } else {
            "memory".to_owned()
        },
    )?;
    own_invalidations(&ctx, &remote, &controller, &welcome)?;

    let section_controller = controller.clone();
    let section_snapshot = use_snapshot;
    let section_api = api.clone();
    let section_translate = translate.clone();
    let section_inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        object(&[
            ("controller", section_controller.clone()),
            ("useSnapshot", section_snapshot.clone()),
            ("api", section_api.clone()),
            ("t", section_translate.clone()),
        ])
        .map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let label_translate = translate.clone();
    let label = Closure::wrap(Box::new(move || {
        required_function(&label_translate, "call", "Models translate")?.call2(
            &label_translate,
            &JsValue::UNDEFINED,
            &JsValue::from_str("nav"),
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    inject_registration(
        &slots,
        "settings.section",
        object(&[
            ("name", JsValue::from_str("settings.section")),
            ("id", JsValue::from_str("models")),
            ("order", JsValue::from_f64(10.0)),
            ("label", label.into_js_value()),
            ("inject", section_inject.into_js_value()),
        ])?,
        components.models_section,
    )?;

    let welcome_controller = welcome.clone();
    let welcome_store = required(&welcome, "store", "Welcome controller")?;
    let welcome_translate = translate.clone();
    let welcome_inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        object(&[
            ("controller", welcome_controller.clone()),
            (
                "hooks",
                object(&[("welcome", welcome_store.clone())])?.into(),
            ),
            ("t", welcome_translate.clone()),
        ])
        .map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    inject_registration(
        &slots,
        "settings.onboarding",
        object(&[
            ("name", JsValue::from_str("settings.onboarding")),
            ("id", JsValue::from_str("welcome-notice")),
            ("order", JsValue::from_f64(-100.0)),
            ("inject", welcome_inject.into_js_value()),
        ])?,
        components.welcome_notice,
    )?;

    let onboarding_controller = controller.clone();
    let onboarding_store = required(&controller, "store", "Models controller")?;
    let onboarding_api = api;
    let onboarding_translate = translate;
    let onboarding_inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        object(&[
            ("controller", onboarding_controller.clone()),
            (
                "hooks",
                object(&[("models", onboarding_store.clone())])?.into(),
            ),
            ("api", onboarding_api.clone()),
            ("t", onboarding_translate.clone()),
        ])
        .map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    inject_registration(
        &slots,
        "settings.onboarding",
        object(&[
            ("name", JsValue::from_str("settings.onboarding")),
            ("id", JsValue::from_str("deepseek-official")),
            ("order", JsValue::from_f64(0.0)),
            ("inject", onboarding_inject.into_js_value()),
        ])?,
        components.deepseek_onboarding,
    )
}

/// Returns the exact browser service dependencies.
#[wasm_bindgen(js_name = settingsModelsInject)]
pub fn settings_models_inject() -> Array {
    let values = Array::new();
    for value in INJECT {
        values.push(&JsValue::from_str(value));
    }
    values
}

fn own_invalidations(
    ctx: &JsValue,
    remote: &JsValue,
    models: &JsValue,
    welcome: &JsValue,
) -> Result<(), JsValue> {
    let remote = remote.clone();
    let ctx_events = ctx.clone();
    let models = models.clone();
    let welcome = welcome.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let disposers = Array::new();
        let settings_models = models.clone();
        let settings_welcome = welcome.clone();
        let settings = Closure::wrap(Box::new(move |namespace: JsValue| {
            let _ = refresh_models_if_loaded(settings_models.clone());
            if namespace.as_string().as_deref() == Some(crate::WELCOME_NOTICE_SETTINGS_NAMESPACE) {
                let _ = refresh_welcome_if_loaded(settings_welcome.clone());
            }
        }) as Box<dyn FnMut(JsValue)>);
        disposers.push(&call_method(
            &remote,
            "$on",
            &[
                JsValue::from_str("settings/document-updated"),
                settings.into_js_value(),
            ],
        )?);
        for event in ["credentials/updated", "llm/adapters-updated"] {
            let controller = models.clone();
            let refresh = Closure::wrap(Box::new(move || {
                let _ = refresh_models_if_loaded(controller.clone());
            }) as Box<dyn FnMut()>);
            disposers.push(&call_method(
                &remote,
                "$on",
                &[JsValue::from_str(event), refresh.into_js_value()],
            )?);
        }
        let reset_models = models.clone();
        let reset_welcome = welcome.clone();
        let reset = Closure::wrap(Box::new(move || {
            let _ = refresh_models_if_loaded(reset_models.clone());
            let _ = refresh_welcome_if_loaded(reset_welcome.clone());
        }) as Box<dyn FnMut()>);
        disposers.push(&call_method(
            &ctx_events,
            "on",
            &[JsValue::from_str("connection/reset"), reset.into_js_value()],
        )?);
        Ok(Closure::wrap(Box::new(move || {
            for disposer in disposers.iter() {
                if let Ok(disposer) = disposer.dyn_into::<Function>() {
                    let _ = disposer.call0(&JsValue::UNDEFINED);
                }
            }
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-settings-models: pushed invalidations"),
        ],
    )?;
    Ok(())
}

fn inject_registration(
    slots: &JsValue,
    declaration: &str,
    options: Object,
    component: JsValue,
) -> Result<(), JsValue> {
    let registration_slots = slots.clone();
    let install = Closure::wrap(Box::new(move || {
        call_method(
            &registration_slots,
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

#[allow(clippy::too_many_lines)] // The card owns one complete settings+credential transaction.
pub(crate) fn render_provider_editor_surface(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let provider = required(props, "provider", "ProviderEditor props")?
        .as_string()
        .unwrap_or_default();
    let display_name = required(props, "displayName", "ProviderEditor props")?
        .as_string()
        .unwrap_or_default();
    let namespace = required(props, "namespace", "ProviderEditor props")?;
    let settings_path = required(props, "settingsPath", "ProviderEditor props")?;
    let api = required(props, "api", "ProviderEditor props")?;
    let translate = required_function(props, "t", "ProviderEditor props")?;
    let user = optional(&namespace, "user")?.unwrap_or(JsValue::UNDEFINED);
    let original = schema_call(modules, "getPath", &[user, settings_path.clone()])?;
    let initial = if original.is_object() && !Array::is_array(&original) {
        clone_value(&original)?
    } else {
        Object::new().into()
    };
    let (draft, set_draft) = use_state(&modules.react, &initial)?;
    let (key_draft, set_key_draft) = use_state(&modules.react, &JsValue::from_str(""))?;
    let (key_state, set_key_state) = use_state(&modules.react, &JsValue::UNDEFINED)?;
    let (busy, set_busy) = use_state(&modules.react, &JsValue::FALSE)?;
    let (failure, set_failure) = use_state(&modules.react, &JsValue::UNDEFINED)?;
    let (committed, set_committed) = use_state(&modules.react, &original)?;
    let (revision, set_revision) = use_state(
        &modules.react,
        &required(&namespace, "revision", "settings namespace")?,
    )?;
    let root = schema_call(
        modules,
        "rehydrateSchema",
        &[required(&namespace, "schema", "settings namespace")?],
    )?;
    let node = schema_call(
        modules,
        "nodeAtPath",
        &[root.clone(), settings_path.clone()],
    )?;
    let fallback = schema_call(
        modules,
        "getPath",
        &[
            required(&namespace, "value", "settings namespace")?,
            settings_path.clone(),
        ],
    )?;
    let namespace_name = required(&namespace, "ns", "settings namespace")?
        .as_string()
        .unwrap_or_default();
    let layout = match namespace_name.as_str() {
        "llm-deepseek" => "deepseek",
        "llm-pi-ai" => "pi-ai",
        _ => "unknown",
    };
    let uses_codex = optional(props, "authentication")?
        .and_then(|value| value.as_string())
        .as_deref()
        == Some("codex-oauth");
    let key_ref = profile_string(modules, &fallback, "apiKeyEnv")?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| derive_key_ref(&provider));
    install_credential_effect(modules, &api, &key_ref, uses_codex, set_key_state.clone())?;
    let busy_bool = busy.as_bool().unwrap_or(false);
    let read_only = optional(props, "readOnly")?
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let disabled = read_only || busy_bool;
    let key_text = key_draft.as_string().unwrap_or_default();
    let key_value = if uses_codex {
        String::new()
    } else {
        trim_api_key(&key_text).to_owned()
    };
    let credential_required = optional(props, "credentialRequired")?
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let mut key_failure = if uses_codex {
        None
    } else {
        api_key_failure(&key_text)
    };
    if credential_required && !key_text.is_empty() && key_value.is_empty() {
        key_failure = Some(ApiKeyFailureKey::KeyBlank);
    }
    let models = schema_call(
        modules,
        "getPath",
        &[
            draft.clone(),
            Array::of1(&JsValue::from_str("models")).into(),
        ],
    )?;
    let validation = js_models_validation(&models)?;
    let credential_only = optional(props, "credentialOnly")?
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if node.is_null() || node.is_undefined() {
        return tag(
            &modules.react,
            "p",
            Some(&class(&css("error"))?),
            &[JsValue::from_str(&format!(
                "{provider}: unresolvable settings path"
            ))],
        );
    }

    let apply_api = api.clone();
    let apply_namespace = namespace.clone();
    let apply_path = settings_path.clone();
    let apply_draft = draft.clone();
    let apply_fallback = fallback.clone();
    let apply_node = node;
    let apply_provider = provider.clone();
    let apply_layout = layout.to_owned();
    let apply_ref = key_ref.clone();
    let apply_key = key_value.clone();
    let apply_original = committed.clone();
    let apply_revision = revision.clone();
    let apply_translate = translate.clone();
    let apply_close = required_function(props, "onClose", "ProviderEditor props")?;
    let apply_set_busy = set_busy;
    let apply_set_failure = set_failure;
    let apply_set_committed = set_committed;
    let apply_set_revision = set_revision;
    let apply_set_draft = set_draft.clone();
    let apply_set_key = set_key_draft.clone();
    let apply_uses_codex = uses_codex;
    let apply_credential_only = credential_only;
    let submit = Closure::wrap(Box::new(move || {
        let api = apply_api.clone();
        let namespace = apply_namespace.clone();
        let settings_path = apply_path.clone();
        let draft = apply_draft.clone();
        let fallback = apply_fallback.clone();
        let node = apply_node.clone();
        let provider = apply_provider.clone();
        let layout = apply_layout.clone();
        let key_ref = apply_ref.clone();
        let key_value = apply_key.clone();
        let original = apply_original.clone();
        let revision = apply_revision.clone();
        let translate = apply_translate.clone();
        let close = apply_close.clone();
        let set_busy = apply_set_busy.clone();
        let set_failure = apply_set_failure.clone();
        let set_committed = apply_set_committed.clone();
        let set_revision = apply_set_revision.clone();
        let set_draft = apply_set_draft.clone();
        let set_key = apply_set_key.clone();
        let _ = set_busy.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
        let _ = set_failure.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
        spawn_local(async move {
            let result = apply_provider_transaction(
                &api,
                &namespace,
                &settings_path,
                &draft,
                &fallback,
                &node,
                &provider,
                &layout,
                &key_ref,
                &key_value,
                &original,
                &revision,
                &translate,
                &set_committed,
                &set_revision,
                &set_draft,
                apply_uses_codex,
                apply_credential_only,
            )
            .await;
            match result {
                Ok(()) => {
                    let _ = set_key.call1(&JsValue::UNDEFINED, &JsValue::from_str(""));
                    let _ = close.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
                }
                Err(error) => {
                    let _ = set_failure.call1(&JsValue::UNDEFINED, &JsValue::from_str(&error));
                }
            }
            let _ = set_busy.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
        });
    }) as Box<dyn FnMut()>);

    let mut children = Vec::new();
    if optional(props, "hideTitle")?.and_then(|value| value.as_bool()) != Some(true) {
        let mut header = vec![tag(
            &modules.react,
            "span",
            Some(&class(&css("editorTitle"))?),
            &[JsValue::from_str(&display_name)],
        )?];
        if provider != display_name {
            header.push(tag(
                &modules.react,
                "span",
                Some(&class(&css("editorRoute"))?),
                &[JsValue::from_str(&provider)],
            )?);
        }
        children.push(tag(
            &modules.react,
            "div",
            Some(&class(&css("editorHeader"))?),
            &header,
        )?);
    }
    if layout == "unknown" {
        children.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("advancedHint"))?),
            &[JsValue::from_str(&format!(
                "{} ({namespace_name})",
                translated(&translate, "advancedHint")?
                    .as_string()
                    .unwrap_or_default()
            ))],
        )?);
    } else {
        children.extend(provider_fields(
            modules,
            props,
            &namespace,
            &settings_path,
            &draft,
            &fallback,
            &key_state,
            &set_draft,
            key_draft,
            set_key_draft,
            disabled,
            layout,
            uses_codex,
            key_failure,
            credential_required,
            credential_only,
        )?);
    }
    if !failure.is_undefined() {
        children.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("error"))?),
            &[failure],
        )?);
    }
    if !credential_only && let Some((index, key)) = validation {
        children.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("advancedHint"))?),
            &[JsValue::from_str(&format!(
                "{} {}: {}",
                translated(&translate, "model")?
                    .as_string()
                    .unwrap_or_default(),
                index + 1,
                translated(&translate, key)?.as_string().unwrap_or_default()
            ))],
        )?);
    }
    let cancel = required_function(props, "onClose", "ProviderEditor props")?;
    let cancel = Closure::wrap(Box::new(move || {
        let _ = cancel.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
    }) as Box<dyn FnMut()>);
    let submit_disabled = disabled
        || layout == "unknown"
        || (!credential_only && validation.is_some())
        || key_failure.is_some()
        || (credential_required && key_value.is_empty());
    children.push(create_element(
        &modules.react,
        &editor_footer_component()?,
        Some(&object(&[
            ("t", translate.into()),
            ("busy", JsValue::from_bool(busy_bool)),
            ("submitDisabled", JsValue::from_bool(submit_disabled)),
            (
                "submitLabel",
                optional(props, "submitLabel")?.unwrap_or(JsValue::from_str("apply")),
            ),
            (
                "submitBusyLabel",
                optional(props, "submitBusyLabel")?.unwrap_or(JsValue::from_str("applying")),
            ),
            (
                "cancelLabel",
                optional(props, "cancelLabel")?.unwrap_or(JsValue::UNDEFINED),
            ),
            ("onCancel", cancel.into_js_value()),
            ("onSubmit", submit.into_js_value()),
        ])?),
        &[],
    )?);
    tag(
        &modules.react,
        "div",
        Some(&class(&css(if credential_only {
            "addBlock"
        } else {
            "editor"
        }))?),
        &children,
    )
}

#[allow(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines
)] // The closed provider-field posture mirrors one source component.
fn provider_fields(
    modules: &BrowserModules,
    props: &JsValue,
    namespace: &JsValue,
    settings_path: &JsValue,
    draft: &JsValue,
    fallback: &JsValue,
    key_state: &JsValue,
    set_draft: &Function,
    key_draft: JsValue,
    set_key_draft: Function,
    disabled: bool,
    layout: &str,
    uses_codex: bool,
    key_failure: Option<ApiKeyFailureKey>,
    credential_required: bool,
    credential_only: bool,
) -> Result<Vec<JsValue>, JsValue> {
    let translate = required_function(props, "t", "ProviderEditor props")?;
    let key_text = key_draft.as_string().unwrap_or_default();
    let probe_key = if uses_codex {
        None
    } else {
        Some(trim_api_key(&key_text).to_owned()).filter(|value| !value.is_empty())
    };
    let mut children = Vec::new();
    if uses_codex {
        children.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("hint"))?),
            &[translated(&translate, "codexOAuth")?],
        )?);
    } else {
        let key_locked =
            optional(key_state, "writable")?.and_then(|value| value.as_bool()) == Some(false);
        let placeholder = if key_locked {
            "keyEnvLocked"
        } else if optional(key_state, "configured")?.and_then(|value| value.as_bool()) == Some(true)
            && !credential_required
        {
            "keyStored"
        } else if layout == "pi-ai" {
            "keyPlaceholderNative"
        } else {
            "keyPlaceholder"
        };
        let set_key = set_key_draft;
        let change = Closure::wrap(Box::new(move |event: JsValue| {
            if let Ok(value) = event_value(&event) {
                let _ = set_key.call1(&JsValue::UNDEFINED, &JsValue::from_str(&value));
            }
        }) as Box<dyn FnMut(JsValue)>);
        let mut field = vec![
            tag(
                &modules.react,
                "span",
                Some(&class(&css("fieldLabel"))?),
                &[translated(&translate, "keyInput")?],
            )?,
            tag(
                &modules.react,
                "input",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("input"))),
                    ("type", JsValue::from_str("password")),
                    ("autoComplete", JsValue::from_str("off")),
                    ("value", key_draft),
                    ("placeholder", translated(&translate, placeholder)?),
                    ("aria-label", translated(&translate, "keyInput")?),
                    ("aria-invalid", JsValue::from_bool(key_failure.is_some())),
                    ("required", JsValue::from_bool(credential_required)),
                    (
                        "autoFocus",
                        JsValue::from_bool(
                            optional(props, "autoFocusCredential")?
                                .and_then(|value| value.as_bool())
                                == Some(true),
                        ),
                    ),
                    ("disabled", JsValue::from_bool(disabled || key_locked)),
                    ("onChange", change.into_js_value()),
                ])?),
                &[],
            )?,
        ];
        if let Some(key) = key_failure {
            field.push(tag(
                &modules.react,
                "p",
                Some(&class(&css("error"))?),
                &[translated(
                    &translate,
                    if credential_required && key == ApiKeyFailureKey::KeyBlank {
                        "keyRequired"
                    } else {
                        match key {
                            ApiKeyFailureKey::KeyBlank => "keyBlank",
                            ApiKeyFailureKey::KeyIllegalCharacters => "keyIllegalCharacters",
                        }
                    },
                )?],
            )?);
        }
        children.push(tag(
            &modules.react,
            "div",
            Some(&class(&css("field"))?),
            &field,
        )?);
    }
    if credential_only {
        return Ok(children);
    }
    let base_url = profile_string(modules, draft, "baseURL")?.unwrap_or_default();
    let probe_base_url = if base_url.is_empty() {
        profile_string(modules, fallback, "baseURL")?
    } else {
        Some(base_url.clone())
    };
    let probe_api = match profile_string(modules, draft, "api")? {
        Some(api) => Some(api),
        None => profile_string(modules, fallback, "api")?,
    };
    let update_draft = set_draft.clone();
    let current_draft = draft.clone();
    let base_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let value = event_value(&event)?;
        let next = if value.is_empty() || trim_ecmascript_whitespace(&value).is_empty() {
            schema_call(
                &configured_modules()?,
                "deletePath",
                &[
                    current_draft.clone(),
                    Array::of1(&JsValue::from_str("baseURL")).into(),
                ],
            )?
        } else {
            schema_call(
                &configured_modules()?,
                "setPath",
                &[
                    current_draft.clone(),
                    Array::of1(&JsValue::from_str("baseURL")).into(),
                    JsValue::from_str(&value),
                ],
            )?
        };
        update_draft.call1(&JsValue::UNDEFINED, &next)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let models = schema_call(
        modules,
        "getPath",
        &[
            draft.clone(),
            Array::of1(&JsValue::from_str("models")).into(),
        ],
    )?;
    let overridden = schema_call(
        modules,
        "hasPath",
        &[
            draft.clone(),
            Array::of1(&JsValue::from_str("models")).into(),
        ],
    )?
    .as_bool()
    .unwrap_or(false);
    let inherited_path = append_path(settings_path, "models")?;
    let inherited = schema_call(
        modules,
        "getPath",
        &[
            optional(namespace, "base")?.unwrap_or(JsValue::UNDEFINED),
            inherited_path.clone(),
        ],
    )?;
    let inherited = if !inherited.is_null() && !inherited.is_undefined() {
        inherited
    } else {
        let root = schema_call(
            modules,
            "rehydrateSchema",
            &[required(namespace, "schema", "settings namespace")?],
        )?;
        let node = schema_call(modules, "nodeAtPath", &[root, inherited_path])?;
        optional(
            &optional(&node, "meta")?.unwrap_or(JsValue::UNDEFINED),
            "default",
        )?
        .unwrap_or(JsValue::UNDEFINED)
    };
    let model_rows = model_drafts_array(if overridden { &models } else { &inherited });
    let change_draft = draft.clone();
    let change_setter = set_draft.clone();
    let model_change = Closure::wrap(Box::new(move |models: JsValue| -> Result<(), JsValue> {
        let next = schema_call(
            &configured_modules()?,
            "setPath",
            &[
                change_draft.clone(),
                Array::of1(&JsValue::from_str("models")).into(),
                models,
            ],
        )?;
        change_setter.call1(&JsValue::UNDEFINED, &next)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let reset_draft = draft.clone();
    let reset_setter = set_draft.clone();
    let reset = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let next = schema_call(
            &configured_modules()?,
            "deletePath",
            &[
                reset_draft.clone(),
                Array::of1(&JsValue::from_str("models")).into(),
            ],
        )?;
        reset_setter.call1(&JsValue::UNDEFINED, &next)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let editor = if layout == "deepseek" {
        deepseek_models_editor_component()?
    } else {
        model_list_editor_component()?
    };
    let mut catalog = vec![
        ("models", model_rows),
        ("overridden", JsValue::from_bool(overridden)),
        ("t", translate.clone().into()),
        ("disabled", JsValue::from_bool(disabled)),
        ("onChange", model_change.into_js_value()),
        ("onReset", reset.into_js_value()),
    ];
    if layout == "deepseek" {
        catalog.extend([
            (
                "defaultContextWindow",
                schema_call(
                    modules,
                    "getPath",
                    &[
                        fallback.clone(),
                        Array::of1(&JsValue::from_str("defaultContextWindow")).into(),
                    ],
                )?,
            ),
            (
                "defaultMaxTokens",
                schema_call(
                    modules,
                    "getPath",
                    &[
                        fallback.clone(),
                        Array::of1(&JsValue::from_str("maxTokens")).into(),
                    ],
                )?,
            ),
        ]);
    } else {
        let probe = object(&[
            (
                "settingsNs",
                required(namespace, "ns", "settings namespace")?,
            ),
            (
                "provider",
                required(props, "provider", "ProviderEditor props")?,
            ),
            (
                "baseURL",
                probe_base_url.map_or(JsValue::UNDEFINED, |value| JsValue::from_str(&value)),
            ),
            (
                "api",
                probe_api.map_or(JsValue::UNDEFINED, |value| JsValue::from_str(&value)),
            ),
            (
                "apiKey",
                probe_key.map_or(JsValue::UNDEFINED, |value| JsValue::from_str(&value)),
            ),
        ])?;
        catalog.extend([
            ("probe", probe.into()),
            (
                "probeBlocked",
                key_failure.map_or(JsValue::UNDEFINED, |failure| {
                    JsValue::from_str(match failure {
                        ApiKeyFailureKey::KeyBlank => "keyBlank",
                        ApiKeyFailureKey::KeyIllegalCharacters => "keyIllegalCharacters",
                    })
                }),
            ),
            ("api", required(props, "api", "ProviderEditor props")?),
        ]);
    }
    let catalog = create_element(&modules.react, &editor, Some(&object(&catalog)?), &[])?;
    let mut customized_body = Vec::new();
    let declared = layout == "pi-ai"
        && optional(props, "declared")?.and_then(|value| value.as_bool()) == Some(true);
    if declared {
        let display_name = profile_string(modules, draft, "displayName")?.unwrap_or_default();
        let display_placeholder = profile_string(
            modules,
            &schema_call(
                modules,
                "getPath",
                &[
                    optional(namespace, "base")?.unwrap_or(JsValue::UNDEFINED),
                    settings_path.clone(),
                ],
            )?,
            "displayName",
        )?
        .unwrap_or_else(|| {
            required(props, "provider", "ProviderEditor props")
                .ok()
                .and_then(|value| value.as_string())
                .unwrap_or_default()
        });
        customized_body.push(text_draft_field(
            modules,
            &translate,
            "customDisplayName",
            &display_name,
            &display_placeholder,
            disabled,
            draft,
            set_draft,
            "displayName",
        )?);
    }
    customized_body.push(tag(
        &modules.react,
        "div",
        Some(&class(&css("field"))?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&class(&css("fieldLabel"))?),
                &[translated(&translate, "baseUrl")?],
            )?,
            tag(
                &modules.react,
                "input",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("input"))),
                    ("type", JsValue::from_str("text")),
                    ("value", JsValue::from_str(&base_url)),
                    (
                        "placeholder",
                        if layout == "deepseek" {
                            JsValue::from_str("https://api.deepseek.com")
                        } else {
                            profile_string(modules, fallback, "baseURL")?
                                .map_or(translated(&translate, "baseUrlDefault")?, |value| {
                                    JsValue::from_str(&value)
                                })
                        },
                    ),
                    ("aria-label", translated(&translate, "baseUrl")?),
                    ("disabled", JsValue::from_bool(disabled)),
                    ("onChange", base_change.into_js_value()),
                ])?),
                &[],
            )?,
        ],
    )?);
    if declared {
        let protocols = protocol_choices(modules, namespace)?;
        let selected = match profile_string(modules, draft, "api")? {
            Some(api) => Some(api),
            None => profile_string(modules, fallback, "api")?,
        }
        .unwrap_or_default();
        customized_body.push(draft_select_field(
            modules,
            &translate,
            "customApi",
            &selected,
            &protocols,
            disabled,
            draft,
            set_draft,
            "api",
        )?);
    }
    customized_body.push(catalog);
    children.push(tag(
        &modules.react,
        "details",
        Some(&class(&css("customized"))?),
        &[
            tag(
                &modules.react,
                "summary",
                Some(&class(&css("customizedSummary"))?),
                &[translated(&translate, "customized")?],
            )?,
            tag(
                &modules.react,
                "div",
                Some(&class(&css("customizedBody"))?),
                &customized_body,
            )?,
        ],
    )?);
    Ok(children)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn apply_provider_transaction(
    api: &JsValue,
    namespace: &JsValue,
    settings_path: &JsValue,
    draft: &JsValue,
    fallback: &JsValue,
    node: &JsValue,
    _provider: &str,
    layout: &str,
    key_ref: &str,
    key_value: &str,
    original: &JsValue,
    revision: &JsValue,
    translate: &Function,
    set_committed: &Function,
    set_revision: &Function,
    set_draft: &Function,
    uses_codex: bool,
    credential_only: bool,
) -> Result<(), String> {
    let mut next = draft.clone();
    if !uses_codex
        && layout == "pi-ai"
        && profile_string(&configured_modules().map_err(js_text)?, &next, "apiKeyEnv")
            .map_err(js_text)?
            .is_none()
        && profile_string(
            &configured_modules().map_err(js_text)?,
            fallback,
            "apiKeyEnv",
        )
        .map_err(js_text)?
        .is_none()
        && !key_value.is_empty()
    {
        next = schema_call(
            &configured_modules().map_err(js_text)?,
            "setPath",
            &[
                next,
                Array::of1(&JsValue::from_str("apiKeyEnv")).into(),
                JsValue::from_str(key_ref),
            ],
        )
        .map_err(js_text)?;
    }
    if !credential_only {
        let modules = configured_modules().map_err(js_text)?;
        let models = schema_call(
            &modules,
            "getPath",
            &[
                next.clone(),
                Array::of1(&JsValue::from_str("models")).into(),
            ],
        )
        .map_err(js_text)?;
        if let Some((index, key)) = js_models_validation(&models).map_err(js_text)? {
            let model = translated(translate, "model")
                .map_err(js_text)?
                .as_string()
                .unwrap_or_default();
            let failure = translated(translate, key)
                .map_err(js_text)?
                .as_string()
                .unwrap_or_default();
            return Err(format!("{model} {}: {failure}", index + 1));
        }
    }
    if !credential_only
        && settings_path
            .dyn_ref::<Array>()
            .is_some_and(|path| path.length() == 0)
    {
        let validation = schema_call(
            &configured_modules().map_err(js_text)?,
            "validateDraft",
            &[node.clone(), next.clone()],
        )
        .map_err(js_text)?;
        if let Some(error) = validation.as_string() {
            return Err(error);
        }
    }
    let operations = if credential_only {
        Array::new()
    } else {
        settings_ops(settings_path, original, &next).map_err(js_text)?
    };
    if operations.length() == 0
        && layout == "pi-ai"
        && fallback.is_undefined()
        && original.is_undefined()
        && Object::keys(&Object::from(next.clone())).length() == 0
    {
        operations.push(
            &object(&[
                ("op", JsValue::from_str("set")),
                ("path", settings_path.clone()),
                ("value", Object::new().into()),
            ])
            .map_err(js_text)?
            .into(),
        );
    }
    if operations.length() > 0 {
        let settings = required(api, "settings", "API client").map_err(js_text)?;
        let response = call_async(
            &settings,
            "mutate",
            &[object(&[
                (
                    "ns",
                    required(namespace, "ns", "settings namespace").map_err(js_text)?,
                ),
                ("ops", operations.into()),
                ("expectedRevision", revision.clone()),
            ])
            .map_err(js_text)?
            .into()],
        )
        .await
        .map_err(|error| rejection_text(&error))?;
        match rpc_value(&response) {
            Ok(value) => {
                commit_provider_update(
                    &value,
                    &next,
                    settings_path,
                    set_committed,
                    set_revision,
                    set_draft,
                )
                .map_err(js_text)?;
            }
            Err((code, message)) => {
                return Err(if code == "settings-conflict" {
                    translated(translate, "conflict")
                        .map_err(js_text)?
                        .as_string()
                        .unwrap_or_default()
                } else {
                    message
                });
            }
        }
    }
    if !uses_codex && !key_value.is_empty() {
        let credentials = required(api, "credentials", "API client").map_err(js_text)?;
        let response = call_async(
            &credentials,
            "set",
            &[object(&[
                ("ref", JsValue::from_str(key_ref)),
                ("value", JsValue::from_str(key_value)),
            ])
            .map_err(js_text)?
            .into()],
        )
        .await
        .map_err(|error| rejection_text(&error))?;
        if let Err((_code, message)) = rpc_value(&response) {
            return Err(message);
        }
    }
    Ok(())
}

fn commit_provider_update(
    response: &JsValue,
    next: &JsValue,
    settings_path: &JsValue,
    set_committed: &Function,
    set_revision: &Function,
    set_draft: &Function,
) -> Result<(), JsValue> {
    let user = optional(response, "user")?.unwrap_or(JsValue::UNDEFINED);
    let committed = schema_call(
        &configured_modules()?,
        "getPath",
        &[user, settings_path.clone()],
    )?;
    set_committed.call1(&JsValue::UNDEFINED, &committed)?;
    set_revision.call1(
        &JsValue::UNDEFINED,
        &required(response, "revision", "settings mutation")?,
    )?;
    set_draft.call1(&JsValue::UNDEFINED, next)?;
    Ok(())
}

#[allow(clippy::too_many_lines)] // Creation is a two-stage retryable profile/key transaction.
pub(crate) fn render_custom_provider_surface(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "CustomProviderCard props")?;
    let protocols =
        required(props, "protocols", "CustomProviderCard props")?.dyn_into::<Array>()?;
    let (opened_at, _set_opened_at) = use_state(
        &modules.react,
        &required(props, "revision", "CustomProviderCard props")?,
    )?;
    let (route, set_route) = use_state(&modules.react, &JsValue::from_str(""))?;
    let (display, set_display) = use_state(&modules.react, &JsValue::from_str(""))?;
    let (base_url, set_base_url) = use_state(&modules.react, &JsValue::from_str(""))?;
    let (protocol, set_protocol) = use_state(&modules.react, &protocols.get(0))?;
    let (key_draft, set_key) = use_state(&modules.react, &JsValue::from_str(""))?;
    let (models, set_models) = use_state(&modules.react, &Array::new().into())?;
    let (busy, set_busy) = use_state(&modules.react, &JsValue::FALSE)?;
    let (failure, set_failure) = use_state(&modules.react, &JsValue::UNDEFINED)?;
    let (committed, set_committed) = use_state(&modules.react, &JsValue::FALSE)?;
    let route_text = route.as_string().unwrap_or_default();
    let display_text = display.as_string().unwrap_or_default();
    let base_text = base_url.as_string().unwrap_or_default();
    let protocol_text = protocol.as_string().unwrap_or_default();
    let key_text = key_draft.as_string().unwrap_or_default();
    let key_value = trim_api_key(&key_text).to_owned();
    let busy_bool = busy.as_bool().unwrap_or(false);
    let read_only = required(props, "readOnly", "CustomProviderCard props")?
        .as_bool()
        .unwrap_or(false);
    let disabled = read_only || busy_bool;
    let profile_disabled = disabled || committed.as_bool() == Some(true);
    let taken = required(props, "taken", "CustomProviderCard props")?.dyn_into::<Array>()?;
    let invalid = !route_text.is_empty() && !route_valid(&route_text);
    let taken_route = taken
        .iter()
        .any(|value| value.as_string().as_deref() == Some(&route_text));
    let model_validation = js_models_validation(&models)?;
    let key_failure = api_key_failure(&key_text);
    let ready = !route_text.is_empty()
        && !invalid
        && !taken_route
        && !base_text.is_empty()
        && models
            .dyn_ref::<Array>()
            .is_some_and(|models| models.length() > 0)
        && model_validation.is_none()
        && key_failure.is_none();
    let api = required(props, "api", "CustomProviderCard props")?;
    let close = required_function(props, "onClose", "CustomProviderCard props")?;
    let submit_api = api.clone();
    let submit_route = route_text.clone();
    let submit_display = display_text.clone();
    let submit_base = base_text.clone();
    let submit_protocol = protocol_text.clone();
    let submit_key = key_value.clone();
    let submit_models = models.clone();
    let submit_revision = opened_at;
    let submit_committed = committed.as_bool() == Some(true);
    let submit_set_committed = set_committed.clone();
    let submit_set_busy = set_busy;
    let submit_set_failure = set_failure;
    let submit_close = close.clone();
    let submit = Closure::wrap(Box::new(move || {
        let api = submit_api.clone();
        let route = submit_route.clone();
        let display = submit_display.clone();
        let base = submit_base.clone();
        let protocol = submit_protocol.clone();
        let key = submit_key.clone();
        let models = submit_models.clone();
        let revision = submit_revision.clone();
        let set_committed = submit_set_committed.clone();
        let set_busy = submit_set_busy.clone();
        let set_failure = submit_set_failure.clone();
        let close = submit_close.clone();
        let _ = set_busy.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
        let _ = set_failure.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
        spawn_local(async move {
            let result = create_provider_transaction(
                &api,
                &route,
                &display,
                &base,
                &protocol,
                &key,
                &models,
                &revision,
                submit_committed,
                &set_committed,
            )
            .await;
            match result {
                Ok(()) => {
                    let _ = close.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
                }
                Err(error) => {
                    let _ = set_failure.call1(&JsValue::UNDEFINED, &JsValue::from_str(&error));
                }
            }
            let _ = set_busy.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
        });
    }) as Box<dyn FnMut()>);
    let mut children = vec![tag(
        &modules.react,
        "div",
        Some(&class(&css("editorHeader"))?),
        &[tag(
            &modules.react,
            "span",
            Some(&class(&css("editorTitle"))?),
            &[translated(&translate, "customTitle")?],
        )?],
    )?];
    children.extend(custom_text_field(
        modules,
        &translate,
        "customRoute",
        &route,
        "acme-gateway",
        profile_disabled,
        set_route,
    )?);
    children.push(tag(
        &modules.react,
        "p",
        Some(&class(&css(if invalid || taken_route {
            "error"
        } else {
            "advancedHint"
        }))?),
        &[translated(
            &translate,
            if invalid {
                "customRouteInvalid"
            } else if taken_route {
                "customRouteTaken"
            } else {
                "customRouteHint"
            },
        )?],
    )?);
    children.extend(custom_text_field(
        modules,
        &translate,
        "customDisplayName",
        &display,
        if route_text.is_empty() {
            ""
        } else {
            &route_text
        },
        profile_disabled,
        set_display,
    )?);
    children.extend(custom_text_field(
        modules,
        &translate,
        "baseUrl",
        &base_url,
        "https://gateway.example/v1",
        profile_disabled,
        set_base_url,
    )?);
    children.push(select_field(
        modules,
        &translate,
        "customApi",
        &protocol,
        &protocols,
        profile_disabled,
        set_protocol,
    )?);
    let key_change = state_text_change(set_key);
    let mut key_children = vec![
        tag(
            &modules.react,
            "span",
            Some(&class(&css("fieldLabel"))?),
            &[translated(&translate, "keyInput")?],
        )?,
        tag(
            &modules.react,
            "input",
            Some(&object(&[
                ("className", JsValue::from_str(&css("input"))),
                ("type", JsValue::from_str("password")),
                ("autoComplete", JsValue::from_str("off")),
                ("value", key_draft),
                ("placeholder", translated(&translate, "keyPlaceholder")?),
                ("aria-label", translated(&translate, "keyInput")?),
                ("disabled", JsValue::from_bool(disabled)),
                ("onChange", key_change),
            ])?),
            &[],
        )?,
    ];
    if let Some(key_failure) = key_failure {
        key_children.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("error"))?),
            &[translated(
                &translate,
                match key_failure {
                    ApiKeyFailureKey::KeyBlank => "keyBlankNew",
                    ApiKeyFailureKey::KeyIllegalCharacters => "keyIllegalCharacters",
                },
            )?],
        )?);
    }
    children.push(tag(
        &modules.react,
        "div",
        Some(&class(&css("field"))?),
        &key_children,
    )?);
    let model_change = set_models;
    children.push(create_element(
        &modules.react,
        &model_list_editor_component()?,
        Some(&object(&[
            ("models", models),
            ("onChange", model_change.into()),
            (
                "probe",
                object(&[
                    ("settingsNs", JsValue::from_str("llm-pi-ai")),
                    ("baseURL", JsValue::from_str(&base_text)),
                    ("api", JsValue::from_str(&protocol_text)),
                    (
                        "apiKey",
                        if key_value.is_empty() {
                            JsValue::UNDEFINED
                        } else {
                            JsValue::from_str(&key_value)
                        },
                    ),
                ])?
                .into(),
            ),
            (
                "probeBlocked",
                key_failure.map_or(JsValue::UNDEFINED, |failure| {
                    JsValue::from_str(match failure {
                        ApiKeyFailureKey::KeyBlank => "keyBlankNew",
                        ApiKeyFailureKey::KeyIllegalCharacters => "keyIllegalCharacters",
                    })
                }),
            ),
            ("api", api),
            ("t", translate.clone().into()),
            ("disabled", JsValue::from_bool(profile_disabled)),
        ])?),
        &[],
    )?);
    if !failure.is_undefined() {
        children.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("error"))?),
            &[failure],
        )?);
    } else if key_failure.is_none() && !route_text.is_empty() && !invalid && !taken_route && !ready
    {
        let hint = if base_text.is_empty() {
            translated(&translate, "customNeedsBaseUrl")?
        } else if let Some((index, key)) = model_validation {
            JsValue::from_str(&format!(
                "{} {}: {}",
                translated(&translate, "model")?
                    .as_string()
                    .unwrap_or_default(),
                index + 1,
                translated(&translate, key)?.as_string().unwrap_or_default()
            ))
        } else {
            translated(&translate, "customNeedsModels")?
        };
        children.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("advancedHint"))?),
            &[hint],
        )?);
    }
    let cancel_committed = committed.as_bool().unwrap_or(false);
    let cancel = Closure::wrap(Box::new(move || {
        let _ = close.call1(&JsValue::UNDEFINED, &JsValue::from_bool(cancel_committed));
    }) as Box<dyn FnMut()>);
    children.push(create_element(
        &modules.react,
        &editor_footer_component()?,
        Some(&object(&[
            ("t", translate.into()),
            ("busy", JsValue::from_bool(busy_bool)),
            ("submitDisabled", JsValue::from_bool(disabled || !ready)),
            ("submitLabel", JsValue::from_str("create")),
            ("submitBusyLabel", JsValue::from_str("creating")),
            ("onCancel", cancel.into_js_value()),
            ("onSubmit", submit.into_js_value()),
        ])?),
        &[],
    )?);
    tag(
        &modules.react,
        "div",
        Some(&class(&css("editor"))?),
        &children,
    )
}

#[allow(clippy::too_many_arguments)]
async fn create_provider_transaction(
    api: &JsValue,
    route: &str,
    display_name: &str,
    base_url: &str,
    protocol: &str,
    key: &str,
    models: &JsValue,
    revision: &JsValue,
    committed: bool,
    set_committed: &Function,
) -> Result<(), String> {
    let key_ref = derive_key_ref(route);
    if !committed {
        let profile = Object::new();
        if !display_name.is_empty() {
            Reflect::set(
                &profile,
                &JsValue::from_str("displayName"),
                &JsValue::from_str(display_name),
            )
            .map_err(|error| rejection_text(&error))?;
        }
        if !key.is_empty() {
            Reflect::set(
                &profile,
                &JsValue::from_str("apiKeyEnv"),
                &JsValue::from_str(&key_ref),
            )
            .map_err(|error| rejection_text(&error))?;
        }
        let profile_models = Array::new();
        for model in models
            .dyn_ref::<Array>()
            .ok_or_else(|| "custom provider models must be an array".to_owned())?
            .iter()
        {
            profile_models.push(&clone_js_object(&model).map_err(js_text)?);
        }
        for (field, value) in [
            ("api", JsValue::from_str(protocol)),
            ("baseURL", JsValue::from_str(base_url)),
            ("models", profile_models.into()),
        ] {
            Reflect::set(&profile, &JsValue::from_str(field), &value)
                .map_err(|error| rejection_text(&error))?;
        }
        let path = Array::of2(&JsValue::from_str("providers"), &JsValue::from_str(route));
        let operation = object(&[
            ("op", JsValue::from_str("set")),
            ("path", path.into()),
            ("value", profile.into()),
        ])
        .map_err(js_text)?;
        let settings = required(api, "settings", "API client").map_err(js_text)?;
        let response = call_async(
            &settings,
            "mutate",
            &[object(&[
                ("ns", JsValue::from_str("llm-pi-ai")),
                ("ops", Array::of1(operation.as_ref()).into()),
                ("expectedRevision", revision.clone()),
            ])
            .map_err(js_text)?
            .into()],
        )
        .await
        .map_err(|error| rejection_text(&error))?;
        if let Err((_code, message)) = rpc_value(&response) {
            return Err(message);
        }
        set_committed
            .call1(&JsValue::UNDEFINED, &JsValue::TRUE)
            .map_err(js_text)?;
    }
    if !key.is_empty() {
        let credentials = required(api, "credentials", "API client").map_err(js_text)?;
        let response = call_async(
            &credentials,
            "set",
            &[object(&[
                ("ref", JsValue::from_str(&key_ref)),
                ("value", JsValue::from_str(key)),
            ])
            .map_err(js_text)?
            .into()],
        )
        .await
        .map_err(|error| rejection_text(&error))?;
        if let Err((_code, message)) = rpc_value(&response) {
            return Err(message);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Section state owns row/setup/add/delete card exclusivity.
pub(crate) fn render_models_section_surface(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let controller = required(props, "controller", "ModelsSection props")?;
    let use_snapshot = required_function(props, "useSnapshot", "ModelsSection props")?;
    let translate = required_function(props, "t", "ModelsSection props")?;
    let selector = Closure::wrap(
        Box::new(move |snapshot: JsValue| snapshot) as Box<dyn FnMut(JsValue) -> JsValue>
    );
    let state = use_snapshot.call1(&JsValue::UNDEFINED, &selector.into_js_value())?;
    let status = required(&state, "status", "Models state")?
        .as_string()
        .unwrap_or_default();
    let (editing, set_editing) = use_state(&modules.react, &JsValue::UNDEFINED)?;
    let (adding, set_adding) = use_state(&modules.react, &JsValue::FALSE)?;
    let (delete_target, set_delete_target) = use_state(&modules.react, &JsValue::UNDEFINED)?;
    let (deleting, set_deleting) = use_state(&modules.react, &JsValue::FALSE)?;
    let (delete_failure, set_delete_failure) = use_state(&modules.react, &JsValue::UNDEFINED)?;
    let (saved_target, set_saved_target) = use_state(&modules.react, &JsValue::UNDEFINED)?;
    let (declaring, set_declaring) = use_state(&modules.react, &JsValue::FALSE)?;
    let (dismissed_setup, set_dismissed_setup) =
        use_state(&modules.react, &Set::new(&JsValue::UNDEFINED).into())?;
    if status == "idle" {
        call_method(&controller, "load", &[])?;
    }
    if status == "error" {
        let retry_controller = controller;
        let retry = Closure::wrap(Box::new(move || {
            let _ = call_method(&retry_controller, "load", &[]);
        }) as Box<dyn FnMut()>);
        return tag(
            &modules.react,
            "div",
            Some(&class(&css("section"))?),
            &[
                tag(
                    &modules.react,
                    "p",
                    Some(&class(&css("error"))?),
                    &[JsValue::from_str(&format!(
                        "{}: {}",
                        translated(&translate, "loadFailed")?
                            .as_string()
                            .unwrap_or_default(),
                        required(&state, "error", "Models state")?
                            .as_string()
                            .unwrap_or_default()
                    ))],
                )?,
                tag(
                    &modules.react,
                    "button",
                    Some(&object(&[
                        ("type", JsValue::from_str("button")),
                        ("className", JsValue::from_str(&css("secondaryButton"))),
                        ("onClick", retry.into_js_value()),
                    ])?),
                    &[translated(&translate, "retry")?],
                )?,
            ],
        );
    }
    let dismissed_setup = dismissed_setup.dyn_into::<Set>()?;
    let rows = required(&state, "rows", "Models state")?.dyn_into::<Array>()?;
    let namespaces = required(&state, "namespaces", "Models state")?.dyn_into::<Map>()?;
    let writable = required(&state, "writable", "Models state")?
        .as_bool()
        .unwrap_or(false);
    let any_usable = rows.iter().any(|row| row_usable(&row));
    let adding_bool = adding.as_bool().unwrap_or(false);
    let mut row_nodes = Vec::new();
    for row in rows.iter() {
        if required(&row, "configured", "provider row")?.as_bool() != Some(true) {
            continue;
        }
        let entry = required(&row, "entry", "provider row")?;
        let provider = required(&entry, "provider", "provider entry")?
            .as_string()
            .unwrap_or_default();
        let display = required(&entry, "displayName", "provider entry")?
            .as_string()
            .unwrap_or_default();
        let namespace = namespaces.get(&required(&entry, "settingsNs", "provider entry")?);
        if namespace.is_undefined() {
            continue;
        }
        let target = editor_target(&row)?;
        let open = !adding_bool
            && optional(&editing, "provider")?
                .and_then(|value| value.as_string())
                .as_deref()
                == Some(&provider);
        let edit_setter = set_editing.clone();
        let edit_declaring = set_declaring.clone();
        let edit_adding = set_adding.clone();
        let edit_saved = set_saved_target.clone();
        let edit_target = target.clone();
        let edit = Closure::wrap(Box::new(move || {
            let next = if open {
                JsValue::UNDEFINED
            } else {
                edit_target.clone().into()
            };
            let _ = edit_declaring.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = edit_adding.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = edit_saved.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
            let _ = edit_setter.call1(&JsValue::UNDEFINED, &next);
        }) as Box<dyn FnMut()>);
        let credential = optional(&row, "credential")?;
        let configured = credential
            .as_ref()
            .and_then(|credential| optional(credential, "configured").ok().flatten())
            .and_then(|value| value.as_bool())
            == Some(true);
        let missing = !configured
            && optional(&row, "apiKeyEnv")?.is_some()
            && credential
                .as_ref()
                .and_then(|credential| optional(credential, "configured").ok().flatten())
                .and_then(|value| value.as_bool())
                == Some(false);
        let setup = !any_usable
            && required(&entry, "settingsPath", "provider entry")?
                .dyn_into::<Array>()?
                .length()
                == 0
            && !configured
            && !dismissed_setup.has(&JsValue::from_str(&provider));
        if setup {
            let close_dismissed = set_dismissed_setup.clone();
            let close_current = dismissed_setup.clone();
            let close_provider = provider.clone();
            let close_display = display.clone();
            let close_controller = controller.clone();
            let close_saved = set_saved_target.clone();
            let close = Closure::wrap(Box::new(move |changed: bool| {
                let next = Set::new(&JsValue::UNDEFINED);
                if let Ok(Some(values)) = js_sys::try_iter(&close_current.values()) {
                    for value in values.flatten() {
                        next.add(&value);
                    }
                }
                next.add(&JsValue::from_str(&close_provider));
                let _ = close_dismissed.call1(&JsValue::UNDEFINED, &next);
                if changed {
                    announce_saved_provider(
                        &close_controller,
                        &close_saved,
                        &close_provider,
                        &close_display,
                    );
                }
            }) as Box<dyn FnMut(bool)>);
            let editor = create_element(
                &modules.react,
                &configured_components()?.provider_editor,
                Some(&provider_editor_props(
                    props,
                    &translate,
                    &entry,
                    namespace,
                    !writable,
                    close.into_js_value(),
                )?),
                &[],
            )?;
            row_nodes.push(tag(
                &modules.react,
                "li",
                Some(&object(&[
                    ("key", JsValue::from_str(&provider)),
                    ("className", JsValue::from_str(&css("setupCard"))),
                ])?),
                &[editor],
            )?);
            continue;
        }
        let mut identity = vec![tag(
            &modules.react,
            "span",
            Some(&class(&css("rowName"))?),
            &[JsValue::from_str(&display)],
        )?];
        if optional(&entry, "declared")?.and_then(|value| value.as_bool()) == Some(true) {
            identity.push(tag(
                &modules.react,
                "span",
                Some(&class(&css("rowTag"))?),
                &[translated(&translate, "customTag")?],
            )?);
        }
        if configured || missing {
            let label = translated(
                &translate,
                if configured {
                    "credentialConfigured"
                } else {
                    "credentialMissing"
                },
            )?;
            identity.push(tag(
                &modules.react,
                "span",
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str(&format!(
                            "{} {}",
                            css("credentialDot"),
                            css(if configured {
                                "credentialDotConfigured"
                            } else {
                                "credentialDotMissing"
                            })
                        )),
                    ),
                    ("role", JsValue::from_str("img")),
                    ("aria-label", label.clone()),
                    ("title", label),
                ])?),
                &[],
            )?);
        }
        let edit_label = provider_copy(
            &translated(&translate, "editProvider")?
                .as_string()
                .unwrap_or_default(),
            &provider,
            &display,
        );
        let mut actions = vec![tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("className", JsValue::from_str(&css("secondaryButton"))),
                ("aria-label", JsValue::from_str(&edit_label)),
                ("onClick", edit.into_js_value()),
            ])?),
            &[translated(&translate, "edit")?],
        )?];
        if required(&row, "removable", "provider row")?.as_bool() == Some(true) {
            let target = target.clone();
            let delete_setter = set_delete_target.clone();
            let failure_setter = set_delete_failure.clone();
            let delete_saved = set_saved_target.clone();
            let remove = Closure::wrap(Box::new(move || {
                let _ = delete_saved.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
                let _ = failure_setter.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
                let _ = delete_setter.call1(&JsValue::UNDEFINED, &target);
            }) as Box<dyn FnMut()>);
            let label = provider_copy(
                &translated(&translate, "removeProvider")?
                    .as_string()
                    .unwrap_or_default(),
                &provider,
                &display,
            );
            actions.push(tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    ("className", JsValue::from_str(&css("dangerButton"))),
                    ("aria-label", JsValue::from_str(&label)),
                    ("disabled", JsValue::from_bool(!writable)),
                    ("onClick", remove.into_js_value()),
                ])?),
                &[translated(&translate, "remove")?],
            )?);
        }
        let head = tag(
            &modules.react,
            "div",
            Some(&class(&css("rowHead"))?),
            &[
                tag(
                    &modules.react,
                    "span",
                    Some(&class(&css("rowIdentity"))?),
                    &identity,
                )?,
                tag(
                    &modules.react,
                    "span",
                    Some(&class(&css("rowActions"))?),
                    &actions,
                )?,
            ],
        )?;
        let mut content = vec![head];
        if open {
            let close_setter = set_editing.clone();
            let close_adding = set_adding.clone();
            let close_declaring = set_declaring.clone();
            let close_provider = provider.clone();
            let close_display = display.clone();
            let close_controller = controller.clone();
            let close_saved = set_saved_target.clone();
            let close = Closure::wrap(Box::new(move |changed: bool| {
                let _ = close_setter.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
                let _ = close_adding.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
                let _ = close_declaring.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
                if changed {
                    announce_saved_provider(
                        &close_controller,
                        &close_saved,
                        &close_provider,
                        &close_display,
                    );
                }
            }) as Box<dyn FnMut(bool)>);
            content.push(create_element(
                &modules.react,
                &configured_components()?.provider_editor,
                Some(&provider_editor_props(
                    props,
                    &translate,
                    &entry,
                    namespace,
                    !writable,
                    close.into_js_value(),
                )?),
                &[],
            )?);
        }
        row_nodes.push(tag(
            &modules.react,
            "li",
            Some(&object(&[
                ("key", JsValue::from_str(&provider)),
                ("className", JsValue::from_str(&css("rowCard"))),
            ])?),
            &content,
        )?);
    }
    let declaring_bool = declaring.as_bool().unwrap_or(false);
    let addable = Array::new();
    for row in rows.iter() {
        let entry = required(&row, "entry", "provider row")?;
        if required(&row, "configured", "provider row")?.as_bool() != Some(true)
            && !required(&entry, "settingsNs", "provider entry")?
                .as_string()
                .unwrap_or_default()
                .is_empty()
        {
            addable.push(&row);
        }
    }
    let pi_namespace = namespaces.get(&JsValue::from_str("llm-pi-ai"));
    let protocols = if pi_namespace.is_undefined() {
        Array::new()
    } else {
        protocol_choices(modules, &pi_namespace)?
    };
    let mut children = vec![
        tag(
            &modules.react,
            "h2",
            Some(&class(&css("title"))?),
            &[translated(&translate, "title")?],
        )?,
        tag(
            &modules.react,
            "p",
            Some(&class(&css("intro"))?),
            &[translated(&translate, "intro")?],
        )?,
    ];
    if !writable && status == "ready" {
        children.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("notice"))?),
            &[translated(&translate, "readOnly")?],
        )?);
    }
    if !saved_target.is_undefined() {
        let saved_provider = required(&saved_target, "provider", "saved provider")?
            .as_string()
            .unwrap_or_default();
        let mut saved_display = required(&saved_target, "displayName", "saved provider")?
            .as_string()
            .unwrap_or_default();
        for row in rows.iter() {
            let entry = required(&row, "entry", "provider row")?;
            if required(&entry, "provider", "provider entry")?
                .as_string()
                .as_deref()
                == Some(saved_provider.as_str())
            {
                saved_display = required(&entry, "displayName", "provider entry")?
                    .as_string()
                    .unwrap_or(saved_display);
                break;
            }
        }
        let notice = provider_copy(
            &translated(&translate, "savedProvider")?
                .as_string()
                .unwrap_or_default(),
            &saved_provider,
            &saved_display,
        );
        children.push(tag(
            &modules.react,
            "p",
            Some(&object(&[
                ("className", JsValue::from_str(&css("savedNotice"))),
                ("role", JsValue::from_str("status")),
                ("aria-live", JsValue::from_str("polite")),
            ])?),
            &[JsValue::from_str(&notice)],
        )?);
    }
    children.push(tag(
        &modules.react,
        "ul",
        Some(&class(&css("rows"))?),
        &row_nodes,
    )?);
    let mut add_children = Vec::new();
    let add_namespace = if adding_bool && !editing.is_undefined() {
        namespaces.get(&required(&editing, "settingsNs", "add provider target")?)
    } else {
        JsValue::UNDEFINED
    };
    if adding_bool && !editing.is_undefined() && !add_namespace.is_undefined() {
        let selected = required(&editing, "provider", "add provider target")?
            .as_string()
            .unwrap_or_default();
        let namespace = add_namespace;
        let mut options = Vec::new();
        for row in addable.iter() {
            let option_entry = required(&row, "entry", "provider row")?;
            let value = required(&option_entry, "provider", "provider entry")?;
            options.push(tag(
                &modules.react,
                "option",
                Some(&object(&[("key", value.clone()), ("value", value)])?),
                &[required(&option_entry, "displayName", "provider entry")?],
            )?);
        }
        let selectable = addable.clone();
        let select_editing = set_editing.clone();
        let select = Closure::wrap(Box::new(move |event: JsValue| {
            let Ok(value) = event_value(&event) else {
                return;
            };
            for row in selectable.iter() {
                let matches = required(&row, "entry", "provider row")
                    .and_then(|entry| required(&entry, "provider", "provider entry"))
                    .ok()
                    .and_then(|provider| provider.as_string())
                    .as_deref()
                    == Some(value.as_str());
                if matches {
                    if let Ok(target) = editor_target(&row) {
                        let _ = select_editing.call1(&JsValue::UNDEFINED, target.as_ref());
                    }
                    break;
                }
            }
        }) as Box<dyn FnMut(JsValue)>);
        let close_adding = set_adding.clone();
        let close_editing = set_editing.clone();
        let close_declaring = set_declaring.clone();
        let close_controller = controller.clone();
        let close_saved = set_saved_target.clone();
        let close_provider = selected.clone();
        let close_display = required(&editing, "displayName", "add provider target")?
            .as_string()
            .unwrap_or_default();
        let close = Closure::wrap(Box::new(move |changed: bool| {
            let _ = close_adding.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = close_editing.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
            let _ = close_declaring.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            if changed {
                announce_saved_provider(
                    &close_controller,
                    &close_saved,
                    &close_provider,
                    &close_display,
                );
            }
        }) as Box<dyn FnMut(bool)>);
        add_children.push(tag(
            &modules.react,
            "div",
            Some(&class(&css("addCard"))?),
            &[
                tag(
                    &modules.react,
                    "div",
                    Some(&class(&css("field"))?),
                    &[
                        tag(
                            &modules.react,
                            "span",
                            Some(&class(&css("fieldLabel"))?),
                            &[translated(&translate, "provider")?],
                        )?,
                        tag(
                            &modules.react,
                            "select",
                            Some(&object(&[
                                (
                                    "className",
                                    JsValue::from_str(&format!(
                                        "{} {}",
                                        css("input"),
                                        css("selectInput")
                                    )),
                                ),
                                ("value", JsValue::from_str(&selected)),
                                ("aria-label", translated(&translate, "provider")?),
                                ("onChange", select.into_js_value()),
                            ])?),
                            &options,
                        )?,
                    ],
                )?,
                create_element(
                    &modules.react,
                    &configured_components()?.provider_editor,
                    Some(&object(&[
                        ("key", JsValue::from_str(&selected)),
                        ("provider", JsValue::from_str(&selected)),
                        (
                            "displayName",
                            required(&editing, "displayName", "add provider target")?,
                        ),
                        ("hideTitle", JsValue::TRUE),
                        ("namespace", namespace),
                        (
                            "settingsPath",
                            required(&editing, "settingsPath", "add provider target")?,
                        ),
                        (
                            "authentication",
                            optional(&editing, "authentication")?.unwrap_or(JsValue::UNDEFINED),
                        ),
                        ("api", required(props, "api", "ModelsSection props")?),
                        ("t", translate.clone().into()),
                        ("readOnly", JsValue::from_bool(!writable)),
                        ("onClose", close.into_js_value()),
                    ])?),
                    &[],
                )?,
            ],
        )?);
    } else if declaring_bool {
        let taken = Array::new();
        for row in rows.iter() {
            taken.push(&required(
                &required(&row, "entry", "provider row")?,
                "provider",
                "provider entry",
            )?);
        }
        let close_declaring_setter = set_declaring.clone();
        let close_declaring_controller = controller.clone();
        let close_declaring = Closure::wrap(Box::new(move |changed: bool| {
            let _ = close_declaring_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            if changed {
                let _ = call_method(&close_declaring_controller, "load", &[]);
            }
        }) as Box<dyn FnMut(bool)>);
        let card = create_element(
            &modules.react,
            &configured_components()?.custom_provider_card,
            Some(&object(&[
                ("taken", taken.into()),
                ("protocols", protocols.clone().into()),
                (
                    "revision",
                    optional(&pi_namespace, "revision")?.unwrap_or(JsValue::from_f64(0.0)),
                ),
                ("api", required(props, "api", "ModelsSection props")?),
                ("t", translate.clone().into()),
                ("readOnly", JsValue::from_bool(!writable)),
                ("onClose", close_declaring.into_js_value()),
            ])?),
            &[],
        )?;
        add_children.push(tag(
            &modules.react,
            "div",
            Some(&class(&css("addCard"))?),
            &[card],
        )?);
    } else {
        let show_add_setter = set_adding.clone();
        let show_add_editing = set_editing.clone();
        let show_add_declaring = set_declaring.clone();
        let show_add_saved = set_saved_target.clone();
        let first = addable.get(0);
        let show_add = Closure::wrap(Box::new(move || {
            let _ = show_add_declaring.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = show_add_saved.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
            let _ = show_add_setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
            if let Ok(target) = editor_target(&first) {
                let _ = show_add_editing.call1(&JsValue::UNDEFINED, target.as_ref());
            }
        }) as Box<dyn FnMut()>);
        let show_custom_declaring = set_declaring;
        let show_custom_adding = set_adding;
        let show_custom_editing = set_editing;
        let show_custom_saved = set_saved_target;
        let show_custom = Closure::wrap(Box::new(move || {
            let _ = show_custom_adding.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = show_custom_editing.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
            let _ = show_custom_saved.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
            let _ = show_custom_declaring.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
        }) as Box<dyn FnMut()>);
        add_children.push(tag(
            &modules.react,
            "div",
            Some(&class(&css("addActions"))?),
            &[
                add_provider_button(
                    modules,
                    &translate,
                    "add",
                    addable.length() == 0 || !writable,
                    show_add.into_js_value(),
                )?,
                add_provider_button(
                    modules,
                    &translate,
                    "customAdd",
                    protocols.length() == 0 || !writable,
                    show_custom.into_js_value(),
                )?,
            ],
        )?);
    }
    children.push(tag(
        &modules.react,
        "div",
        Some(&class(&css("addBlock"))?),
        &add_children,
    )?);
    children.push(delete_provider_modal(
        modules,
        props,
        &translate,
        &controller,
        &delete_target,
        &set_delete_target,
        deleting.as_bool().unwrap_or(false),
        &set_deleting,
        &delete_failure,
        &set_delete_failure,
    )?);
    tag(
        &modules.react,
        "div",
        Some(&class(&css("section"))?),
        &children,
    )
}

fn editor_target(row: &JsValue) -> Result<Object, JsValue> {
    let entry = required(row, "entry", "provider row")?;
    let provider = required(&entry, "provider", "provider entry")?
        .as_string()
        .unwrap_or_default();
    let target = object(&[
        ("provider", JsValue::from_str(&provider)),
        (
            "displayName",
            required(&entry, "displayName", "provider entry")?,
        ),
        (
            "settingsNs",
            required(&entry, "settingsNs", "provider entry")?,
        ),
        (
            "settingsPath",
            required(&entry, "settingsPath", "provider entry")?,
        ),
    ])?;
    if let Some(authentication) = optional(&entry, "authentication")? {
        Reflect::set(
            &target,
            &JsValue::from_str("authentication"),
            &authentication,
        )?;
    }
    let managed = derive_key_ref(&provider);
    let credential = optional(row, "credential")?;
    let manages_credential = optional(row, "apiKeyEnv")?
        .and_then(|value| value.as_string())
        .as_deref()
        == Some(managed.as_str())
        && credential
            .as_ref()
            .and_then(|credential| optional(credential, "configured").ok().flatten())
            .and_then(|value| value.as_bool())
            == Some(true)
        && credential
            .as_ref()
            .and_then(|credential| optional(credential, "writable").ok().flatten())
            .and_then(|value| value.as_bool())
            == Some(true);
    if manages_credential {
        Reflect::set(
            &target,
            &JsValue::from_str("credentialRef"),
            &JsValue::from_str(&managed),
        )?;
    }
    if optional(&entry, "declared")?.and_then(|value| value.as_bool()) == Some(true) {
        Reflect::set(&target, &JsValue::from_str("declared"), &JsValue::TRUE)?;
    }
    Ok(target)
}

fn provider_editor_props(
    props: &JsValue,
    translate: &Function,
    entry: &JsValue,
    namespace: JsValue,
    read_only: bool,
    on_close: JsValue,
) -> Result<Object, JsValue> {
    object(&[
        ("provider", required(entry, "provider", "provider entry")?),
        (
            "displayName",
            required(entry, "displayName", "provider entry")?,
        ),
        ("namespace", namespace),
        (
            "settingsPath",
            required(entry, "settingsPath", "provider entry")?,
        ),
        (
            "authentication",
            optional(entry, "authentication")?.unwrap_or(JsValue::UNDEFINED),
        ),
        (
            "declared",
            optional(entry, "declared")?.unwrap_or(JsValue::UNDEFINED),
        ),
        ("api", required(props, "api", "ModelsSection props")?),
        ("t", translate.clone().into()),
        ("readOnly", JsValue::from_bool(read_only)),
        ("onClose", on_close),
    ])
}

fn announce_saved_provider(
    controller: &JsValue,
    setter: &Function,
    provider: &str,
    display_name: &str,
) {
    let Ok(target) = object(&[
        ("provider", JsValue::from_str(provider)),
        ("displayName", JsValue::from_str(display_name)),
    ]) else {
        return;
    };
    let Ok(returned) = call_method(controller, "load", &[]) else {
        return;
    };
    let setter = setter.clone();
    spawn_local(async move {
        if JsFuture::from(Promise::resolve(&returned)).await.is_ok() {
            let _ = setter.call1(&JsValue::UNDEFINED, &target);
        }
    });
}

fn row_usable(row: &JsValue) -> bool {
    let Ok(entry) = required(row, "entry", "provider row") else {
        return false;
    };
    if required(&entry, "active", "provider entry")
        .ok()
        .and_then(|value| value.as_bool())
        != Some(true)
    {
        return false;
    }
    if optional(row, "apiKeyEnv").ok().flatten().is_none() {
        return true;
    }
    optional(row, "credential")
        .ok()
        .flatten()
        .and_then(|credential| optional(&credential, "configured").ok().flatten())
        .and_then(|value| value.as_bool())
        == Some(true)
}

fn add_provider_button(
    modules: &BrowserModules,
    translate: &Function,
    key: &str,
    disabled: bool,
    on_click: JsValue,
) -> Result<JsValue, JsValue> {
    tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&css("addButton"))),
            ("disabled", JsValue::from_bool(disabled)),
            ("onClick", on_click),
        ])?),
        &[
            create_element(
                &modules.react,
                &modules.primitive("IconPlusOutline16")?,
                Some(&object(&[("size", JsValue::from_f64(14.0))])?),
                &[],
            )?,
            translated(translate, key)?,
        ],
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn delete_provider_modal(
    modules: &BrowserModules,
    props: &JsValue,
    translate: &Function,
    controller: &JsValue,
    target: &JsValue,
    set_target: &Function,
    deleting: bool,
    set_deleting: &Function,
    failure: &JsValue,
    set_failure: &Function,
) -> Result<JsValue, JsValue> {
    let open = !target.is_undefined();
    let provider = optional(target, "provider")?
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    let display = optional(target, "displayName")?
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    let close_target = set_target.clone();
    let close_failure = set_failure.clone();
    let close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if deleting {
            return Ok(());
        }
        close_target.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED)?;
        close_failure.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .dyn_into::<Function>()?;
    let confirm_api = required(props, "api", "ModelsSection props")?;
    let confirm_controller = controller.clone();
    let confirm_target = target.clone();
    let confirm_set_target = set_target.clone();
    let confirm_set_deleting = set_deleting.clone();
    let confirm_set_failure = set_failure.clone();
    let confirm = Closure::wrap(Box::new(move || {
        if deleting || confirm_target.is_undefined() {
            return;
        }
        let api = confirm_api.clone();
        let controller = confirm_controller.clone();
        let target = confirm_target.clone();
        let set_target = confirm_set_target.clone();
        let set_deleting = confirm_set_deleting.clone();
        let set_failure = confirm_set_failure.clone();
        let _ = set_deleting.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
        let _ = set_failure.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
        spawn_local(async move {
            match remove_provider_profile(&api, &controller, &target).await {
                Ok(()) => {
                    let _ = set_target.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
                }
                Err(error) => {
                    let _ = set_failure.call1(&JsValue::UNDEFINED, &JsValue::from_str(&error));
                }
            }
            let _ = set_deleting.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
        });
    }) as Box<dyn FnMut()>);
    let title = if open {
        provider_copy(
            &translated(translate, "deleteTitle")?
                .as_string()
                .unwrap_or_default(),
            &provider,
            &display,
        )
    } else {
        String::new()
    };
    let managed = optional(target, "credentialRef")?.is_some();
    let description = if open {
        provider_copy(
            &translated(
                translate,
                if managed {
                    "deleteDescriptionWithCredential"
                } else {
                    "deleteDescription"
                },
            )?
            .as_string()
            .unwrap_or_default(),
            &provider,
            &display,
        )
    } else {
        String::new()
    };
    let confirm_copy = if open {
        provider_copy(
            &translated(
                translate,
                if deleting {
                    "deleting"
                } else {
                    "deleteConfirm"
                },
            )?
            .as_string()
            .unwrap_or_default(),
            &provider,
            &display,
        )
    } else {
        String::new()
    };
    let footer = tag(
        &modules.react,
        "div",
        Some(&class(&css("editorActions"))?),
        &[
            create_element(
                &modules.react,
                &modules.primitive("Button")?,
                Some(&object(&[
                    ("variant", JsValue::from_str("outline")),
                    ("autoFocus", JsValue::TRUE),
                    ("disabled", JsValue::from_bool(deleting)),
                    ("onClick", close.clone().into()),
                ])?),
                &[translated(translate, "cancel")?],
            )?,
            create_element(
                &modules.react,
                &modules.primitive("Button")?,
                Some(&object(&[
                    ("variant", JsValue::from_str("outline")),
                    ("className", JsValue::from_str(&css("deleteConfirm"))),
                    ("disabled", JsValue::from_bool(deleting)),
                    ("onClick", confirm.into_js_value()),
                ])?),
                &[JsValue::from_str(&confirm_copy)],
            )?,
        ],
    )?;
    let mut body = Vec::new();
    if !failure.is_undefined() {
        body.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("error"))?),
            std::slice::from_ref(failure),
        )?);
    }
    create_element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("onClose", close.into()),
            ("title", JsValue::from_str(&title)),
            ("closeLabel", translated(translate, "close")?),
            ("description", JsValue::from_str(&description)),
            ("className", JsValue::from_str(&css("deleteDialog"))),
            ("footer", footer),
        ])?),
        &body,
    )
}

async fn remove_provider_profile(
    api: &JsValue,
    controller: &JsValue,
    target: &JsValue,
) -> Result<(), String> {
    if let Some(reference) = optional(target, "credentialRef")
        .map_err(js_text)?
        .and_then(|value| value.as_string())
    {
        let credentials = required(api, "credentials", "API client").map_err(js_text)?;
        let response = call_async(
            &credentials,
            "unset",
            &[object(&[("ref", JsValue::from_str(&reference))])
                .map_err(js_text)?
                .into()],
        )
        .await
        .map_err(|error| rejection_text(&error))?;
        if let Err((_code, message)) = rpc_value(&response) {
            return Err(message);
        }
    }
    let settings = required(api, "settings", "API client").map_err(js_text)?;
    let operation = object(&[
        ("op", JsValue::from_str("unset")),
        (
            "path",
            required(target, "settingsPath", "provider deletion").map_err(js_text)?,
        ),
    ])
    .map_err(js_text)?;
    let response = call_async(
        &settings,
        "mutate",
        &[object(&[
            (
                "ns",
                required(target, "settingsNs", "provider deletion").map_err(js_text)?,
            ),
            ("ops", Array::of1(operation.as_ref()).into()),
        ])
        .map_err(js_text)?
        .into()],
    )
    .await
    .map_err(|error| rejection_text(&error))?;
    if let Err((_code, message)) = rpc_value(&response) {
        return Err(message);
    }
    let returned = call_method(controller, "load", &[]).map_err(js_text)?;
    JsFuture::from(Promise::resolve(&returned))
        .await
        .map_err(|error| rejection_text(&error))?;
    Ok(())
}

fn install_credential_effect(
    modules: &BrowserModules,
    api: &JsValue,
    reference: &str,
    skip: bool,
    setter: Function,
) -> Result<(), JsValue> {
    let credentials_dependency = required(api, "credentials", "API client")?;
    let reference_dependency = reference.to_owned();
    let api = api.clone();
    let reference = reference.to_owned();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        setter.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED)?;
        let alive = Rc::new(Cell::new(true));
        if skip {
            let cleanup_alive = alive;
            return Ok(Closure::wrap(
                Box::new(move || cleanup_alive.set(false)) as Box<dyn FnMut()>
            )
            .into_js_value());
        }
        let credentials = required(&api, "credentials", "API client")?;
        let payload = object(&[("refs", Array::of1(&JsValue::from_str(&reference)).into())])?;
        let returned = call_method(&credentials, "describe", &[payload.into()])?;
        let settle_setter = setter.clone();
        let settle_reference = reference.clone();
        let fulfilled_alive = alive.clone();
        let fulfilled = Closure::wrap(Box::new(move |response: JsValue| {
            if fulfilled_alive.get()
                && let Ok(value) = rpc_value(&response)
            {
                let credentials = required(&value, "credentials", "credentials value")
                    .unwrap_or(JsValue::UNDEFINED);
                let credential = Reflect::get(&credentials, &JsValue::from_str(&settle_reference))
                    .unwrap_or(JsValue::UNDEFINED);
                let _ = settle_setter.call1(&JsValue::UNDEFINED, &credential);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let rejected =
            Closure::wrap(Box::new(move |_error: JsValue| {}) as Box<dyn FnMut(JsValue)>);
        let promise: JsValue = Promise::resolve(&returned).into();
        call_method(
            &promise,
            "then",
            &[fulfilled.into_js_value(), rejected.into_js_value()],
        )?;
        let cleanup_alive = alive;
        Ok(
            Closure::wrap(Box::new(move || cleanup_alive.set(false)) as Box<dyn FnMut()>)
                .into_js_value(),
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(&modules.react, "useEffect", "React")?.call2(
        &modules.react,
        &effect.into_js_value(),
        &Array::of3(
            &credentials_dependency,
            &JsValue::from_str(&reference_dependency),
            &JsValue::from_bool(skip),
        ),
    )?;
    Ok(())
}

fn schema_call(
    modules: &BrowserModules,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    required_function(&modules.schema_form, name, "schema form")?.apply(
        &JsValue::UNDEFINED,
        &arguments.iter().cloned().collect::<Array>(),
    )
}

fn profile_string(
    modules: &BrowserModules,
    profile: &JsValue,
    field: &str,
) -> Result<Option<String>, JsValue> {
    if profile.is_null() || profile.is_undefined() {
        return Ok(None);
    }
    let value = schema_call(
        modules,
        "getPath",
        &[
            profile.clone(),
            Array::of1(&JsValue::from_str(field)).into(),
        ],
    )?;
    Ok(value
        .as_string()
        .filter(|value| !trim_ecmascript_whitespace(value).is_empty()))
}

fn append_path(path: &JsValue, field: &str) -> Result<JsValue, JsValue> {
    let path = path.clone().dyn_into::<Array>()?;
    let next = Array::from(&path);
    next.push(&JsValue::from_str(field));
    Ok(next.into())
}

fn model_drafts_array(value: &JsValue) -> JsValue {
    let drafts = Array::new();
    if let Some(models) = value.dyn_ref::<Array>() {
        for model in models.iter() {
            if model.is_object() && !Array::is_array(&model) {
                drafts.push(&model);
            } else {
                drafts.push(&Object::new());
            }
        }
    }
    drafts.into()
}

fn clone_js_object(value: &JsValue) -> Result<JsValue, JsValue> {
    let source = value
        .clone()
        .dyn_into::<Object>()
        .map_err(|_| js_sys::TypeError::new("model row must be an object"))?;
    Ok(Object::assign(&Object::new(), &source).into())
}

fn clone_value(value: &JsValue) -> Result<JsValue, JsValue> {
    let structured = required_function(&js_sys::global(), "structuredClone", "global")?;
    structured.call1(&JsValue::UNDEFINED, value)
}

fn js_models_validation(value: &JsValue) -> Result<Option<(usize, &'static str)>, JsValue> {
    if value.is_undefined() {
        return Ok(None);
    }
    let encoded = JSON::stringify(value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("models must be JSON-compatible"))?;
    let parsed: Value = serde_json::from_str(&encoded)
        .map_err(|error| js_sys::TypeError::new(&error.to_string()))?;
    Ok(validate_models(Some(&parsed)).map(|failure| (failure.index, failure.key.as_str())))
}

fn settings_ops(path: &JsValue, before: &JsValue, after: &JsValue) -> Result<Array, JsValue> {
    let base = path
        .clone()
        .dyn_into::<Array>()?
        .iter()
        .map(|value| {
            value
                .as_string()
                .ok_or_else(|| js_sys::TypeError::new("settings path must contain strings"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let before = js_json_value(before)?;
    let after = js_json_value(after)?;
    let after = after
        .as_object()
        .ok_or_else(|| js_sys::TypeError::new("settings draft must be an object"))?;
    let operations = path_ops(&base, Some(&before), after);
    let result = Array::new();
    for operation in operations {
        result.push(&match operation {
            SettingsPathOp::Set { path, value } => object(&[
                ("op", JsValue::from_str("set")),
                ("path", string_array(&path).into()),
                (
                    "value",
                    JSON::parse(&serde_json::to_string(&value).unwrap_or_default())?,
                ),
            ])?
            .into(),
            SettingsPathOp::Unset { path } => object(&[
                ("op", JsValue::from_str("unset")),
                ("path", string_array(&path).into()),
            ])?
            .into(),
        });
    }
    Ok(result)
}

fn js_json_value(value: &JsValue) -> Result<Value, JsValue> {
    if value.is_undefined() {
        return Ok(Value::Null);
    }
    let encoded = JSON::stringify(value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("value must be JSON-compatible"))?;
    serde_json::from_str(&encoded)
        .map_err(|error| js_sys::TypeError::new(&error.to_string()).into())
}

fn string_array(values: &[String]) -> Array {
    let output = Array::new();
    for value in values {
        output.push(&JsValue::from_str(value));
    }
    output
}

fn rpc_value(response: &JsValue) -> Result<JsValue, (String, String)> {
    let result = required(response, "result", "RPC response")
        .map_err(|error| (String::new(), rejection_text(&error)))?;
    if required(&result, "ok", "RPC result")
        .map_err(|error| (String::new(), rejection_text(&error)))?
        .as_bool()
        == Some(true)
    {
        required(&result, "value", "RPC result")
            .map_err(|error| (String::new(), rejection_text(&error)))
    } else {
        let error = required(&result, "error", "RPC result")
            .map_err(|error| (String::new(), rejection_text(&error)))?;
        Err((
            optional(&error, "code")
                .ok()
                .flatten()
                .and_then(|value| value.as_string())
                .unwrap_or_default(),
            required(&error, "message", "RPC error")
                .map_err(|error| (String::new(), rejection_text(&error)))?
                .as_string()
                .unwrap_or_default(),
        ))
    }
}

fn custom_text_field(
    modules: &BrowserModules,
    translate: &Function,
    key: &str,
    value: &JsValue,
    placeholder: &str,
    disabled: bool,
    setter: Function,
) -> Result<Vec<JsValue>, JsValue> {
    Ok(vec![tag(
        &modules.react,
        "div",
        Some(&class(&css("field"))?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&class(&css("fieldLabel"))?),
                &[translated(translate, key)?],
            )?,
            tag(
                &modules.react,
                "input",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("input"))),
                    ("type", JsValue::from_str("text")),
                    ("value", value.clone()),
                    (
                        "placeholder",
                        if placeholder.is_empty() {
                            translated(translate, key)?
                        } else {
                            JsValue::from_str(placeholder)
                        },
                    ),
                    ("aria-label", translated(translate, key)?),
                    ("disabled", JsValue::from_bool(disabled)),
                    ("onChange", state_text_change(setter)),
                ])?),
                &[],
            )?,
        ],
    )?])
}

#[allow(clippy::too_many_arguments)]
fn text_draft_field(
    modules: &BrowserModules,
    translate: &Function,
    key: &str,
    value: &str,
    placeholder: &str,
    disabled: bool,
    draft: &JsValue,
    setter: &Function,
    field: &'static str,
) -> Result<JsValue, JsValue> {
    let current = draft.clone();
    let update = setter.clone();
    let change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let value = event_value(&event)?;
        let next = schema_call(
            &configured_modules()?,
            if trim_ecmascript_whitespace(&value).is_empty() {
                "deletePath"
            } else {
                "setPath"
            },
            &if trim_ecmascript_whitespace(&value).is_empty() {
                vec![
                    current.clone(),
                    Array::of1(&JsValue::from_str(field)).into(),
                ]
            } else {
                vec![
                    current.clone(),
                    Array::of1(&JsValue::from_str(field)).into(),
                    JsValue::from_str(&value),
                ]
            },
        )?;
        update.call1(&JsValue::UNDEFINED, &next)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    tag(
        &modules.react,
        "div",
        Some(&class(&css("field"))?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&class(&css("fieldLabel"))?),
                &[translated(translate, key)?],
            )?,
            tag(
                &modules.react,
                "input",
                Some(&object(&[
                    ("className", JsValue::from_str(&css("input"))),
                    ("type", JsValue::from_str("text")),
                    ("value", JsValue::from_str(value)),
                    ("placeholder", JsValue::from_str(placeholder)),
                    ("aria-label", translated(translate, key)?),
                    ("disabled", JsValue::from_bool(disabled)),
                    ("onChange", change.into_js_value()),
                ])?),
                &[],
            )?,
        ],
    )
}

#[allow(clippy::too_many_arguments)]
fn draft_select_field(
    modules: &BrowserModules,
    translate: &Function,
    key: &str,
    value: &str,
    choices: &Array,
    disabled: bool,
    draft: &JsValue,
    setter: &Function,
    field: &'static str,
) -> Result<JsValue, JsValue> {
    let current = draft.clone();
    let update = setter.clone();
    let change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let value = event_value(&event)?;
        let next = schema_call(
            &configured_modules()?,
            if value.is_empty() {
                "deletePath"
            } else {
                "setPath"
            },
            &if value.is_empty() {
                vec![
                    current.clone(),
                    Array::of1(&JsValue::from_str(field)).into(),
                ]
            } else {
                vec![
                    current.clone(),
                    Array::of1(&JsValue::from_str(field)).into(),
                    JsValue::from_str(&value),
                ]
            },
        )?;
        update.call1(&JsValue::UNDEFINED, &next)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let mut options = Vec::new();
    if value.is_empty() {
        options.push(tag(
            &modules.react,
            "option",
            Some(&object(&[("value", JsValue::from_str(""))])?),
            &[translated(translate, "customApiUnset")?],
        )?);
    }
    for choice in choices.iter() {
        options.push(tag(
            &modules.react,
            "option",
            Some(&object(&[
                ("key", choice.clone()),
                ("value", choice.clone()),
            ])?),
            std::slice::from_ref(&choice),
        )?);
    }
    tag(
        &modules.react,
        "div",
        Some(&class(&css("field"))?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&class(&css("fieldLabel"))?),
                &[translated(translate, key)?],
            )?,
            tag(
                &modules.react,
                "select",
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str(&format!("{} {}", css("input"), css("selectInput"))),
                    ),
                    ("value", JsValue::from_str(value)),
                    ("aria-label", translated(translate, key)?),
                    ("disabled", JsValue::from_bool(disabled)),
                    ("onChange", change.into_js_value()),
                ])?),
                &options,
            )?,
        ],
    )
}

fn protocol_choices(modules: &BrowserModules, namespace: &JsValue) -> Result<Array, JsValue> {
    let root = schema_call(
        modules,
        "rehydrateSchema",
        &[required(namespace, "schema", "settings namespace")?],
    )?;
    let path = Array::new();
    for segment in ["providers", "\0probe", "api"] {
        path.push(&JsValue::from_str(segment));
    }
    let node = schema_call(modules, "nodeAtPath", &[root, path.into()])?;
    let output = Array::new();
    if optional(&node, "type")?
        .and_then(|value| value.as_string())
        .as_deref()
        != Some("union")
    {
        return Ok(output);
    }
    if let Some(list) = optional(&node, "list")?
        && let Ok(list) = list.dyn_into::<Array>()
    {
        for entry in list.iter() {
            if let Some(value) = optional(&entry, "value")?.and_then(|value| value.as_string()) {
                output.push(&JsValue::from_str(&value));
            }
        }
    }
    Ok(output)
}

fn select_field(
    modules: &BrowserModules,
    translate: &Function,
    key: &str,
    value: &JsValue,
    choices: &Array,
    disabled: bool,
    setter: Function,
) -> Result<JsValue, JsValue> {
    let mut options = Vec::new();
    for choice in choices.iter() {
        options.push(tag(
            &modules.react,
            "option",
            Some(&object(&[
                ("key", choice.clone()),
                ("value", choice.clone()),
            ])?),
            std::slice::from_ref(&choice),
        )?);
    }
    tag(
        &modules.react,
        "div",
        Some(&class(&css("field"))?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&class(&css("fieldLabel"))?),
                &[translated(translate, key)?],
            )?,
            tag(
                &modules.react,
                "select",
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str(&format!("{} {}", css("input"), css("selectInput"))),
                    ),
                    ("value", value.clone()),
                    ("aria-label", translated(translate, key)?),
                    ("disabled", JsValue::from_bool(disabled)),
                    ("onChange", state_text_change(setter)),
                ])?),
                &options,
            )?,
        ],
    )
}

fn state_text_change(setter: Function) -> JsValue {
    Closure::wrap(Box::new(move |event: JsValue| {
        if let Ok(value) = event_value(&event) {
            let _ = setter.call1(&JsValue::UNDEFINED, &JsValue::from_str(&value));
        }
    }) as Box<dyn FnMut(JsValue)>)
    .into_js_value()
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn event_value(event: &JsValue) -> Result<String, JsValue> {
    required(
        &required(event, "target", "input event")?,
        "value",
        "input target",
    )?
    .as_string()
    .ok_or_else(|| js_sys::TypeError::new("input value must be a string").into())
}

#[allow(clippy::needless_pass_by_value)] // `map_err` supplies owned JavaScript errors.
fn js_text(error: JsValue) -> String {
    rejection_text(&error)
}
