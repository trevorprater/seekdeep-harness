//! Browser background-job action, timer, dismissal, and Client plugin.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    JOB_LIST_STYLES, JOB_LOCALES, JOB_NS, JobCount, JobDotState, JobDuration, JobView, dot_state,
    format_duration, is_live, job_count, ordered, status_key,
};

const INJECT: &[&str] = &["sessions", "slots", "locale"];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    chevron_down: JsValue,
    state_dot: JsValue,
    empty_jobs: JsValue,
}

/// Configures React, UI primitives, and the compiled stylesheet.
///
/// # Errors
///
/// Returns missing primitive or DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiJobs)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_jobs(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            chevron_down: required(&primitives, "IconChevronDownOutline14", "UI primitives")?,
            state_dot: required(&primitives, "StateDot", "UI primitives")?,
            empty_jobs: Array::new().into(),
        });
        Ok::<_, JsValue>(())
    })?;
    inject_styles()
}

/// Applies the background-job browser plugin.
///
/// # Errors
///
/// Returns missing Session, Slot, locale, registration, or component failures.
#[wasm_bindgen(js_name = applyClientUiJobs)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_jobs(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    required(&ctx, "sessions", "Client Context")?;
    let slots = required(&ctx, "slots", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    own_locale_dictionaries(&ctx, &locale)?;

    let component = job_list_action_component(&modules);
    let registration_slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let options = object(&[
            (
                "name",
                JsValue::from_str("conversation.session.header.actions"),
            ),
            ("id", JsValue::from_str("job-list")),
            ("order", JsValue::from_f64(20.0)),
            ("locale", JsValue::from_str(JOB_NS)),
        ])?;
        call_method(
            &registration_slots,
            "register",
            &[options.into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.session.header.actions"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = jobsInject)]
pub fn jobs_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled `JobListAction` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = jobListActionComponent)]
pub fn exported_job_list_action_component() -> Result<JsValue, JsValue> {
    Ok(job_list_action_component(&configured_modules()?))
}

fn job_list_action_component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_job_list_action(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_job_list_action(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let session_id = required_string(props, "sessionId", "JobListAction")?;
    let use_sessions = required_function(props, "useSessions", "JobListAction")?;
    let translate = required_function(props, "t", "JobListAction")?;
    let selector_empty = modules.empty_jobs.clone();
    let selector_session = session_id;
    let selector = Closure::wrap(Box::new(move |state: JsValue| -> Result<JsValue, JsValue> {
        let jobs_by_session = required(&state, "jobsBySession", "Session list state")?;
        let jobs = Reflect::get(&jobs_by_session, &JsValue::from_str(&selector_session))?;
        Ok(if jobs.is_undefined() {
            selector_empty.clone()
        } else {
            jobs
        })
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let jobs_value = use_sessions.call1(&JsValue::UNDEFINED, &selector.into_js_value())?;
    let jobs = serde_wasm_bindgen::from_value::<Vec<JobView>>(jobs_value)
        .map_err(js_error_from_display)?;

    let (open, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let clock = Closure::wrap(Box::new(js_sys::Date::now) as Box<dyn FnMut() -> f64>);
    let (now, set_now) = use_state(&modules.react, &clock.into_js_value())?;
    let root_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let trigger_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let open = open.as_bool().unwrap_or(false);
    let now = now
        .as_f64()
        .ok_or_else(|| js_sys::Error::new("JobListAction clock state must be a number"))?;
    let live_count = jobs.iter().filter(|job| is_live(job)).count();

    install_outside_effect(&modules.react, open, &root_ref, &set_open)?;
    install_timer_effect(&modules.react, open, live_count, &set_now)?;
    install_empty_effect(&modules.react, jobs.len(), open, &set_open)?;

    if jobs.is_empty() {
        return Ok(JsValue::NULL);
    }

    let count = job_count(&jobs);
    let count_label = render_count(count, &translate)?;
    let trigger = render_trigger(
        modules,
        open,
        live_count,
        &count_label,
        &trigger_ref,
        &set_open,
        &set_now,
    )?;
    let mut children = vec![trigger];
    if open {
        children.push(render_menu(modules, &jobs, now, &translate)?);
    }
    let key_open = open;
    let close_open = set_open;
    let focus_ref = trigger_ref;
    let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if Reflect::get(&event, &JsValue::from_str("key"))?
            .as_string()
            .as_deref()
            != Some("Escape")
            || !key_open
        {
            return Ok(());
        }
        call_method(&event, "preventDefault", &[])?;
        close_open.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        let trigger = Reflect::get(&focus_ref, &JsValue::from_str("current"))?;
        if !trigger.is_null() {
            call_method(&trigger, "focus", &[])?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("ref", root_ref),
            ("className", JsValue::from_str("seekdeep-jobs-root")),
            ("onKeyDown", keydown.into_js_value()),
        ])?),
        &children,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_trigger(
    modules: &BrowserModules,
    open: bool,
    live_count: usize,
    count_label: &JsValue,
    trigger_ref: &JsValue,
    set_open: &Function,
    set_now: &Function,
) -> Result<JsValue, JsValue> {
    let mut children = Vec::new();
    if live_count > 0 {
        children.push(component(
            &modules.react,
            &modules.state_dot,
            Some(&object(&[
                ("state", JsValue::from_str("ongoing")),
                ("className", JsValue::from_str("seekdeep-jobs-triggerDot")),
            ])?),
            &[],
        )?);
    }
    children.push(tag(
        &modules.react,
        "span",
        Some(&class("seekdeep-jobs-count")?),
        std::slice::from_ref(count_label),
    )?);
    children.push(component(
        &modules.react,
        &modules.chevron_down,
        Some(&object(&[(
            "className",
            if open {
                JsValue::from_str("seekdeep-jobs-triggerOpen")
            } else {
                JsValue::UNDEFINED
            },
        )])?),
        &[],
    )?);
    let toggle_open = set_open.clone();
    let sample_now = set_now.clone();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        sample_now.call1(&JsValue::UNDEFINED, &JsValue::from_f64(js_sys::Date::now()))?;
        let toggle = Closure::wrap(Box::new(|value: bool| !value) as Box<dyn FnMut(bool) -> bool>);
        toggle_open.call1(&JsValue::UNDEFINED, &toggle.into_js_value())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("ref", trigger_ref.clone()),
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str("seekdeep-jobs-trigger")),
            ("aria-expanded", JsValue::from_bool(open)),
            ("aria-label", count_label.clone()),
            ("onClick", click.into_js_value()),
        ])?),
        &children,
    )
}

fn render_menu(
    modules: &BrowserModules,
    jobs: &[JobView],
    now: f64,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let mut rows = Vec::new();
    for job in ordered(jobs) {
        rows.push(render_job_row(modules, &job, now, translate)?);
    }
    tag(
        &modules.react,
        "ul",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-jobs-menu")),
            ("aria-label", translated(translate, "list.aria")?),
        ])?),
        &rows,
    )
}

fn render_job_row(
    modules: &BrowserModules,
    job: &JobView,
    now: f64,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let live = is_live(job);
    let elapsed = if live {
        f64_as_i128(now) - i128::from(job.started_at)
    } else {
        i128::from(job.finished_at.unwrap_or(job.started_at)) - i128::from(job.started_at)
    };
    let duration = render_duration(format_duration(elapsed), translate)?;
    let status = translated(translate, &format!("status.{}", status_key(job.status)))?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("job status must translate to a string"))?;
    let detail = job.detail.as_deref().unwrap_or(&status);
    let dot = component(
        &modules.react,
        &modules.state_dot,
        Some(&object(&[
            (
                "state",
                JsValue::from_str(dot_state_name(dot_state(job.status))),
            ),
            ("className", JsValue::from_str("seekdeep-jobs-rowDot")),
        ])?),
        &[],
    )?;
    let kind = cell(&modules.react, "seekdeep-jobs-kind", &job.kind, None)?;
    let label = cell(
        &modules.react,
        "seekdeep-jobs-label",
        &job.label,
        Some(&job.label),
    )?;
    let status = cell(&modules.react, "seekdeep-jobs-status", detail, Some(detail))?;
    let title = translated_value(
        translate,
        if live {
            "duration.title.live"
        } else {
            "duration.title.done"
        },
        "duration",
        JsValue::from_str(&duration),
    )?;
    let duration_cell = tag(
        &modules.react,
        "span",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-jobs-duration")),
            ("title", title),
        ])?),
        &[JsValue::from_str(&duration)],
    )?;
    tag(
        &modules.react,
        "li",
        Some(&object(&[
            ("key", JsValue::from_str(&job.id)),
            (
                "className",
                JsValue::from_str(if live {
                    "seekdeep-jobs-row"
                } else {
                    "seekdeep-jobs-row seekdeep-jobs-rowSettled"
                }),
            ),
        ])?),
        &[dot, kind, label, status, duration_cell],
    )
}

fn cell(
    react: &JsValue,
    class_name: &str,
    text: &str,
    title: Option<&str>,
) -> Result<JsValue, JsValue> {
    tag(
        react,
        "span",
        Some(&object(&[
            ("className", JsValue::from_str(class_name)),
            ("title", title.map_or(JsValue::UNDEFINED, JsValue::from_str)),
        ])?),
        &[JsValue::from_str(text)],
    )
}

fn render_count(count: JobCount, translate: &Function) -> Result<JsValue, JsValue> {
    let key = match (count.live, count.count == 1) {
        (true, true) => "count.live.one",
        (true, false) => "count.live.other",
        (false, true) => "count.idle.one",
        (false, false) => "count.idle.other",
    };
    translated_value(
        translate,
        key,
        "count",
        JsValue::from_f64(usize_as_f64(count.count)),
    )
}

fn render_duration(duration: JobDuration, translate: &Function) -> Result<String, JsValue> {
    let value = match duration {
        JobDuration::Seconds(seconds) => translated_value(
            translate,
            "duration.seconds",
            "seconds",
            JsValue::from_f64(u64_as_f64(seconds)),
        )?,
        JobDuration::Minutes { minutes, seconds } => translated_values(
            translate,
            "duration.minutes",
            &[
                ("minutes", JsValue::from_f64(u64_as_f64(minutes))),
                ("seconds", JsValue::from_f64(u64_as_f64(seconds))),
            ],
        )?,
        JobDuration::Hours { hours, minutes } => translated_values(
            translate,
            "duration.hours",
            &[
                ("hours", JsValue::from_f64(u64_as_f64(hours))),
                ("minutes", JsValue::from_f64(u64_as_f64(minutes))),
            ],
        )?,
    };
    value
        .as_string()
        .ok_or_else(|| js_sys::Error::new("job duration must translate to a string").into())
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
            if !target.is_instance_of::<web_sys::Node>() {
                return Ok(());
            }
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
            &[JsValue::from_str("pointerdown"), listener.clone()],
        )?;
        let cleanup_document = document;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &cleanup_document,
                "removeEventListener",
                &[JsValue::from_str("pointerdown"), listener.clone()],
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

fn install_timer_effect(
    react: &JsValue,
    open: bool,
    live_count: usize,
    set_now: &Function,
) -> Result<(), JsValue> {
    let setter = set_now.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open || live_count == 0 {
            return Ok(JsValue::UNDEFINED);
        }
        setter.call1(&JsValue::UNDEFINED, &JsValue::from_f64(js_sys::Date::now()))?;
        let tick_setter = setter.clone();
        let tick = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            tick_setter.call1(&JsValue::UNDEFINED, &JsValue::from_f64(js_sys::Date::now()))?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let global = js_sys::global();
        let timer = call_method(
            &global,
            "setInterval",
            &[tick.clone(), JsValue::from_f64(1_000.0)],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &js_sys::global(),
                "clearInterval",
                std::slice::from_ref(&timer),
            );
            drop(tick.clone());
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(
            &JsValue::from_bool(open),
            &JsValue::from_f64(usize_as_f64(live_count)),
        ),
    )
}

fn install_empty_effect(
    react: &JsValue,
    jobs_len: usize,
    open: bool,
    set_open: &Function,
) -> Result<(), JsValue> {
    let setter = set_open.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if jobs_len == 0 && open {
            setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(
            &JsValue::from_f64(usize_as_f64(jobs_len)),
            &JsValue::from_bool(open),
        ),
    )
}

const fn dot_state_name(state: JobDotState) -> &'static str {
    match state {
        JobDotState::Ongoing => "ongoing",
        JobDotState::Warning => "warning",
        JobDotState::Done => "done",
        JobDotState::Error => "error",
    }
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, zh_value, en_value) in JOB_LOCALES {
        set(&zh, key, &JsValue::from_str(zh_value))?;
        set(&en, key, &JsValue::from_str(en_value))?;
    }
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(JOB_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-job: dictionaries"),
        ],
    )?;
    Ok(())
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-jobs";
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let selector = format!(
        "style[data-plugin={}]",
        serde_json::to_string(PACKAGE).unwrap()
    );
    if !call_method(&document, "querySelector", &[JsValue::from_str(&selector)])?.is_null() {
        return Ok(());
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin"), JsValue::from_str(PACKAGE)],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(JOB_LIST_STYLES),
    )?;
    let head = required(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-jobs is not configured").into())
    })
}

fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn translated_value(
    translate: &Function,
    key: &str,
    field: &str,
    value: JsValue,
) -> Result<JsValue, JsValue> {
    translated_values(translate, key, &[(field, value)])
}

fn translated_values(
    translate: &Function,
    key: &str,
    values: &[(&str, JsValue)],
) -> Result<JsValue, JsValue> {
    translate.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str(key),
        &object(values)?.into(),
    )
}

fn tag(
    react: &JsValue,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, &JsValue::from_str(name), props, children)
}

fn component(
    react: &JsValue,
    component: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, component, props, children)
}

fn element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let args = Array::new();
    args.push(kind);
    args.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        args.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &args)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn use_effect(react: &JsValue, effect: &JsValue, dependencies: &Array) -> Result<(), JsValue> {
    required_function(react, "useEffect", "React")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

fn class(value: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(value))])
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

fn set(value: &Object, key: &str, entry: &JsValue) -> Result<(), JsValue> {
    Reflect::set(value, &JsValue::from_str(key), entry).map(|_| ())
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let entry = Reflect::get(value, &JsValue::from_str(key))?;
    if entry.is_null() || entry.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into())
    } else {
        Ok(entry)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a string")).into())
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into()
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn f64_as_i128(value: f64) -> i128 {
    #[allow(clippy::cast_possible_truncation)]
    {
        value as i128
    }
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn js_error_from_display(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
