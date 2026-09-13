//! Rust/WASM two-level composer model and reasoning selector.

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use super::{
    BrowserModules, call_method, component as react_component, fragment, object, optional,
    required, required_bool, required_function, required_string, tag, translated,
    translated_values, use_effect, use_ref, use_state,
};
use crate::{
    ModelDirectoryState, ModelDirectoryStatus, ModelEntry, ModelProviderGroup, ModelReasoning,
    ModelSelection,
};

#[derive(Clone)]
struct Choice {
    group: ModelProviderGroup,
    model: ModelEntry,
}

#[derive(Clone)]
struct RenderContext {
    modules: BrowserModules,
    state: ModelDirectoryState,
    store: JsValue,
    load: Function,
    select: Function,
    translate: Function,
    set_open: Function,
    set_pane: Function,
    set_toast: Function,
    last_action_ref: JsValue,
    toast_seq_ref: JsValue,
    root_ref: JsValue,
    trigger_ref: JsValue,
    item_refs: JsValue,
    id: String,
    open: bool,
    item_index: Rc<Cell<u32>>,
    choices: Vec<Choice>,
    current_choice: Option<Choice>,
    reasoning: Option<ModelReasoning>,
    effective_effort: Option<String>,
    effort_label: Option<String>,
    busy: bool,
}

pub(crate) fn component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(Box::new(move |props: JsValue| render(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let locked = required_bool(props, "locked", "ModelSelect")?;
    let available = required_bool(props, "available", "ModelSelect")?;
    let store = required(props, "directory", "ModelSelect")?;
    let load = required_function(props, "load", "ModelSelect")?;
    let select = required_function(props, "select", "ModelSelect")?;
    let translate = required_function(props, "t", "ModelSelect")?;
    let state_value = use_store(&modules.react, &store)?;
    let state = serde_wasm_bindgen::from_value::<ModelDirectoryState>(state_value)
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    let (open, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let (pane, set_pane) = use_state(&modules.react, &JsValue::from_str("root"))?;
    let (toast, set_toast) = use_state(&modules.react, &JsValue::NULL)?;
    let last_action_ref = use_ref(&modules.react, &JsValue::from_str("load"))?;
    let toast_seq_ref = use_ref(&modules.react, &JsValue::from_f64(0.0))?;
    let root_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let trigger_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let item_refs = use_ref(&modules.react, &Array::new().into())?;
    let id = required_function(&modules.react, "useId", "React")?
        .call0(&modules.react)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("React.useId must return a string"))?;
    let open = open.as_bool().unwrap_or(false);
    let pane = pane.as_string().unwrap_or_else(|| "root".to_owned());
    let choices = flatten_choices(&state);
    let current_choice = state.current.as_ref().and_then(|current| {
        choices
            .iter()
            .find(|choice| choice.group.id == current.provider && choice.model.id == current.model)
            .cloned()
    });
    let reasoning = current_choice
        .as_ref()
        .and_then(|choice| choice.model.reasoning.clone());
    let effective_effort = state
        .current
        .as_ref()
        .and_then(|current| current.reasoning_effort.as_ref())
        .map(|effort| effort.as_str().to_owned())
        .or_else(|| {
            reasoning
                .as_ref()
                .and_then(|value| value.default_effort.as_ref())
                .map(|effort| effort.as_str().to_owned())
        });
    let effort_label = effort_label(reasoning.as_ref(), effective_effort.as_deref(), &translate)?;
    let busy = state.status == ModelDirectoryStatus::Selecting;

    install_load_effect(&modules.react, available, &load, &last_action_ref)?;
    install_outside_effect(&modules.react, open, &root_ref, &set_open)?;
    if !available {
        return Ok(JsValue::NULL);
    }
    Reflect::set(&item_refs, &JsValue::from_str("current"), &Array::new())?;
    let context = RenderContext {
        modules: modules.clone(),
        state,
        store,
        load,
        select,
        translate,
        set_open,
        set_pane,
        set_toast,
        last_action_ref,
        toast_seq_ref,
        root_ref: root_ref.clone(),
        trigger_ref: trigger_ref.clone(),
        item_refs,
        id,
        open,
        item_index: Rc::new(Cell::new(0)),
        choices,
        current_choice,
        reasoning,
        effective_effort,
        effort_label,
        busy,
    };
    let model_label = context.current_choice.as_ref().map_or_else(
        || translated_string(&context.translate, "trigger.fallback"),
        |choice| Ok(choice.model.name.clone()),
    )?;
    let trigger_label = context.effort_label.as_ref().map_or_else(
        || model_label.clone(),
        |effort| format!("{model_label} · {effort}"),
    );
    let trigger_aria = if context.current_choice.is_none() {
        translated(&context.translate, "trigger.selectAria")?
    } else if let Some(effort) = &context.effort_label {
        translated_values(
            &context.translate,
            "trigger.ariaEffort",
            &[
                ("model", JsValue::from_str(&model_label)),
                ("effort", JsValue::from_str(effort)),
            ],
        )?
    } else {
        translated_values(
            &context.translate,
            "trigger.aria",
            &[("model", JsValue::from_str(&model_label))],
        )?
    };
    let trigger = render_trigger(
        &context,
        locked,
        open,
        &model_label,
        &trigger_label,
        trigger_aria,
    )?;
    let mut children = vec![trigger];
    if open {
        children.push(render_menu(&context, &pane, &model_label)?);
    }
    if !toast.is_null() && !toast.is_undefined() {
        children.push(render_toast(&context, &toast)?);
    }
    let key_context = context.clone();
    let key_pane = pane;
    let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        on_root_key(&key_context, &key_pane, &event)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let blur_context = context;
    let blur = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        on_blur(&blur_context, &event)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("ref", root_ref),
            ("className", JsValue::from_str("seekdeep-model-root")),
            ("onKeyDown", keydown.into_js_value()),
            ("onBlur", blur.into_js_value()),
        ])?),
        &children,
    )
}

fn use_store(react: &JsValue, store: &JsValue) -> Result<JsValue, JsValue> {
    let subscribe_store = store.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        call_method(&subscribe_store, "subscribe", &[listener.into()])
    })
        as Box<dyn FnMut(Function) -> Result<JsValue, JsValue>>);
    let snapshot_store = store.clone();
    let snapshot = Closure::wrap(
        Box::new(move || call_method(&snapshot_store, "getSnapshot", &[]))
            as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
    );
    required_function(react, "useSyncExternalStore", "React")?.call2(
        react,
        &subscribe.into_js_value(),
        &snapshot.into_js_value(),
    )
}

fn flatten_choices(state: &ModelDirectoryState) -> Vec<Choice> {
    state
        .groups
        .iter()
        .flat_map(|group| {
            group.models.iter().map(|model| Choice {
                group: group.clone(),
                model: model.clone(),
            })
        })
        .collect()
}

fn effort_label(
    reasoning: Option<&ModelReasoning>,
    effective: Option<&str>,
    translate: &Function,
) -> Result<Option<String>, JsValue> {
    let Some(reasoning) = reasoning else {
        return Ok(None);
    };
    let label = match effective {
        None => translated_string(translate, "effort.providerDefault")?,
        Some(effort) => reasoning
            .efforts
            .iter()
            .find(|level| level.id.as_str() == effort)
            .map_or_else(|| effort.to_owned(), |level| level.name.clone()),
    };
    Ok(Some(label))
}

fn install_load_effect(
    react: &JsValue,
    available: bool,
    load: &Function,
    last_action_ref: &JsValue,
) -> Result<(), JsValue> {
    let dependency = load.clone();
    let load = load.clone();
    let action = last_action_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if available {
            Reflect::set(
                &action,
                &JsValue::from_str("current"),
                &JsValue::from_str("load"),
            )?;
            load.call0(&JsValue::UNDEFINED)?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(available), &dependency.into()),
    )
}

fn install_outside_effect(
    react: &JsValue,
    open: bool,
    root_ref: &JsValue,
    set_open: &Function,
) -> Result<(), JsValue> {
    let root = root_ref.clone();
    let close = set_open.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open {
            return Ok(JsValue::UNDEFINED);
        }
        let document = required(&js_sys::global(), "document", "global")?;
        let listener_root = root.clone();
        let listener_close = close.clone();
        let listener = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let target = Reflect::get(&event, &JsValue::from_str("target"))?;
            let current = Reflect::get(&listener_root, &JsValue::from_str("current"))?;
            let contained = current
                .dyn_ref::<web_sys::Node>()
                .zip(target.dyn_ref::<web_sys::Node>())
                .is_some_and(|(root, target)| root.contains(Some(target)));
            if !contained {
                listener_close.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        call_method(
            &document,
            "addEventListener",
            &[JsValue::from_str("mousedown"), listener.clone()],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &document,
                "removeEventListener",
                &[JsValue::from_str("mousedown"), listener.clone()],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of1(&JsValue::from_bool(open)),
    )
}

fn render_trigger(
    context: &RenderContext,
    locked: bool,
    open: bool,
    model_label: &str,
    trigger_label: &str,
    aria: JsValue,
) -> Result<JsValue, JsValue> {
    let mut children = vec![tag(
        &context.modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-model-triggerLabel"),
        )])?),
        &[JsValue::from_str(model_label)],
    )?];
    if let Some(effort) = &context.effort_label {
        children.push(tag(
            &context.modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-model-triggerEffort"),
            )])?),
            &[JsValue::from_str(effort)],
        )?);
    }
    children.push(react_component(
        &context.modules.react,
        &context.modules.chevron_down,
        Some(&object(&[(
            "className",
            JsValue::from_str(if open {
                "seekdeep-model-chevron seekdeep-model-chevronOpen"
            } else {
                "seekdeep-model-chevron"
            }),
        )])?),
        &[],
    )?);
    let click_context = context.clone();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if open {
            close(&click_context, false)
        } else {
            click_context
                .set_pane
                .call1(&JsValue::UNDEFINED, &JsValue::from_str("root"))?;
            click_context
                .set_open
                .call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            reload(&click_context)
        }
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    tag(
        &context.modules.react,
        "button",
        Some(&object(&[
            ("ref", context.trigger_ref.clone()),
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str("seekdeep-model-trigger")),
            ("aria-label", aria),
            ("aria-haspopup", JsValue::from_str("menu")),
            ("aria-expanded", JsValue::from_bool(open)),
            (
                "aria-controls",
                if open {
                    JsValue::from_str(&format!("{}-menu", context.id))
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("title", JsValue::from_str(trigger_label)),
            ("disabled", JsValue::from_bool(locked)),
            ("onClick", click.into_js_value()),
        ])?),
        &children,
    )
}

fn render_menu(context: &RenderContext, pane: &str, model_label: &str) -> Result<JsValue, JsValue> {
    let children = match pane {
        "root" => render_root_pane(context, model_label)?,
        "model" => render_model_pane(context)?,
        "effort" => render_effort_pane(context)?,
        _ => Vec::new(),
    };
    tag(
        &context.modules.react,
        "div",
        Some(&object(&[
            ("id", JsValue::from_str(&format!("{}-menu", context.id))),
            ("className", JsValue::from_str("seekdeep-model-menu")),
            ("role", JsValue::from_str("menu")),
            ("aria-label", translated(&context.translate, "menu.aria")?),
            (
                "aria-busy",
                JsValue::from_bool(
                    context.state.status == ModelDirectoryStatus::Loading || context.busy,
                ),
            ),
        ])?),
        &children,
    )
}

fn render_root_pane(context: &RenderContext, model_label: &str) -> Result<Vec<JsValue>, JsValue> {
    let mut rows = vec![render_cell(context, "menu.model", model_label, "model")?];
    if context.reasoning.is_some() {
        rows.push(render_cell(
            context,
            "menu.effort",
            context.effort_label.as_deref().unwrap_or_default(),
            "effort",
        )?);
    }
    Ok(rows)
}

fn render_cell(
    context: &RenderContext,
    label_key: &str,
    value: &str,
    pane: &str,
) -> Result<JsValue, JsValue> {
    let setter = context.set_pane.clone();
    let pane = pane.to_owned();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        setter.call1(&JsValue::UNDEFINED, &JsValue::from_str(&pane))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    tag(
        &context.modules.react,
        "button",
        Some(&object(&[
            ("ref", item_ref(context)?),
            ("type", JsValue::from_str("button")),
            ("role", JsValue::from_str("menuitem")),
            ("className", JsValue::from_str("seekdeep-model-cell")),
            ("onClick", click.into_js_value()),
        ])?),
        &[
            tag(
                &context.modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-model-cellLabel"),
                )])?),
                &[translated(&context.translate, label_key)?],
            )?,
            tag(
                &context.modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-model-cellValue"),
                )])?),
                &[JsValue::from_str(value)],
            )?,
            react_component(
                &context.modules.react,
                &context.modules.chevron_right,
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-model-cellChevron"),
                )])?),
                &[],
            )?,
        ],
    )
}

fn render_model_pane(context: &RenderContext) -> Result<Vec<JsValue>, JsValue> {
    let mut rows = Vec::new();
    if context.state.status == ModelDirectoryStatus::Loading {
        rows.push(message_row(
            context,
            "seekdeep-model-status",
            "status.loading",
        )?);
    }
    if context.state.error.is_some() && last_action(context)? == "load" {
        rows.push(error_row(
            context,
            context.state.error.as_deref().unwrap_or_default(),
            "retry",
        )?);
    }
    for failure in &context.state.failures {
        let message = translated_values(
            &context.translate,
            "warning.groupLoad",
            &[
                ("name", JsValue::from_str(&failure.name)),
                ("message", JsValue::from_str(&failure.message)),
            ],
        )?;
        rows.push(retry_row(
            context,
            "seekdeep-model-warning",
            message,
            "retry",
        )?);
    }
    let mut groups = Vec::new();
    for group in &context.state.groups {
        let mut models = Vec::new();
        for model in &group.models {
            models.push(render_model_option(context, group, model)?);
        }
        groups.push(tag(
            &context.modules.react,
            "section",
            Some(&object(&[
                ("key", JsValue::from_str(group.id.as_str())),
                ("role", JsValue::from_str("group")),
                (
                    "aria-labelledby",
                    JsValue::from_str(&format!("{}-{}", context.id, group.id.as_str())),
                ),
                ("className", JsValue::from_str("seekdeep-model-group")),
            ])?),
            &[
                tag(
                    &context.modules.react,
                    "div",
                    Some(&object(&[
                        ("className", JsValue::from_str("seekdeep-model-groupTitle")),
                        (
                            "id",
                            JsValue::from_str(&format!("{}-{}", context.id, group.id.as_str())),
                        ),
                    ])?),
                    &[JsValue::from_str(&group.name)],
                )?,
                fragment(&context.modules.react, &models)?,
            ],
        )?);
    }
    rows.push(tag(
        &context.modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-model-groups scrollable"),
        )])?),
        &groups,
    )?);
    if context.state.status == ModelDirectoryStatus::Ready && context.choices.is_empty() {
        rows.push(message_row(
            context,
            "seekdeep-model-empty",
            "empty.models",
        )?);
    }
    Ok(rows)
}

fn render_model_option(
    context: &RenderContext,
    group: &ModelProviderGroup,
    model: &ModelEntry,
) -> Result<JsValue, JsValue> {
    let selected = context
        .state
        .current
        .as_ref()
        .is_some_and(|current| current.provider == group.id && current.model == model.id);
    let selection = ModelSelection {
        provider: group.id.clone(),
        model: model.id.clone(),
        reasoning_effort: None,
    };
    let choose_context = context.clone();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        choose(&choose_context, &selection)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let mut copy = vec![tag(
        &context.modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-model-modelName"),
        )])?),
        &[JsValue::from_str(&model.name)],
    )?];
    if let Some(description) = &model.description {
        copy.push(tag(
            &context.modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-model-description"),
            )])?),
            &[JsValue::from_str(description)],
        )?);
    }
    option_row(
        context,
        model.id.as_str(),
        selected,
        context.busy,
        Some(&model.name),
        click.into_js_value(),
        &copy,
    )
}

fn render_effort_pane(context: &RenderContext) -> Result<Vec<JsValue>, JsValue> {
    let mut rows = Vec::new();
    if context.state.error.is_some() && last_action(context)? == "load" {
        rows.push(error_row(
            context,
            context.state.error.as_deref().unwrap_or_default(),
            "action.reload",
        )?);
    }
    let efforts = effort_choices(context)?;
    if efforts.is_empty() {
        rows.push(message_row(
            context,
            "seekdeep-model-empty",
            "empty.efforts",
        )?);
        return Ok(rows);
    }
    for (key, effort, label, description) in efforts {
        let selected = context.effective_effort.as_deref() == effort.as_deref();
        let choose_context = context.clone();
        let choose_effort = effort.clone();
        let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            choose_effort_value(&choose_context, choose_effort.clone())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let mut copy = vec![tag(
            &context.modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-model-modelName"),
            )])?),
            &[JsValue::from_str(&label)],
        )?];
        if let Some(description) = description {
            copy.push(tag(
                &context.modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-model-description"),
                )])?),
                &[JsValue::from_str(&description)],
            )?);
        }
        rows.push(option_row(
            context,
            &key,
            selected,
            context.busy,
            None,
            click.into_js_value(),
            &copy,
        )?);
    }
    Ok(rows)
}

type EffortRow = (String, Option<String>, String, Option<String>);

fn effort_choices(context: &RenderContext) -> Result<Vec<EffortRow>, JsValue> {
    let Some(reasoning) = &context.reasoning else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    if reasoning.default_effort.is_none() {
        rows.push((
            "provider-default".to_owned(),
            None,
            translated_string(&context.translate, "effort.providerDefault")?,
            None,
        ));
    }
    rows.extend(reasoning.efforts.iter().map(|effort| {
        (
            format!("effort:{}", effort.id.as_str()),
            Some(effort.id.as_str().to_owned()),
            effort.name.clone(),
            effort.description.clone(),
        )
    }));
    Ok(rows)
}

fn option_row(
    context: &RenderContext,
    key: &str,
    selected: bool,
    disabled: bool,
    title: Option<&str>,
    click: JsValue,
    copy: &[JsValue],
) -> Result<JsValue, JsValue> {
    let check = if selected {
        react_component(&context.modules.react, &context.modules.check, None, &[])?
    } else {
        JsValue::NULL
    };
    tag(
        &context.modules.react,
        "button",
        Some(&object(&[
            ("ref", item_ref(context)?),
            ("type", JsValue::from_str("button")),
            ("role", JsValue::from_str("menuitemradio")),
            ("aria-checked", JsValue::from_bool(selected)),
            (
                "className",
                JsValue::from_str(if selected {
                    "seekdeep-model-option seekdeep-model-selected"
                } else {
                    "seekdeep-model-option"
                }),
            ),
            ("key", JsValue::from_str(key)),
            ("disabled", JsValue::from_bool(disabled)),
            ("title", title.map_or(JsValue::UNDEFINED, JsValue::from_str)),
            ("onClick", click),
        ])?),
        &[
            tag(
                &context.modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-model-optionCopy"),
                )])?),
                copy,
            )?,
            tag(
                &context.modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-model-check"),
                )])?),
                &[check],
            )?,
        ],
    )
}

fn error_row(context: &RenderContext, message: &str, retry_key: &str) -> Result<JsValue, JsValue> {
    let message = translated_values(
        &context.translate,
        "error.action",
        &[("message", JsValue::from_str(message))],
    )?;
    retry_row(context, "seekdeep-model-error", message, retry_key)
}

fn retry_row(
    context: &RenderContext,
    class_name: &str,
    message: JsValue,
    retry_key: &str,
) -> Result<JsValue, JsValue> {
    let retry_context = context.clone();
    let retry = Closure::wrap(
        Box::new(move || -> Result<(), JsValue> { reload(&retry_context) })
            as Box<dyn FnMut() -> Result<(), JsValue>>,
    );
    tag(
        &context.modules.react,
        "div",
        Some(&object(&[("className", JsValue::from_str(class_name))])?),
        &[
            tag(&context.modules.react, "span", None, &[message])?,
            tag(
                &context.modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    ("className", JsValue::from_str("seekdeep-model-retry")),
                    ("onClick", retry.into_js_value()),
                ])?),
                &[translated(&context.translate, retry_key)?],
            )?,
        ],
    )
}

fn message_row(context: &RenderContext, class_name: &str, key: &str) -> Result<JsValue, JsValue> {
    tag(
        &context.modules.react,
        "div",
        Some(&object(&[("className", JsValue::from_str(class_name))])?),
        &[translated(&context.translate, key)?],
    )
}

fn reload(context: &RenderContext) -> Result<(), JsValue> {
    Reflect::set(
        &context.last_action_ref,
        &JsValue::from_str("current"),
        &JsValue::from_str("load"),
    )?;
    context.load.call0(&JsValue::UNDEFINED)?;
    Ok(())
}

fn choose(context: &RenderContext, selection: &ModelSelection) -> Result<(), JsValue> {
    if context.state.current.as_ref().is_some_and(|current| {
        current.provider == selection.provider && current.model == selection.model
    }) {
        return close(context, true);
    }
    Reflect::set(
        &context.last_action_ref,
        &JsValue::from_str("current"),
        &JsValue::from_str("select"),
    )?;
    submit(context.clone(), selection)
}

fn choose_effort_value(context: &RenderContext, effort: Option<String>) -> Result<(), JsValue> {
    let Some(current) = &context.state.current else {
        return Ok(());
    };
    if context.effective_effort == effort {
        return close(context, true);
    }
    let selection = ModelSelection {
        provider: current.provider.clone(),
        model: current.model.clone(),
        reasoning_effort: effort.map(crate::ReasoningEffortId::new),
    };
    Reflect::set(
        &context.last_action_ref,
        &JsValue::from_str("current"),
        &JsValue::from_str("select"),
    )?;
    submit(context.clone(), &selection)
}

fn submit(context: RenderContext, selection: &ModelSelection) -> Result<(), JsValue> {
    let value = serde_wasm_bindgen::to_value(&selection)
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    let returned = context.select.call1(&JsValue::UNDEFINED, &value)?;
    spawn_local(async move {
        let accepted = JsFuture::from(Promise::resolve(&returned))
            .await
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let _ = settle(&context, accepted);
    });
    Ok(())
}

fn settle(context: &RenderContext, accepted: bool) -> Result<(), JsValue> {
    if accepted {
        let root = Reflect::get(&context.root_ref, &JsValue::from_str("current"))?;
        if !root.is_null() {
            close(context, true)?;
        }
        return Ok(());
    }
    let snapshot = call_method(&context.store, "getSnapshot", &[])?;
    let Some(message) = optional(&snapshot, "error")?.and_then(|value| value.as_string()) else {
        return Ok(());
    };
    let seq = Reflect::get(&context.toast_seq_ref, &JsValue::from_str("current"))?
        .as_f64()
        .unwrap_or(0.0)
        + 1.0;
    Reflect::set(
        &context.toast_seq_ref,
        &JsValue::from_str("current"),
        &JsValue::from_f64(seq),
    )?;
    let text = translated_values(
        &context.translate,
        "error.action",
        &[("message", JsValue::from_str(&message))],
    )?;
    context.set_toast.call1(
        &JsValue::UNDEFINED,
        &object(&[("seq", JsValue::from_f64(seq)), ("text", text)])?.into(),
    )?;
    Ok(())
}

fn close(context: &RenderContext, restore_focus: bool) -> Result<(), JsValue> {
    context
        .set_open
        .call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
    context
        .set_pane
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("root"))?;
    if restore_focus {
        let trigger_ref = context.trigger_ref.clone();
        queue_microtask(move || {
            let trigger = Reflect::get(&trigger_ref, &JsValue::from_str("current"))?;
            if !trigger.is_null() {
                call_method(&trigger, "focus", &[])?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn on_root_key(context: &RenderContext, pane: &str, event: &JsValue) -> Result<(), JsValue> {
    let key = required_string(event, "key", "keyboard event")?;
    if key == "Escape" && context.open {
        call_method(event, "preventDefault", &[])?;
        if pane == "root" {
            close(context, true)?;
        } else {
            context
                .set_pane
                .call1(&JsValue::UNDEFINED, &JsValue::from_str("root"))?;
        }
    } else if context.open && matches!(key.as_str(), "ArrowDown" | "ArrowUp") {
        call_method(event, "preventDefault", &[])?;
        move_focus(context, if key == "ArrowDown" { 1 } else { -1 })?;
    }
    Ok(())
}

fn on_blur(context: &RenderContext, event: &JsValue) -> Result<(), JsValue> {
    let related = Reflect::get(event, &JsValue::from_str("relatedTarget"))?;
    let root = Reflect::get(&context.root_ref, &JsValue::from_str("current"))?;
    let contained = root
        .dyn_ref::<web_sys::Node>()
        .zip(related.dyn_ref::<web_sys::Node>())
        .is_some_and(|(root, related)| root.contains(Some(related)));
    if !contained {
        close(context, false)?;
    }
    Ok(())
}

fn move_focus(context: &RenderContext, offset: isize) -> Result<(), JsValue> {
    let refs = Array::from(&Reflect::get(
        &context.item_refs,
        &JsValue::from_str("current"),
    )?);
    let items = refs
        .iter()
        .filter(|item| !item.is_null())
        .collect::<Vec<_>>();
    if items.is_empty() {
        return Ok(());
    }
    let document = required(&js_sys::global(), "document", "global")?;
    let active = Reflect::get(&document, &JsValue::from_str("activeElement"))?;
    let at = items
        .iter()
        .position(|item| Object::is(item, &active))
        .unwrap_or(0);
    let len = isize::try_from(items.len()).unwrap_or(isize::MAX);
    let next = (isize::try_from(at).unwrap_or(0) + offset).rem_euclid(len);
    call_method(&items[usize::try_from(next).unwrap_or(0)], "focus", &[])?;
    Ok(())
}

fn item_ref(context: &RenderContext) -> Result<JsValue, JsValue> {
    let refs = Reflect::get(&context.item_refs, &JsValue::from_str("current"))?;
    let index = context.item_index.get();
    context.item_index.set(index.saturating_add(1));
    let callback_refs = refs;
    let callback = Closure::wrap(Box::new(move |node: JsValue| {
        let _ = Reflect::set(&callback_refs, &JsValue::from_f64(f64::from(index)), &node);
    }) as Box<dyn FnMut(JsValue)>);
    Ok(callback.into_js_value())
}

fn last_action(context: &RenderContext) -> Result<String, JsValue> {
    Reflect::get(&context.last_action_ref, &JsValue::from_str("current"))?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("model last action must be a string").into())
}

fn render_toast(context: &RenderContext, toast: &JsValue) -> Result<JsValue, JsValue> {
    let clear = context.set_toast.clone();
    let done = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        clear.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let icon = react_component(&context.modules.react, &context.modules.warning, None, &[])?;
    let root = Reflect::get(&context.root_ref, &JsValue::from_str("current"))?;
    let anchor = if root.is_null() {
        JsValue::NULL
    } else {
        call_method(
            &root,
            "closest",
            &[JsValue::from_str("[data-composer-card]")],
        )?
    };
    react_component(
        &context.modules.react,
        &context.modules.toast,
        Some(&object(&[
            ("key", required(toast, "seq", "model toast")?),
            ("text", required(toast, "text", "model toast")?),
            ("icon", icon),
            ("anchor", anchor),
            ("onDone", done.into_js_value()),
        ])?),
        &[],
    )
}

fn queue_microtask(task: impl FnMut() -> Result<(), JsValue> + 'static) -> Result<(), JsValue> {
    let task = Closure::wrap(Box::new(task) as Box<dyn FnMut() -> Result<(), JsValue>>);
    call_method(&js_sys::global(), "queueMicrotask", &[task.into_js_value()])?;
    Ok(())
}

fn translated_string(translate: &Function, key: &str) -> Result<String, JsValue> {
    translated(translate, key)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{key} must translate to a string")).into())
}
