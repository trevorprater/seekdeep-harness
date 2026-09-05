//! Compiled Models settings, provider editor, and onboarding components.

use std::cell::RefCell;

use js_sys::{Array, Function, Map, Object, Promise, Reflect, Set, try_iter};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
    api_key::trim_ecmascript_whitespace,
    browser::{
        call_method, class, create_element, css, inject_prefixed_style, object, optional,
        rejection_text, required, required_function, tag, translated,
    },
    format_capacity, parse_capacity,
};

const MODELS_CSS: &str =
    include_str!("../../../packages/client/ui-settings-models/src/client/ModelsSection.module.css");
const ONBOARDING_CSS: &str = include_str!(
    "../../../packages/client/ui-settings-models/src/client/OnboardingModal.module.css"
);
const DEEPSEEK_ONBOARDING_CSS: &str = include_str!(
    "../../../packages/client/ui-settings-models/src/client/DeepSeekOnboardingDialog.module.css"
);
const WELCOME_CSS: &str =
    include_str!("../../../packages/client/ui-settings-models/src/client/WelcomeNotice.module.css");

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
    static COMPONENTS: RefCell<Option<Components>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct BrowserModules {
    pub(crate) react: JsValue,
    primitives: JsValue,
    pub(crate) schema_form: JsValue,
    pub(crate) bind_snapshot_selector: Function,
    ignore_implicit_dismiss: Function,
}

impl BrowserModules {
    pub(crate) fn primitive(&self, name: &str) -> Result<JsValue, JsValue> {
        required(&self.primitives, name, "UI primitives")
    }
}

#[derive(Clone)]
pub(crate) struct Components {
    pub(crate) models_section: JsValue,
    pub(crate) provider_editor: JsValue,
    pub(crate) custom_provider_card: JsValue,
    pub(crate) deepseek_models_editor: JsValue,
    pub(crate) model_list_editor: JsValue,
    pub(crate) editor_footer: JsValue,
    pub(crate) onboarding_modal: JsValue,
    pub(crate) deepseek_onboarding: JsValue,
    pub(crate) welcome_notice: JsValue,
}

struct FetchCatalogState {
    busy: JsValue,
    set_busy: Function,
    failure: JsValue,
    set_failure: Function,
    candidates: JsValue,
    set_candidates: Function,
    picked: JsValue,
    set_picked: Function,
}

/// Configures page-owned React, primitives, schema-form helpers, and selector binding.
///
/// # Errors
///
/// Returns before mutation when a required module value or stylesheet is unavailable.
#[wasm_bindgen(js_name = configureClientUiSettingsModels)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_settings_models(
    react: JsValue,
    primitives: JsValue,
    schema_form: JsValue,
    bind_snapshot_selector: Function,
) -> Result<(), JsValue> {
    for method in [
        "createElement",
        "useCallback",
        "useEffect",
        "useMemo",
        "useRef",
        "useState",
    ] {
        required_function(&react, method, "React")?;
    }
    required(&react, "Fragment", "React")?;
    for primitive in [
        "Button",
        "IconChevronDownOutline14",
        "IconChevronRightOutline14",
        "IconPlusOutline16",
        "IconTrashOutline16",
        "Modal",
    ] {
        required(&primitives, primitive, "UI primitives")?;
    }
    for helper in [
        "deletePath",
        "getPath",
        "hasPath",
        "nodeAtPath",
        "rehydrateSchema",
        "setPath",
        "validateDraft",
    ] {
        required_function(&schema_form, helper, "schema form")?;
    }
    inject_prefixed_style("ModelsSection", MODELS_CSS)?;
    inject_prefixed_style("OnboardingModal", ONBOARDING_CSS)?;
    inject_prefixed_style("DeepSeekOnboardingDialog", DEEPSEEK_ONBOARDING_CSS)?;
    inject_prefixed_style("WelcomeNotice", WELCOME_CSS)?;
    let modules = BrowserModules {
        react,
        primitives,
        schema_form,
        bind_snapshot_selector,
        ignore_implicit_dismiss: Function::new_no_args(""),
    };
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules.clone()));
    let onboarding_modal = component(&modules, render_onboarding_modal);
    let editor_footer = component(&modules, render_editor_footer);
    let deepseek_models_editor = component(&modules, render_deepseek_models_editor);
    let model_list_editor = component(&modules, render_model_list_editor);
    let provider_editor = component(&modules, render_provider_editor);
    let custom_provider_card = component(&modules, render_custom_provider_card);
    let models_section = component(&modules, render_models_section);
    let deepseek_onboarding = component(&modules, render_deepseek_onboarding);
    let welcome_notice = component(&modules, render_welcome_notice);
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(Components {
            models_section,
            provider_editor,
            custom_provider_card,
            deepseek_models_editor,
            model_list_editor,
            editor_footer,
            onboarding_modal,
            deepseek_onboarding,
            welcome_notice,
        });
    });
    Ok(())
}

type Renderer = fn(&BrowserModules, &JsValue) -> Result<JsValue, JsValue>;

fn component(modules: &BrowserModules, renderer: Renderer) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(Box::new(move |props: JsValue| renderer(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

pub(crate) fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-settings-models modules were not configured").into()
        })
    })
}

pub(crate) fn configured_components() -> Result<Components, JsValue> {
    COMPONENTS.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-settings-models components were not configured").into()
        })
    })
}

macro_rules! component_getter {
    ($rust:ident, $js:literal, $field:ident, $label:literal) => {
        #[doc = concat!("Returns the compiled `", $label, "` component.")]
        ///
        /// # Errors
        ///
        /// Returns before browser configuration.
        #[wasm_bindgen(js_name = $js)]
        pub fn $rust() -> Result<JsValue, JsValue> {
            Ok(configured_components()?.$field)
        }
    };
}

component_getter!(
    models_section_component,
    "modelsSectionComponent",
    models_section,
    "ModelsSection"
);
component_getter!(
    provider_editor_component,
    "providerEditorComponent",
    provider_editor,
    "ProviderEditor"
);
component_getter!(
    custom_provider_card_component,
    "customProviderCardComponent",
    custom_provider_card,
    "CustomProviderCard"
);
component_getter!(
    deepseek_models_editor_component,
    "deepSeekModelsEditorComponent",
    deepseek_models_editor,
    "DeepSeekModelsEditor"
);
component_getter!(
    model_list_editor_component,
    "modelListEditorComponent",
    model_list_editor,
    "ModelListEditor"
);
component_getter!(
    editor_footer_component,
    "editorFooterComponent",
    editor_footer,
    "EditorFooter"
);
component_getter!(
    onboarding_modal_component,
    "onboardingModalComponent",
    onboarding_modal,
    "OnboardingModal"
);
component_getter!(
    deepseek_onboarding_component,
    "deepSeekOnboardingComponent",
    deepseek_onboarding,
    "DeepSeekOnboardingDialog"
);
component_getter!(
    welcome_notice_component,
    "welcomeNoticeComponent",
    welcome_notice,
    "WelcomeNotice"
);

fn render_editor_footer(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "EditorFooter props")?;
    let busy = required(props, "busy", "EditorFooter props")?
        .as_bool()
        .unwrap_or(false);
    let disabled = required(props, "submitDisabled", "EditorFooter props")?
        .as_bool()
        .unwrap_or(false);
    let cancel_key = optional(props, "cancelLabel")?
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| "cancel".to_owned());
    let submit_key = if busy {
        required(props, "submitBusyLabel", "EditorFooter props")?
    } else {
        required(props, "submitLabel", "EditorFooter props")?
    };
    tag(
        &modules.react,
        "div",
        Some(&class(&css("editorActions"))?),
        &[
            tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    ("className", JsValue::from_str(&css("secondaryButton"))),
                    ("disabled", JsValue::from_bool(busy)),
                    (
                        "onClick",
                        required(props, "onCancel", "EditorFooter props")?,
                    ),
                ])?),
                &[translated(&translate, &cancel_key)?],
            )?,
            tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    ("className", JsValue::from_str(&css("primaryButton"))),
                    ("disabled", JsValue::from_bool(disabled)),
                    (
                        "onClick",
                        required(props, "onSubmit", "EditorFooter props")?,
                    ),
                ])?),
                &[translate.call1(&JsValue::UNDEFINED, &submit_key)?],
            )?,
        ],
    )
}

fn render_onboarding_modal(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let title = required(props, "title", "OnboardingModal props")?;
    let focus_title = optional(props, "focusTitle")?
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let title_ref = use_ref(&modules.react, &JsValue::NULL)?;
    install_inert_effect(&modules.react)?;
    install_focus_effect(&modules.react, &title_ref, focus_title)?;
    let title_node = tag(
        &modules.react,
        "h2",
        Some(&object(&[
            ("ref", title_ref),
            ("className", JsValue::from_str(&css("title"))),
            (
                "tabIndex",
                if focus_title {
                    JsValue::from_f64(-1.0)
                } else {
                    JsValue::UNDEFINED
                },
            ),
        ])?),
        std::slice::from_ref(&title),
    )?;
    let content = tag(
        &modules.react,
        "div",
        Some(&class(&css("content"))?),
        &[
            title_node,
            tag(
                &modules.react,
                "div",
                Some(&class(&css("body"))?),
                &optional(props, "children")?.into_iter().collect::<Vec<_>>(),
            )?,
        ],
    )?;
    create_element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::TRUE),
            ("title", title),
            ("onClose", modules.ignore_implicit_dismiss.clone().into()),
            ("headless", JsValue::TRUE),
            ("className", JsValue::from_str(&css("dialog"))),
        ])?),
        &[content],
    )
}

#[allow(clippy::too_many_lines)] // Two effects and one guarded transaction form the complete notice.
fn render_welcome_notice(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let complete = required_function(props, "complete", "WelcomeNotice props")?;
    let controller = required(props, "controller", "WelcomeNotice props")?;
    let use_welcome = required_function(props, "useWelcome", "WelcomeNotice props")?;
    let translate = required_function(props, "t", "WelcomeNotice props")?;
    let selector = Closure::wrap(
        Box::new(move |snapshot: JsValue| snapshot) as Box<dyn FnMut(JsValue) -> JsValue>
    );
    let state = use_welcome.call1(&JsValue::UNDEFINED, &selector.into_js_value())?;
    let finished = use_ref(&modules.react, &JsValue::FALSE)?;
    let finish_ref = finished.clone();
    let finish_dependency = complete.clone();
    let finish_complete = complete;
    let finish_callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if Reflect::get(&finish_ref, &JsValue::from_str("current"))?
            .as_bool()
            .unwrap_or(false)
        {
            return Ok(());
        }
        Reflect::set(&finish_ref, &JsValue::from_str("current"), &JsValue::TRUE)?;
        finish_complete.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let finish = required_function(&modules.react, "useCallback", "React")?
        .call2(
            &modules.react,
            &finish_callback,
            &Array::of1(finish_dependency.as_ref()),
        )?
        .dyn_into::<Function>()?;
    let status = required(&state, "status", "welcome state")?
        .as_string()
        .unwrap_or_default();
    let acknowledged = required(&state, "acknowledged", "welcome state")?
        .as_bool()
        .unwrap_or(false);
    install_load_effect(&modules.react, &controller, &status)?;
    install_finish_effect(&modules.react, &finish, acknowledged)?;
    if matches!(status.as_str(), "idle" | "loading") || acknowledged {
        return Ok(JsValue::NULL);
    }
    let acknowledge_controller = controller;
    let acknowledge_finish = finish;
    let acknowledge = Closure::wrap(Box::new(move || {
        let returned = call_method(&acknowledge_controller, "acknowledge", &[]);
        let finish = acknowledge_finish.clone();
        spawn_local(async move {
            if let Ok(returned) = returned
                && let Ok(accepted) = JsFuture::from(Promise::resolve(&returned)).await
                && accepted.as_bool() == Some(true)
            {
                let _ = finish.call0(&JsValue::UNDEFINED);
            }
        });
    }) as Box<dyn FnMut()>);
    let body = translated(&translate, "welcomeBody")?
        .as_string()
        .unwrap_or_default();
    let mut paragraphs = Vec::new();
    for paragraph in body.split("\n\n") {
        paragraphs.push(tag(
            &modules.react,
            "p",
            Some(&object(&[("key", JsValue::from_str(paragraph))])?),
            &[JsValue::from_str(paragraph)],
        )?);
    }
    let mut children = vec![tag(
        &modules.react,
        "div",
        Some(&class(&css("copy"))?),
        &paragraphs,
    )?];
    if !required(&state, "error", "welcome state")?.is_null() {
        children.push(tag(
            &modules.react,
            "p",
            Some(&object(&[
                ("className", JsValue::from_str(&css("error"))),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[translated(&translate, "welcomeError")?],
        )?);
    }
    let button = create_element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&[
            ("variant", JsValue::from_str("primary")),
            ("className", JsValue::from_str(&css("primary"))),
            ("disabled", JsValue::from_bool(status == "saving")),
            ("onClick", acknowledge.into_js_value()),
        ])?),
        &[translated(&translate, "welcomeContinue")?],
    )?;
    children.push(tag(
        &modules.react,
        "div",
        Some(&class(&css("actions"))?),
        &[button],
    )?);
    create_element(
        &modules.react,
        &configured_components()?.onboarding_modal,
        Some(&object(&[
            ("title", translated(&translate, "welcomeTitle")?),
            ("focusTitle", JsValue::TRUE),
        ])?),
        &children,
    )
}

fn render_deepseek_onboarding(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let complete = required_function(props, "complete", "DeepSeek onboarding props")?;
    let controller = required(props, "controller", "DeepSeek onboarding props")?;
    let use_models = required_function(props, "useModels", "DeepSeek onboarding props")?;
    let translate = required_function(props, "t", "DeepSeek onboarding props")?;
    let selector = Closure::wrap(
        Box::new(move |snapshot: JsValue| snapshot) as Box<dyn FnMut(JsValue) -> JsValue>
    );
    let state = use_models.call1(&JsValue::UNDEFINED, &selector.into_js_value())?;
    let status = required(&state, "status", "models state")?
        .as_string()
        .unwrap_or_default();
    install_load_effect(&modules.react, &controller, &status)?;
    let kind = onboarding_kind(&state)?;
    let completes = matches!(
        kind.as_str(),
        "adapter-absent" | "provider-ready" | "unavailable"
    );
    install_complete_effect(&modules.react, &complete, completes, &kind)?;
    if kind != "credential-missing" {
        return Ok(JsValue::NULL);
    }
    let row = official_row(&required(&state, "rows", "models state")?)?;
    let namespaces = required(&state, "namespaces", "models state")?.dyn_into::<Map>()?;
    let namespace = namespaces.get(&JsValue::from_str("llm-deepseek"));
    if row.is_none() || namespace.is_undefined() {
        return Ok(JsValue::NULL);
    }
    let row = row.unwrap_or(JsValue::UNDEFINED);
    let finish_controller = controller;
    let finish_complete = complete;
    let finish = Closure::wrap(Box::new(move |changed: bool| -> Result<(), JsValue> {
        if changed {
            call_method(&finish_controller, "load", &[])?;
        } else {
            finish_complete.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut(bool) -> Result<(), JsValue>>);
    let entry = required(&row, "entry", "official provider row")?;
    let editor = create_element(
        &modules.react,
        &configured_components()?.provider_editor,
        Some(&object(&[
            (
                "provider",
                required(&entry, "provider", "official provider entry")?,
            ),
            (
                "displayName",
                required(&entry, "displayName", "official provider entry")?,
            ),
            ("namespace", namespace),
            (
                "settingsPath",
                required(&entry, "settingsPath", "official provider entry")?,
            ),
            ("api", required(props, "api", "DeepSeek onboarding props")?),
            ("t", translate.clone().into()),
            ("readOnly", JsValue::FALSE),
            ("hideTitle", JsValue::TRUE),
            ("credentialOnly", JsValue::TRUE),
            ("credentialRequired", JsValue::TRUE),
            ("autoFocusCredential", JsValue::TRUE),
            ("cancelLabel", JsValue::from_str("onboardingLater")),
            ("submitLabel", JsValue::from_str("onboardingSave")),
            ("submitBusyLabel", JsValue::from_str("onboardingSaving")),
            ("onClose", finish.into_js_value()),
        ])?),
        &[],
    )?;
    let body = vec![
        tag(
            &modules.react,
            "p",
            Some(&class(&css("description"))?),
            &[translated(&translate, "onboardingDescription")?],
        )?,
        tag(
            &modules.react,
            "div",
            Some(&class(&css("editor"))?),
            &[editor],
        )?,
    ];
    create_element(
        &modules.react,
        &configured_components()?.onboarding_modal,
        Some(&object(&[(
            "title",
            translated(&translate, "onboardingTitle")?,
        )])?),
        &body,
    )
}

fn render_deepseek_models_editor(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    render_model_catalog(modules, props, true)
}

fn render_model_list_editor(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    render_model_catalog(modules, props, false)
}

fn model_chevron(modules: &BrowserModules, deepseek: bool, open: bool) -> Result<JsValue, JsValue> {
    if deepseek {
        return create_element(
            &modules.react,
            &modules.primitive(if open {
                "IconChevronDownOutline14"
            } else {
                "IconChevronRightOutline14"
            })?,
            None,
            &[],
        );
    }
    let path = tag(
        &modules.react,
        "path",
        Some(&object(&[
            ("d", JsValue::from_str("M6 3.5L10.5 8L6 12.5")),
            ("stroke", JsValue::from_str("currentColor")),
            ("strokeWidth", JsValue::from_f64(1.5)),
            ("strokeLinecap", JsValue::from_str("round")),
            ("strokeLinejoin", JsValue::from_str("round")),
        ])?),
        &[],
    )?;
    tag(
        &modules.react,
        "svg",
        Some(&object(&[
            ("width", JsValue::from_f64(14.0)),
            ("height", JsValue::from_f64(14.0)),
            ("viewBox", JsValue::from_str("0 0 16 16")),
            ("fill", JsValue::from_str("none")),
            ("aria-hidden", JsValue::TRUE),
            (
                "style",
                object(&[
                    (
                        "transform",
                        if open {
                            JsValue::from_str("rotate(90deg)")
                        } else {
                            JsValue::UNDEFINED
                        },
                    ),
                    ("transition", JsValue::from_str("transform 120ms ease")),
                ])?
                .into(),
            ),
        ])?),
        &[path],
    )
}

fn model_trash(modules: &BrowserModules, deepseek: bool) -> Result<JsValue, JsValue> {
    if deepseek {
        return create_element(
            &modules.react,
            &modules.primitive("IconTrashOutline16")?,
            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
            &[],
        );
    }
    let path = tag(
        &modules.react,
        "path",
        Some(&object(&[
            (
                "d",
                JsValue::from_str(
                    "M2.5 4h11M6.5 4V2.5h3V4M4 4l.7 9a1 1 0 001 .9h4.6a1 1 0 001-.9L12 4M6.5 6.8v4.4M9.5 6.8v4.4",
                ),
            ),
            ("stroke", JsValue::from_str("currentColor")),
            ("strokeWidth", JsValue::from_f64(1.3)),
            ("strokeLinecap", JsValue::from_str("round")),
            ("strokeLinejoin", JsValue::from_str("round")),
        ])?),
        &[],
    )?;
    tag(
        &modules.react,
        "svg",
        Some(&object(&[
            ("width", JsValue::from_f64(14.0)),
            ("height", JsValue::from_f64(14.0)),
            ("viewBox", JsValue::from_str("0 0 16 16")),
            ("fill", JsValue::from_str("none")),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[path],
    )
}

#[allow(clippy::too_many_lines)] // One row grammar owns add/edit/remove/disclosure behavior.
fn render_model_catalog(
    modules: &BrowserModules,
    props: &JsValue,
    deepseek: bool,
) -> Result<JsValue, JsValue> {
    let models = required(props, "models", "model editor props")?.dyn_into::<Array>()?;
    let translate = required_function(props, "t", "model editor props")?;
    let disabled = required(props, "disabled", "model editor props")?
        .as_bool()
        .unwrap_or(false);
    let (expanded, set_expanded, editing, set_editing, fetch) = if deepseek {
        let (editing, set_editing) = use_state(&modules.react, &Map::new().into())?;
        let (expanded, set_expanded) =
            use_state(&modules.react, &Set::new(&JsValue::UNDEFINED).into())?;
        (expanded, set_expanded, editing, set_editing, None)
    } else {
        let (busy, set_busy) = use_state(&modules.react, &JsValue::FALSE)?;
        let (failure, set_failure) = use_state(&modules.react, &JsValue::UNDEFINED)?;
        let (candidates, set_candidates) = use_state(&modules.react, &JsValue::UNDEFINED)?;
        let (picked, set_picked) =
            use_state(&modules.react, &Set::new(&JsValue::UNDEFINED).into())?;
        let (expanded, set_expanded) =
            use_state(&modules.react, &Set::new(&JsValue::UNDEFINED).into())?;
        let (editing, set_editing) = use_state(&modules.react, &Map::new().into())?;
        (
            expanded,
            set_expanded,
            editing,
            set_editing,
            Some(FetchCatalogState {
                busy,
                set_busy,
                failure,
                set_failure,
                candidates,
                set_candidates,
                picked,
                set_picked,
            }),
        )
    };
    let expanded = expanded.dyn_into::<Set>()?;
    let editing = editing.dyn_into::<Map>()?;
    let mut entries = Vec::new();
    for index in 0..models.length() {
        let model = models.get(index);
        let open = expanded.has(&JsValue::from_f64(f64::from(index)));
        let toggle_set = set_expanded.clone();
        let toggle_current = expanded.clone();
        let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let next = Set::new(&toggle_current);
            let key = JsValue::from_f64(f64::from(index));
            if next.has(&key) {
                next.delete(&key);
            } else {
                next.add(&key);
            }
            toggle_set.call1(&JsValue::UNDEFINED, &next)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let id = Reflect::get(&model, &JsValue::from_str("id"))?;
        let name = Reflect::get(&model, &JsValue::from_str("name"))?;
        let id_change = model_field_change(props, &models, index, "id", false, deepseek)?;
        let id_blur = model_id_blur(props, &models, index, deepseek)?;
        let name_change = model_field_change(props, &models, index, "name", true, deepseek)?;
        let remove = model_remove(
            props,
            &models,
            index,
            &editing,
            &set_editing,
            &expanded,
            &set_expanded,
            deepseek,
        )?;
        let mut children = vec![tag(
            &modules.react,
            "div",
            Some(&class(&css("modelRow"))?),
            &[
                tag(
                    &modules.react,
                    "input",
                    Some(&object(&[
                        ("className", JsValue::from_str(&css("input"))),
                        ("type", JsValue::from_str("text")),
                        (
                            "value",
                            id.as_string()
                                .as_deref()
                                .map_or(JsValue::from_str(""), JsValue::from_str),
                        ),
                        ("placeholder", translated(&translate, "modelId")?),
                        (
                            "aria-label",
                            JsValue::from_str(&format!(
                                "{} {}",
                                translated(&translate, "modelId")?
                                    .as_string()
                                    .unwrap_or_default(),
                                index + 1
                            )),
                        ),
                        ("disabled", JsValue::from_bool(disabled)),
                        ("onChange", id_change),
                        ("onBlur", id_blur),
                    ])?),
                    &[],
                )?,
                tag(
                    &modules.react,
                    "input",
                    Some(&object(&[
                        ("className", JsValue::from_str(&css("input"))),
                        ("type", JsValue::from_str("text")),
                        (
                            "value",
                            name.as_string()
                                .as_deref()
                                .map_or(JsValue::from_str(""), JsValue::from_str),
                        ),
                        ("placeholder", translated(&translate, "modelName")?),
                        (
                            "aria-label",
                            JsValue::from_str(&format!(
                                "{} {}",
                                translated(&translate, "modelName")?
                                    .as_string()
                                    .unwrap_or_default(),
                                index + 1
                            )),
                        ),
                        ("disabled", JsValue::from_bool(disabled)),
                        ("onChange", name_change),
                    ])?),
                    &[],
                )?,
                tag(
                    &modules.react,
                    "button",
                    Some(&object(&[
                        ("type", JsValue::from_str("button")),
                        ("className", JsValue::from_str(&css("iconButton"))),
                        (
                            "aria-label",
                            JsValue::from_str(&format!(
                                "{} {}",
                                translated(&translate, "modelAdvanced")?
                                    .as_string()
                                    .unwrap_or_default(),
                                index + 1
                            )),
                        ),
                        ("aria-expanded", JsValue::from_bool(open)),
                        ("title", translated(&translate, "modelAdvanced")?),
                        ("onClick", toggle.into_js_value()),
                    ])?),
                    &[model_chevron(modules, deepseek, open)?],
                )?,
                tag(
                    &modules.react,
                    "button",
                    Some(&object(&[
                        ("type", JsValue::from_str("button")),
                        (
                            "className",
                            JsValue::from_str(&format!(
                                "{} {}",
                                css("iconButton"),
                                css("iconButtonDanger")
                            )),
                        ),
                        (
                            "aria-label",
                            JsValue::from_str(&format!(
                                "{} {}",
                                translated(&translate, "removeModel")?
                                    .as_string()
                                    .unwrap_or_default(),
                                index + 1
                            )),
                        ),
                        ("title", translated(&translate, "removeModel")?),
                        ("disabled", JsValue::from_bool(disabled)),
                        ("onClick", remove),
                    ])?),
                    &[model_trash(modules, deepseek)?],
                )?,
            ],
        )?];
        if open {
            children.push(capacity_fields(
                modules,
                props,
                &models,
                &model,
                index,
                &editing,
                &set_editing,
                deepseek,
            )?);
        }
        entries.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("className", JsValue::from_str(&css("modelEntry"))),
                ("key", JsValue::from_f64(f64::from(index))),
            ])?),
            &children,
        )?);
    }
    let mut body = Vec::new();
    let overridden = optional(props, "overridden")?.and_then(|value| value.as_bool());
    let mut heading = vec![tag(
        &modules.react,
        "span",
        Some(&class(&css("modelCatalogTitle"))?),
        &[translated(&translate, "models")?],
    )?];
    if let Some(overridden) = overridden {
        heading.push(tag(
            &modules.react,
            "span",
            Some(&class(&css("modelCatalogMeta"))?),
            &[translated(
                &translate,
                if overridden {
                    "modelsCustomized"
                } else {
                    "modelsInherited"
                },
            )?],
        )?);
    }
    let mut header_children = vec![tag(
        &modules.react,
        "div",
        Some(&class(&css("modelCatalogHeading"))?),
        &heading,
    )?];
    if overridden == Some(true)
        && let Some(reset) = optional(props, "onReset")?
    {
        let reset = if deepseek {
            let reset_editing = set_editing.clone();
            let reset_expanded = set_expanded.clone();
            let reset = reset.dyn_into::<Function>()?;
            Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                reset_editing.call1(&JsValue::UNDEFINED, &Map::new())?;
                reset_expanded.call1(&JsValue::UNDEFINED, &Set::new(&JsValue::UNDEFINED))?;
                reset.call0(&JsValue::UNDEFINED)?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>)
            .into_js_value()
        } else {
            reset
        };
        header_children.push(tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("className", JsValue::from_str(&css("linkButton"))),
                ("disabled", JsValue::from_bool(disabled)),
                ("onClick", reset),
            ])?),
            &[translated(&translate, "resetModels")?],
        )?);
    }
    if let Some(fetch) = fetch.as_ref() {
        header_children.push(fetch_models_button(
            modules,
            props,
            &models,
            disabled,
            fetch.busy.as_bool().unwrap_or(false),
            &fetch.set_busy,
            &fetch.set_failure,
            &fetch.set_candidates,
            &fetch.set_picked,
        )?);
    }
    body.push(tag(
        &modules.react,
        "div",
        Some(&class(&css("modelListHead"))?),
        &header_children,
    )?);
    if models.length() == 0 {
        body.push(tag(
            &modules.react,
            "p",
            Some(&class(&css("modelEmpty"))?),
            &[translated(&translate, "modelsEmpty")?],
        )?);
    } else if deepseek {
        body.push(tag(
            &modules.react,
            "div",
            Some(&class(&css("modelList"))?),
            &entries,
        )?);
    } else {
        body.extend(entries);
    }
    body.push(add_model_button(
        modules, props, &models, disabled, deepseek,
    )?);
    if let Some(fetch) = fetch {
        if !fetch.failure.is_undefined() {
            body.push(tag(
                &modules.react,
                "p",
                Some(&class(&css("error"))?),
                std::slice::from_ref(&fetch.failure),
            )?);
        }
        body.push(render_candidate_modal(
            modules,
            props,
            &models,
            &fetch.candidates,
            &fetch.picked.dyn_into::<Set>()?,
            &fetch.set_candidates,
            &fetch.set_picked,
        )?);
    }
    tag(
        &modules.react,
        "section",
        Some(&object(&[
            ("className", JsValue::from_str(&css("modelCatalog"))),
            ("aria-label", translated(&translate, "models")?),
        ])?),
        &body,
    )
}

fn render_provider_editor(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    crate::browser_apply::render_provider_editor_surface(modules, props)
}

fn render_custom_provider_card(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    crate::browser_apply::render_custom_provider_surface(modules, props)
}

fn render_models_section(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    if ["controller", "useSnapshot", "api", "t"]
        .iter()
        .any(|key| optional(props, key).ok().flatten().is_none())
    {
        return Ok(JsValue::NULL);
    }
    crate::browser_apply::render_models_section_surface(modules, props)
}

fn install_inert_effect(react: &JsValue) -> Result<(), JsValue> {
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
        let root = call_method(&document, "getElementById", &[JsValue::from_str("root")])?;
        if root.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        let previous = Reflect::get(&root, &JsValue::from_str("inert"))?;
        Reflect::set(&root, &JsValue::from_str("inert"), &JsValue::TRUE)?;
        let cleanup_root = root;
        Ok(Closure::wrap(Box::new(move || {
            let _ = Reflect::set(&cleanup_root, &JsValue::from_str("inert"), &previous);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::new(),
    )?;
    Ok(())
}

fn install_focus_effect(react: &JsValue, title_ref: &JsValue, focus: bool) -> Result<(), JsValue> {
    let reference = title_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if focus {
            let current = Reflect::get(&reference, &JsValue::from_str("current"))?;
            if !current.is_null() {
                call_method(&current, "focus", &[])?;
            }
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of1(&JsValue::from_bool(focus)),
    )?;
    Ok(())
}

fn install_load_effect(react: &JsValue, controller: &JsValue, status: &str) -> Result<(), JsValue> {
    let load_controller = controller.clone();
    let idle = status == "idle";
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if idle {
            call_method(&load_controller, "load", &[])?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of2(controller, &JsValue::from_str(status)),
    )?;
    Ok(())
}

fn install_finish_effect(
    react: &JsValue,
    finish: &Function,
    acknowledged: bool,
) -> Result<(), JsValue> {
    let finish_dependency = finish.clone();
    let finish = finish.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if acknowledged {
            finish.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of2(
            finish_dependency.as_ref(),
            &JsValue::from_bool(acknowledged),
        ),
    )?;
    Ok(())
}

fn install_complete_effect(
    react: &JsValue,
    complete: &Function,
    should_complete: bool,
    kind: &str,
) -> Result<(), JsValue> {
    let complete_dependency = complete.clone();
    let complete = complete.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if should_complete {
            complete.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of2(complete_dependency.as_ref(), &JsValue::from_str(kind)),
    )?;
    Ok(())
}

fn official_row(rows: &JsValue) -> Result<Option<JsValue>, JsValue> {
    let rows = rows.clone().dyn_into::<Array>()?;
    for row in rows.iter() {
        let entry = required(&row, "entry", "provider row")?;
        if required(&entry, "provider", "provider entry")?
            .as_string()
            .as_deref()
            == Some("deepseek-official")
            && required(&entry, "settingsNs", "provider entry")?
                .as_string()
                .as_deref()
                == Some("llm-deepseek")
            && required(&entry, "settingsPath", "provider entry")?
                .dyn_into::<Array>()?
                .length()
                == 0
        {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

fn onboarding_kind(state: &JsValue) -> Result<String, JsValue> {
    let status = required(state, "status", "models state")?
        .as_string()
        .unwrap_or_default();
    let rows = required(state, "rows", "models state")?.dyn_into::<Array>()?;
    if matches!(status.as_str(), "idle" | "loading") && rows.length() == 0 {
        return Ok("loading".to_owned());
    }
    if status == "error" {
        return Ok("unavailable".to_owned());
    }
    for row in rows.iter() {
        let entry = required(&row, "entry", "provider row")?;
        let active = required(&entry, "active", "provider entry")?.as_bool() == Some(true);
        let usable = active
            && match optional(&row, "apiKeyEnv")? {
                None => true,
                Some(_) => {
                    optional(&row, "credential")?
                        .and_then(|credential| optional(&credential, "configured").ok().flatten())
                        .and_then(|configured| configured.as_bool())
                        == Some(true)
                }
            };
        if usable {
            return Ok("provider-ready".to_owned());
        }
    }
    let Some(row) = official_row(rows.as_ref())? else {
        return Ok("adapter-absent".to_owned());
    };
    let entry = required(&row, "entry", "official provider row")?;
    if required(&entry, "active", "official provider entry")?.as_bool() != Some(true) {
        return Ok("unavailable".to_owned());
    }
    if !required(state, "credentialError", "models state")?.is_null()
        || optional(&row, "credential")?.is_none()
        || required(state, "writable", "models state")?.as_bool() != Some(true)
        || optional(&row, "credential")?
            .and_then(|credential| optional(&credential, "writable").ok().flatten())
            .and_then(|writable| writable.as_bool())
            != Some(true)
    {
        return Ok("unavailable".to_owned());
    }
    Ok("credential-missing".to_owned())
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
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
    .ok_or_else(|| js_sys::TypeError::new("input target value must be a string").into())
}

fn clone_object(value: &JsValue) -> Result<Object, JsValue> {
    let source = value
        .clone()
        .dyn_into::<Object>()
        .map_err(|_| js_sys::TypeError::new("model row must be an object"))?;
    Ok(Object::assign(&Object::new(), &source))
}

fn model_field_change(
    props: &JsValue,
    models: &Array,
    index: u32,
    field: &'static str,
    optional_field: bool,
    deepseek: bool,
) -> Result<JsValue, JsValue> {
    let change = required_function(props, "onChange", "model editor props")?;
    let models = models.clone();
    Ok(
        Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let value = event_value(&event)?;
            let next = Array::new();
            for at in 0..models.length() {
                if !deepseek && at != index {
                    next.push(&models.get(at));
                    continue;
                }
                let copy = clone_object(&models.get(at))?;
                if at == index {
                    if optional_field && value.is_empty() {
                        Reflect::delete_property(&copy, &JsValue::from_str(field))?;
                    } else {
                        Reflect::set(&copy, &JsValue::from_str(field), &JsValue::from_str(&value))?;
                    }
                }
                next.push(&copy);
            }
            change.call1(&JsValue::UNDEFINED, &next)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value(),
    )
}

fn model_id_blur(
    props: &JsValue,
    models: &Array,
    index: u32,
    deepseek: bool,
) -> Result<JsValue, JsValue> {
    let change = required_function(props, "onChange", "model editor props")?;
    let models = models.clone();
    Ok(
        Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let value = event_value(&event)?;
            let trimmed = trim_ecmascript_whitespace(&value);
            if trimmed == value {
                return Ok(());
            }
            let next = Array::new();
            for at in 0..models.length() {
                if !deepseek && at != index {
                    next.push(&models.get(at));
                    continue;
                }
                let copy = clone_object(&models.get(at))?;
                if at == index {
                    Reflect::set(&copy, &JsValue::from_str("id"), &JsValue::from_str(trimmed))?;
                }
                next.push(&copy);
            }
            change.call1(&JsValue::UNDEFINED, &next)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value(),
    )
}

#[allow(clippy::too_many_arguments)]
fn model_remove(
    props: &JsValue,
    models: &Array,
    index: u32,
    editing: &Map,
    set_editing: &Function,
    expanded: &Set,
    set_expanded: &Function,
    deepseek: bool,
) -> Result<JsValue, JsValue> {
    let change = required_function(props, "onChange", "model editor props")?;
    let models = models.clone();
    let current_editing = editing.clone();
    let update_editing = set_editing.clone();
    let current_expanded = expanded.clone();
    let update_expanded = set_expanded.clone();
    Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let next = Array::new();
        for at in 0..models.length() {
            if at != index {
                if deepseek {
                    next.push(&clone_object(&models.get(at))?.into());
                } else {
                    next.push(&models.get(at));
                }
            }
        }
        if deepseek {
            update_editing.call1(
                &JsValue::UNDEFINED,
                &reindex_editing(&current_editing, index)?.into(),
            )?;
            update_expanded.call1(
                &JsValue::UNDEFINED,
                &reindex_expanded(&current_expanded, index)?.into(),
            )?;
            change.call1(&JsValue::UNDEFINED, &next)?;
        } else {
            change.call1(&JsValue::UNDEFINED, &next)?;
            update_expanded.call1(
                &JsValue::UNDEFINED,
                &reindex_expanded(&current_expanded, index)?.into(),
            )?;
            update_editing.call1(
                &JsValue::UNDEFINED,
                &reindex_editing(&current_editing, index)?.into(),
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // One capacity pair owns text buffering and settlement.
fn capacity_fields(
    modules: &BrowserModules,
    props: &JsValue,
    models: &Array,
    model: &JsValue,
    index: u32,
    editing: &Map,
    set_editing: &Function,
    deepseek: bool,
) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "model editor props")?;
    let discovered_catalog = optional(props, "probe")?.is_some();
    let mut fields = Vec::new();
    for field in ["contextWindow", "maxTokens"] {
        let key = match (discovered_catalog, field) {
            (true, "contextWindow") => "modelContextWindow",
            (true, _) => "modelMaxTokens",
            (false, "contextWindow") => "contextWindow",
            (false, _) => "maxTokens",
        };
        let buffer_key = format!("{index}:{field}");
        let typed = editing.get(&JsValue::from_str(&buffer_key));
        let value = typed.as_string().unwrap_or_else(|| {
            Reflect::get(model, &JsValue::from_str(field))
                .ok()
                .and_then(|value| value.as_f64())
                .map_or_else(String::new, format_capacity)
        });
        let change = required_function(props, "onChange", "model editor props")?;
        let source = models.clone();
        let current_editing = editing.clone();
        let update_editing = set_editing.clone();
        let field_name = field;
        let change_key = buffer_key.clone();
        let on_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let text = event_value(&event)?;
            let next_editing = clone_map(&current_editing)?;
            next_editing.set(&JsValue::from_str(&change_key), &JsValue::from_str(&text));
            update_editing.call1(&JsValue::UNDEFINED, &next_editing)?;
            let parsed = parse_capacity(&text);
            let next = Array::new();
            for at in 0..source.length() {
                if !deepseek && at != index {
                    next.push(&source.get(at));
                    continue;
                }
                let copy = clone_object(&source.get(at))?;
                if at == index {
                    match parsed {
                        None => {
                            Reflect::delete_property(&copy, &JsValue::from_str(field_name))?;
                        }
                        Some(value) => {
                            Reflect::set(
                                &copy,
                                &JsValue::from_str(field_name),
                                &JsValue::from_f64(value),
                            )?;
                        }
                    }
                }
                next.push(&copy);
            }
            change.call1(&JsValue::UNDEFINED, &next)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let on_blur = if discovered_catalog {
            None
        } else {
            let blur_editing = editing.clone();
            let blur_setter = set_editing.clone();
            let blur_key = buffer_key;
            Some(
                Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                    let typed = blur_editing.get(&JsValue::from_str(&blur_key));
                    let Some(parsed) = typed.as_string().as_deref().and_then(parse_capacity) else {
                        if typed.is_undefined() {
                            return Ok(());
                        }
                        let next = clone_map(&blur_editing)?;
                        next.delete(&JsValue::from_str(&blur_key));
                        blur_setter.call1(&JsValue::UNDEFINED, &next)?;
                        return Ok(());
                    };
                    if parsed.is_nan() {
                        return Ok(());
                    }
                    let next = clone_map(&blur_editing)?;
                    next.delete(&JsValue::from_str(&blur_key));
                    blur_setter.call1(&JsValue::UNDEFINED, &next)?;
                    Ok(())
                })
                    as Box<dyn FnMut() -> Result<(), JsValue>>)
                .into_js_value(),
            )
        };
        let label = translated(&translate, key)?.as_string().unwrap_or_default();
        let default_key = if field == "contextWindow" {
            "defaultContextWindow"
        } else {
            "defaultMaxTokens"
        };
        let placeholder = optional(props, default_key)?
            .and_then(|value| value.as_f64())
            .map_or_else(
                || {
                    if optional(props, "probe").ok().flatten().is_some() {
                        if field == "contextWindow" {
                            "256K".to_owned()
                        } else {
                            "32K".to_owned()
                        }
                    } else {
                        translated(
                            &translate,
                            if field == "contextWindow" {
                                "contextWindowPlaceholder"
                            } else {
                                "maxTokensPlaceholder"
                            },
                        )
                        .ok()
                        .and_then(|value| value.as_string())
                        .unwrap_or_default()
                    }
                },
                format_capacity,
            );
        let mut input_props = vec![
            ("className", JsValue::from_str(&css("input"))),
            ("type", JsValue::from_str("text")),
            ("inputMode", JsValue::from_str("numeric")),
            ("value", JsValue::from_str(&value)),
            ("placeholder", JsValue::from_str(&placeholder)),
            (
                "aria-label",
                JsValue::from_str(&format!("{label} {}", index + 1)),
            ),
            (
                "disabled",
                required(props, "disabled", "model editor props")?,
            ),
            ("onChange", on_change.into_js_value()),
        ];
        if let Some(on_blur) = on_blur {
            input_props.push(("onBlur", on_blur));
        }
        fields.push(tag(
            &modules.react,
            "label",
            Some(&class(&css("modelField"))?),
            &[
                tag(
                    &modules.react,
                    "span",
                    Some(&class(&css("modelFieldLabel"))?),
                    &[JsValue::from_str(&label)],
                )?,
                tag(&modules.react, "input", Some(&object(&input_props)?), &[])?,
            ],
        )?);
    }
    tag(
        &modules.react,
        "div",
        Some(&class(&css("modelAdvanced"))?),
        &fields,
    )
}

fn reindex_editing(current: &Map, removed: u32) -> Result<Map, JsValue> {
    let next = Map::new();
    if let Some(entries) = try_iter(&current.entries())? {
        for entry in entries {
            let pair = Array::from(&entry?);
            let key = pair.get(0).as_string().unwrap_or_default();
            let separator = key.find(':').unwrap_or(key.len());
            let at = key[..separator].parse::<u32>().unwrap_or(u32::MAX);
            if at == removed {
                continue;
            }
            let shifted = if at > removed {
                format!("{}{}", at - 1, &key[separator..])
            } else {
                key
            };
            next.set(&JsValue::from_str(&shifted), &pair.get(1));
        }
    }
    Ok(next)
}

fn clone_map(current: &Map) -> Result<Map, JsValue> {
    let next = Map::new();
    if let Some(entries) = try_iter(&current.entries())? {
        for entry in entries {
            let pair = Array::from(&entry?);
            next.set(&pair.get(0), &pair.get(1));
        }
    }
    Ok(next)
}

#[allow(clippy::float_cmp)] // JavaScript Set members are exact integer row indexes.
fn reindex_expanded(current: &Set, removed: u32) -> Result<Set, JsValue> {
    let next = Set::new(&JsValue::UNDEFINED);
    if let Some(values) = try_iter(&current.values())? {
        for value in values {
            let value = value?.as_f64().unwrap_or(f64::NAN);
            if value == f64::from(removed) {
                continue;
            }
            next.add(&JsValue::from_f64(if value > f64::from(removed) {
                value - 1.0
            } else {
                value
            }));
        }
    }
    Ok(next)
}

fn add_model_button(
    modules: &BrowserModules,
    props: &JsValue,
    models: &Array,
    disabled: bool,
    deepseek: bool,
) -> Result<JsValue, JsValue> {
    let change = required_function(props, "onChange", "model editor props")?;
    let models = models.clone();
    let add = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let next = Array::new();
        for model in models.iter() {
            if deepseek {
                next.push(&clone_object(&model)?.into());
            } else {
                next.push(&model);
            }
        }
        next.push(&object(&[("id", JsValue::from_str(""))])?.into());
        change.call1(&JsValue::UNDEFINED, &next)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let translate = required_function(props, "t", "model editor props")?;
    let mut children = Vec::new();
    if deepseek {
        children.push(create_element(
            &modules.react,
            &modules.primitive("IconPlusOutline16")?,
            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
            &[],
        )?);
    }
    children.push(translated(&translate, "addModel")?);
    tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&css("addModelButton"))),
            ("disabled", JsValue::from_bool(disabled)),
            ("onClick", add.into_js_value()),
        ])?),
        &children,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // One fetch transaction owns all picker state.
fn fetch_models_button(
    modules: &BrowserModules,
    props: &JsValue,
    models: &Array,
    disabled: bool,
    busy: bool,
    set_busy: &Function,
    set_failure: &Function,
    set_candidates: &Function,
    set_picked: &Function,
) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "model editor props")?;
    let probe = required(props, "probe", "ModelListEditor props")?;
    let askable = optional(&probe, "provider")?.is_some()
        || optional(&probe, "baseURL")?
            .and_then(|value| value.as_string())
            .is_some_and(|value| !value.is_empty());
    let blocked = optional(props, "probeBlocked")?;
    let api = required(props, "api", "ModelListEditor props")?;
    let known = models
        .iter()
        .filter_map(|model| {
            Reflect::get(&model, &JsValue::from_str("id"))
                .ok()
                .and_then(|value| value.as_string())
        })
        .collect::<Vec<_>>();
    let click_probe = discovery_request(&probe)?;
    let click_translate = translate.clone();
    let click_set_busy = set_busy.clone();
    let click_set_failure = set_failure.clone();
    let click_set_candidates = set_candidates.clone();
    let click_set_picked = set_picked.clone();
    let click = Closure::wrap(Box::new(move || {
        let set_busy = click_set_busy.clone();
        let set_failure = click_set_failure.clone();
        let set_candidates = click_set_candidates.clone();
        let set_picked = click_set_picked.clone();
        let known = known.clone();
        let translate = click_translate.clone();
        let _ = set_busy.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
        let _ = set_failure.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED);
        let returned = required(&api, "llm", "API client").and_then(|llm| {
            call_method(&llm, "discoverModels", std::slice::from_ref(&click_probe))
        });
        spawn_local(async move {
            let result = match returned {
                Ok(returned) => JsFuture::from(Promise::resolve(&returned))
                    .await
                    .map_err(|error| rejection_text(&error))
                    .and_then(|value| component_rpc_value(&value)),
                Err(error) => Err(rejection_text(&error)),
            };
            match result {
                Ok(value) => {
                    let found = required(&value, "models", "discoverModels value")
                        .ok()
                        .and_then(|value| value.dyn_into::<Array>().ok())
                        .unwrap_or_default();
                    if found.length() == 0 {
                        let copy = translated(&translate, "fetchEmpty")
                            .unwrap_or(JsValue::from_str("fetchEmpty"));
                        let _ = set_failure.call1(&JsValue::UNDEFINED, &copy);
                    } else {
                        let selected = Set::new(&JsValue::UNDEFINED);
                        for candidate in found.iter() {
                            let id = Reflect::get(&candidate, &JsValue::from_str("id"))
                                .ok()
                                .and_then(|value| value.as_string())
                                .unwrap_or_default();
                            if !known.contains(&id) {
                                selected.add(&JsValue::from_str(&id));
                            }
                        }
                        let _ = set_candidates.call1(&JsValue::UNDEFINED, &found);
                        let _ = set_picked.call1(&JsValue::UNDEFINED, &selected);
                    }
                }
                Err(error) => {
                    let _ = set_failure.call1(&JsValue::UNDEFINED, &JsValue::from_str(&error));
                }
            }
            let _ = set_busy.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
        });
    }) as Box<dyn FnMut()>);
    tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&css("linkButton"))),
            (
                "disabled",
                JsValue::from_bool(disabled || busy || !askable || blocked.is_some()),
            ),
            (
                "title",
                blocked.map_or_else(
                    || {
                        if askable {
                            Ok(JsValue::UNDEFINED)
                        } else {
                            translated(&translate, "fetchNeedsBaseUrl")
                        }
                    },
                    |key| translate.call1(&JsValue::UNDEFINED, &key),
                )?,
            ),
            ("onClick", click.into_js_value()),
        ])?),
        &[translated(
            &translate,
            if busy { "fetching" } else { "fetchModels" },
        )?],
    )
}

fn discovery_request(probe: &JsValue) -> Result<JsValue, JsValue> {
    let request = Object::new();
    Reflect::set(
        &request,
        &JsValue::from_str("settingsNs"),
        &required(probe, "settingsNs", "model discovery probe")?,
    )?;
    for field in ["provider", "api", "apiKey"] {
        if let Some(value) = optional(probe, field)? {
            Reflect::set(&request, &JsValue::from_str(field), &value)?;
        }
    }
    if let Some(base_url) = optional(probe, "baseURL")?
        && base_url.as_string().is_some_and(|value| !value.is_empty())
    {
        Reflect::set(&request, &JsValue::from_str("baseURL"), &base_url)?;
    }
    Ok(request.into())
}

#[allow(clippy::too_many_lines)] // Candidate selection and adoption stay in one dialog renderer.
fn render_candidate_modal(
    modules: &BrowserModules,
    props: &JsValue,
    models: &Array,
    candidates: &JsValue,
    picked: &Set,
    set_candidates: &Function,
    set_picked: &Function,
) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "ModelListEditor props")?;
    let open = !candidates.is_undefined();
    let candidates = if open {
        candidates.clone().dyn_into::<Array>()?
    } else {
        Array::new()
    };
    let mut rows = Vec::new();
    for candidate in candidates.iter() {
        let id = required(&candidate, "id", "discovered model")?
            .as_string()
            .unwrap_or_default();
        let checked = picked.has(&JsValue::from_str(&id));
        let toggle_picked = picked.clone();
        let toggle_setter = set_picked.clone();
        let toggle_id = id.clone();
        let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let next = clone_set(&toggle_picked)?;
            let id = JsValue::from_str(&toggle_id);
            if next.has(&id) {
                next.delete(&id);
            } else {
                next.add(&id);
            }
            toggle_setter.call1(&JsValue::UNDEFINED, &next)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let label = tag(
            &modules.react,
            "label",
            Some(&class(&css("candidateLabel"))?),
            &[
                tag(
                    &modules.react,
                    "input",
                    Some(&object(&[
                        ("type", JsValue::from_str("checkbox")),
                        ("checked", JsValue::from_bool(checked)),
                        ("onChange", toggle.into_js_value()),
                    ])?),
                    &[],
                )?,
                tag(
                    &modules.react,
                    "span",
                    Some(&class(&css("candidateId"))?),
                    &[JsValue::from_str(&id)],
                )?,
            ],
        )?;
        rows.push(tag(
            &modules.react,
            "li",
            Some(&object(&[
                ("key", JsValue::from_str(&id)),
                ("className", JsValue::from_str(&css("candidate"))),
            ])?),
            &[label],
        )?);
    }
    let close_candidates = set_candidates.clone();
    let close_picked = set_picked.clone();
    let close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        close_candidates.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED)?;
        close_picked.call1(&JsValue::UNDEFINED, &Set::new(&JsValue::UNDEFINED))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .dyn_into::<Function>()?;
    let adopt_models = models.clone();
    let adopt_candidates = candidates;
    let adopt_picked = picked.clone();
    let adopt_change = required_function(props, "onChange", "ModelListEditor props")?;
    let adopt_close = close.clone();
    let adopt = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let by_id = Map::new();
        for model in adopt_models.iter() {
            let id = Reflect::get(&model, &JsValue::from_str("id"))?
                .as_string()
                .unwrap_or_default();
            by_id.set(&JsValue::from_str(&id), &model);
        }
        for candidate in adopt_candidates.iter() {
            let id = required(&candidate, "id", "discovered model")?
                .as_string()
                .unwrap_or_default();
            let key = JsValue::from_str(&id);
            if adopt_picked.has(&key) && !by_id.has(&key) {
                let model = Object::new();
                Reflect::set(&model, &JsValue::from_str("id"), &key)?;
                for field in ["name", "contextWindow", "maxTokens"] {
                    if let Some(value) = optional(&candidate, field)? {
                        Reflect::set(&model, &JsValue::from_str(field), &value)?;
                    }
                }
                by_id.set(&key, &model);
            }
        }
        let next = Array::new();
        if let Some(values) = try_iter(&by_id.values())? {
            for value in values {
                next.push(&value?);
            }
        }
        adopt_change.call1(&JsValue::UNDEFINED, &next)?;
        adopt_close.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let footer = create_element(
        &modules.react,
        &required(&modules.react, "Fragment", "React")?,
        None,
        &[
            create_element(
                &modules.react,
                &modules.primitive("Button")?,
                Some(&object(&[
                    ("variant", JsValue::from_str("outline")),
                    ("onClick", close.clone().into()),
                ])?),
                &[translated(&translate, "cancel")?],
            )?,
            create_element(
                &modules.react,
                &modules.primitive("Button")?,
                Some(&object(&[
                    ("variant", JsValue::from_str("outline")),
                    ("onClick", adopt.into_js_value()),
                ])?),
                &[translated(&translate, "fetchAdopt")?],
            )?,
        ],
    )?;
    let content = tag(
        &modules.react,
        "ul",
        Some(&class(&css("candidateList"))?),
        &rows,
    )?;
    create_element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("title", translated(&translate, "fetchTitle")?),
            ("onClose", close.into()),
            ("closeLabel", translated(&translate, "close")?),
            ("description", translated(&translate, "fetchDescription")?),
            ("className", JsValue::from_str(&css("fetchDialog"))),
            ("footer", footer),
        ])?),
        &[content],
    )
}

fn clone_set(current: &Set) -> Result<Set, JsValue> {
    let next = Set::new(&JsValue::UNDEFINED);
    if let Some(values) = try_iter(&current.values())? {
        for value in values {
            next.add(&value?);
        }
    }
    Ok(next)
}

fn component_rpc_value(response: &JsValue) -> Result<JsValue, String> {
    let result = required(response, "result", "RPC response").map_err(|error| {
        Reflect::get(&error, &JsValue::from_str("message"))
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default()
    })?;
    if required(&result, "ok", "RPC result")
        .ok()
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        required(&result, "value", "RPC result").map_err(|_| "missing RPC value".to_owned())
    } else {
        let error =
            required(&result, "error", "RPC result").map_err(|_| "missing RPC error".to_owned())?;
        Ok(())
            .and_then(|()| {
                required(&error, "message", "RPC error")
                    .map_err(|_| "missing RPC error message".to_owned())
            })?
            .as_string()
            .ok_or_else(|| "RPC error message must be a string".to_owned())
            .and_then(Err)
    }
}
