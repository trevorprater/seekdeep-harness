//! Browser plugin, produced-file row, and mention resolver.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    DELIVERABLES_EN, DELIVERABLES_NS, DELIVERABLES_ZH, DeliverablesTurnData, PRODUCED_FILES_STYLES,
    SHOWN_LIMIT, basename, deliverables_definition, fit_produced_files, produced_file_mention,
    produced_for_closing, select_produced_files,
};

const INJECT: &[&str] = &["slots", "locale", "conversationEvents", "connection"];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
}

/// Configures React and the compiled stylesheet.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiDeliverables)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_deliverables(react: JsValue) -> Result<(), JsValue> {
    MODULES.with(|modules| *modules.borrow_mut() = Some(BrowserModules { react }));
    inject_styles()
}

/// Applies the browser deliverables plugin.
///
/// # Errors
///
/// Returns missing service, native Definition, locale, Slot, service-provision, or component
/// failures.
#[wasm_bindgen(js_name = applyClientUiDeliverables)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_deliverables(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required(&ctx, "slots", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    let events = required(&ctx, "conversationEvents", "Client Context")?;
    let connection = required(&ctx, "connection", "Client Context")?;
    call_method(
        &events,
        "register",
        &[
            seekdeep_client_runtime::native_conversation_node_definition_to_js(
                deliverables_definition(),
            )?,
        ],
    )?;
    own_locale_dictionaries(&ctx, &locale)?;
    let translate = call_method(&locale, "bind", &[JsValue::from_str(DELIVERABLES_NS)])?
        .dyn_into::<Function>()?;

    let component = produced_files_component(&modules);
    let registration_slots = slots.clone();
    let installer_connection = connection;
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let select = Closure::wrap(Box::new(move |owner: JsValue| -> Result<JsValue, JsValue> {
            select_paths(&owner).map(|paths| paths.unwrap_or(JsValue::NULL))
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        let inject_connection = installer_connection.clone();
        let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            object(&[
                (
                    "isLoopback",
                    required(&inject_connection, "isLoopback", "connection")?,
                ),
                (
                    "hooks",
                    object(&[(
                        "hostDescription",
                        required(&inject_connection, "hostDescription", "connection")?,
                    )])?
                    .into(),
                ),
            ])
            .map(Into::into)
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        let options = object(&[
            ("name", JsValue::from_str("conversation.chat.turnTail")),
            ("select", select.into_js_value()),
            ("locale", JsValue::from_str(DELIVERABLES_NS)),
            ("inject", inject.into_js_value()),
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
            JsValue::from_str("conversation.chat.turnTail"),
            installer.into_js_value(),
        ],
    )?;

    let mentions_translate = translate;
    let mentions = object(&[(
        "forClosing",
        Closure::wrap(Box::new(move |owner: JsValue| -> Result<JsValue, JsValue> {
            let Some(paths) = select_paths_vec(&owner)? else {
                return Ok(JsValue::UNDEFINED);
            };
            mention_resolver(&owner, paths, &mentions_translate)
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )])?;
    call_method(
        &ctx,
        "provide",
        &[JsValue::from_str("chatFileMentions"), mentions.into()],
    )?;
    Ok(())
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = deliverablesInject)]
pub fn deliverables_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the first-seen produced paths settled no later than the optional closing sequence.
///
/// # Errors
///
/// Returns when the data payload or finite closing sequence is invalid.
#[wasm_bindgen(js_name = producedForClosing)]
#[allow(clippy::needless_pass_by_value)]
pub fn exported_produced_for_closing(
    data: JsValue,
    closing_seq: Option<f64>,
) -> Result<Array, JsValue> {
    let data = if data.is_null() || data.is_undefined() {
        None
    } else {
        Some(
            serde_wasm_bindgen::from_value::<DeliverablesTurnData>(data)
                .map_err(js_error_from_display)?,
        )
    };
    let closing_seq = match closing_seq {
        None => None,
        Some(value) if value == f64::INFINITY => None,
        Some(value) => Some(
            f64_to_u64(value)
                .ok_or_else(|| js_sys::Error::new("closing sequence must be a finite u64"))?,
        ),
    };
    let paths = Array::new();
    for path in produced_for_closing(data.as_ref(), closing_seq) {
        paths.push(&JsValue::from_str(&path));
    }
    Ok(paths)
}

/// Returns the compiled `ProducedFiles` component.
///
/// # Errors
///
/// Returns before React is configured.
#[wasm_bindgen(js_name = producedFilesComponent)]
pub fn exported_produced_files_component() -> Result<JsValue, JsValue> {
    Ok(produced_files_component(&configured_modules()?))
}

fn produced_files_component(modules: &BrowserModules) -> JsValue {
    let react = modules.react.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_produced_files(&react, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_produced_files(react: &JsValue, props: &JsValue) -> Result<JsValue, JsValue> {
    let matched = required(props, "matched", "ProducedFiles")?;
    let paths = Array::from(&matched)
        .iter()
        .map(|path| {
            path.as_string()
                .ok_or_else(|| js_sys::Error::new("produced path must be a string").into())
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    let open_file = required_function(props, "openFile", "ProducedFiles")?;
    let translate = required_function(props, "t", "ProducedFiles")?;
    let is_loopback = required(props, "isLoopback", "ProducedFiles")?
        .as_bool()
        .unwrap_or(false);
    let use_host_description = required_function(props, "useHostDescription", "ProducedFiles")?;
    let selector = Closure::wrap(Box::new(move |description: JsValue| -> bool {
        !description.is_null()
            && !description.is_undefined()
            && Reflect::get(&description, &JsValue::from_str("canOpenPath"))
                .ok()
                .and_then(|value| value.as_bool())
                == Some(true)
    }) as Box<dyn FnMut(JsValue) -> bool>);
    let host_can_open = use_host_description
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())?
        .as_bool()
        .unwrap_or(false);
    let limit = paths.len().min(SHOWN_LIMIT);
    let (shown, set_shown) = use_state(react, &JsValue::from_f64(usize_as_f64(limit)))?;
    let shown = shown
        .as_f64()
        .and_then(f64_to_usize)
        .unwrap_or(limit)
        .min(limit);
    let row_ref = use_ref(react, &JsValue::NULL)?;
    let probes_ref = use_ref(react, &Array::new().into())?;
    let more_ref = use_ref(react, &JsValue::NULL)?;
    install_measurement_effect(
        react,
        &row_ref,
        &probes_ref,
        &more_ref,
        &paths,
        &matched,
        limit,
        &translate,
        &set_shown,
    )?;

    let mut visible = Vec::new();
    for path in paths.iter().take(shown) {
        visible.push(file_button(react, path, &open_file, &translate)?);
    }
    let hidden = paths.len() - shown;
    if hidden > 0 {
        visible.push(tag(
            react,
            "span",
            Some(&class("seekdeep-deliverables-more")?),
            &[more_label(&translate, hidden)?],
        )?);
    }
    let row = tag(
        react,
        "div",
        Some(&object(&[
            ("ref", row_ref),
            ("className", JsValue::from_str("seekdeep-deliverables-row")),
            ("data-produced-files-row", JsValue::from_str("")),
        ])?),
        &visible,
    )?;
    let mut root_children = vec![
        tag(
            react,
            "span",
            Some(&class("seekdeep-deliverables-label")?),
            &[translated(&translate, "produced.label")?],
        )?,
        row,
    ];
    if hidden > 0 && is_loopback && host_can_open {
        let folder_open = open_file.clone();
        let open = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            folder_open.call1(&JsValue::UNDEFINED, &JsValue::from_str("."))?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        root_children.push(tag(
            react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str("seekdeep-deliverables-showFolder"),
                ),
                ("onClick", open.into_js_value()),
            ])?),
            &[translated(&translate, "produced.showInFolder")?],
        )?);
    }
    let probes = Reflect::get(&probes_ref, &JsValue::from_str("current"))?.dyn_into::<Array>()?;
    let mut measure_children = Vec::new();
    for (index, path) in paths.iter().take(limit).enumerate() {
        let callback_probes = probes.clone();
        let callback = Closure::wrap(Box::new(move |node: JsValue| {
            callback_probes.set(u32::try_from(index).unwrap_or(u32::MAX), node);
        }) as Box<dyn FnMut(JsValue)>);
        measure_children.push(probe_button(react, path, &callback.into_js_value())?);
    }
    let more = tag(
        react,
        "span",
        Some(&object(&[
            ("ref", more_ref),
            (
                "className",
                JsValue::from_str("seekdeep-deliverables-more seekdeep-deliverables-probe"),
            ),
        ])?),
        &[],
    )?;
    measure_children.push(more);
    root_children.push(tag(
        react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-deliverables-measure"),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &measure_children,
    )?);
    tag(
        react,
        "div",
        Some(&class("seekdeep-deliverables-root")?),
        &root_children,
    )
}

fn file_button(
    react: &JsValue,
    path: &str,
    open_file: &Function,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let opener = open_file.clone();
    let path_owned = path.to_owned();
    let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        opener.call1(&JsValue::UNDEFINED, &JsValue::from_str(&path_owned))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    tag(
        react,
        "button",
        Some(&object(&[
            ("key", JsValue::from_str(path)),
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str("seekdeep-deliverables-file")),
            ("title", JsValue::from_str(path)),
            (
                "aria-label",
                translated_with(translate, "produced.open", "name", path)?,
            ),
            ("onClick", on_click.into_js_value()),
        ])?),
        &[JsValue::from_str(basename(path))],
    )
}

fn probe_button(react: &JsValue, path: &str, reference: &JsValue) -> Result<JsValue, JsValue> {
    tag(
        react,
        "button",
        Some(&object(&[
            ("key", JsValue::from_str(path)),
            ("ref", reference.clone()),
            ("type", JsValue::from_str("button")),
            ("tabIndex", JsValue::from_f64(-1.0)),
            (
                "className",
                JsValue::from_str("seekdeep-deliverables-file seekdeep-deliverables-probe"),
            ),
        ])?),
        &[JsValue::from_str(basename(path))],
    )
}

#[allow(clippy::too_many_arguments)]
fn install_measurement_effect(
    react: &JsValue,
    row_ref: &JsValue,
    probes_ref: &JsValue,
    more_ref: &JsValue,
    paths: &[String],
    paths_dependency: &JsValue,
    limit: usize,
    translate: &Function,
    set_shown: &Function,
) -> Result<(), JsValue> {
    let row_ref = row_ref.clone();
    let probes_ref = probes_ref.clone();
    let more_ref = more_ref.clone();
    let paths = paths.to_vec();
    let translate = translate.clone();
    let dependency_paths = paths_dependency.clone();
    let dependency_translate = translate.clone();
    let set_shown = set_shown.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let row = Reflect::get(&row_ref, &JsValue::from_str("current"))?;
        let more = Reflect::get(&more_ref, &JsValue::from_str("current"))?;
        if row.is_null() || more.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        let measure_row = row.clone();
        let measure_more = more.clone();
        let measure_probes = probes_ref.clone();
        let measure_paths = paths.clone();
        let measure_translate = translate.clone();
        let measure_setter = set_shown.clone();
        let measure = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let global = js_sys::global();
            let styles = call_method(
                &global,
                "getComputedStyle",
                std::slice::from_ref(&measure_row),
            )?;
            let gap = computed_gap(&styles);
            let probes = Reflect::get(&measure_probes, &JsValue::from_str("current"))?
                .dyn_into::<Array>()?;
            let mut widths = Vec::new();
            for index in 0..limit {
                let probe = probes.get(u32::try_from(index).unwrap_or(u32::MAX));
                let bounds = call_method(&probe, "getBoundingClientRect", &[])?;
                widths.push(required_number(&bounds, "width", "file probe rectangle")?);
            }
            let mut more_widths = Vec::new();
            for shown in 0..=limit {
                if measure_paths.len() == shown {
                    more_widths.push(None);
                    continue;
                }
                let label = more_label(&measure_translate, measure_paths.len() - shown)?;
                Reflect::set(&measure_more, &JsValue::from_str("textContent"), &label)?;
                let bounds = call_method(&measure_more, "getBoundingClientRect", &[])?;
                more_widths.push(Some(required_number(
                    &bounds,
                    "width",
                    "remainder probe rectangle",
                )?));
            }
            let available = required_number(&measure_row, "clientWidth", "produced row")?;
            let shown = fit_produced_files(available, gap, &widths, &more_widths);
            measure_setter.call1(&JsValue::UNDEFINED, &JsValue::from_f64(usize_as_f64(shown)))?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        measure
            .as_ref()
            .unchecked_ref::<Function>()
            .call0(&JsValue::UNDEFINED)?;
        let resize = Reflect::get(&js_sys::global(), &JsValue::from_str("ResizeObserver"))?;
        if !resize.is_function() {
            return Ok(JsValue::UNDEFINED);
        }
        let observer = Reflect::construct(
            &resize.dyn_into::<Function>()?,
            &Array::of1(&measure.into_js_value()),
        )?;
        call_method(&observer, "observe", std::slice::from_ref(&row))?;
        let probes =
            Reflect::get(&probes_ref, &JsValue::from_str("current"))?.dyn_into::<Array>()?;
        for probe in probes.iter().chain(std::iter::once(more)) {
            if !probe.is_null() {
                call_method(&observer, "observe", &[probe])?;
            }
        }
        let cleanup_observer = observer;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(&cleanup_observer, "disconnect", &[]);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_layout_effect(
        react,
        &effect.into_js_value(),
        &Array::of3(
            &JsValue::from_f64(usize_as_f64(limit)),
            &dependency_paths,
            dependency_translate.as_ref(),
        ),
    )
}

fn mention_resolver(
    owner: &JsValue,
    paths: Vec<String>,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let open_file = required_function(owner, "openFile", "turn-tail owner")?;
    let resolver = Object::new();
    let resolve_paths = paths;
    let resolve_translate = translate.clone();
    let resolve = Closure::wrap(Box::new(move |value: String| -> Result<JsValue, JsValue> {
        let Some(mention) = produced_file_mention(&resolve_paths, &value, |_| String::new()) else {
            return Ok(JsValue::UNDEFINED);
        };
        let label = translated_with(&resolve_translate, "produced.open", "name", &mention.path)?;
        let opener = open_file.clone();
        let path = mention.path.clone();
        let open = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            opener.call1(&JsValue::UNDEFINED, &JsValue::from_str(&path))?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        object(&[
            ("label", label),
            ("title", JsValue::from_str(&mention.title)),
            ("open", open.into_js_value()),
        ])
        .map(Into::into)
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(&resolver, "resolve", &resolve.into_js_value())?;
    Ok(resolver.into())
}

fn select_paths(owner: &JsValue) -> Result<Option<JsValue>, JsValue> {
    Ok(select_paths_vec(owner)?.map(|paths| {
        let values = Array::new();
        for path in paths {
            values.push(&JsValue::from_str(&path));
        }
        values.into()
    }))
}

fn select_paths_vec(owner: &JsValue) -> Result<Option<Vec<String>>, JsValue> {
    let turn = required(owner, "turn", "turn-tail owner")?;
    let data = required(&turn, "data", "Turn location")?;
    let value = call_method(&data, "get", &[JsValue::from_str("deliverables")])?;
    if value.is_undefined() {
        return Ok(None);
    }
    let data = serde_wasm_bindgen::from_value::<DeliverablesTurnData>(value)
        .map_err(js_error_from_display)?;
    let seq = required_number(owner, "seq", "turn-tail owner")?;
    let seq =
        f64_to_u64(seq).ok_or_else(|| js_sys::Error::new("turn-tail owner seq must be a u64"))?;
    Ok(select_produced_files(Some(&data), seq))
}

fn more_label(translate: &Function, count: usize) -> Result<JsValue, JsValue> {
    if count == 1 {
        translated(translate, "produced.moreOne")
    } else {
        translated_with(translate, "produced.more", "count", &count.to_string())
    }
}

fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn translated_with(
    translate: &Function,
    key: &str,
    field: &str,
    value: &str,
) -> Result<JsValue, JsValue> {
    translate.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str(key),
        &object(&[(field, JsValue::from_str(value))])?.into(),
    )
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = dictionary(DELIVERABLES_ZH)?;
    let en = dictionary(DELIVERABLES_EN)?;
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(DELIVERABLES_NS),
                dictionaries.clone().into(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-deliverables: dictionaries"),
        ],
    )?;
    Ok(())
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-deliverables";
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
        &JsValue::from_str(PRODUCED_FILES_STYLES),
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
            .ok_or_else(|| js_sys::Error::new("client-ui-deliverables is not configured").into())
    })
}

fn tag(
    react: &JsValue,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let args = Array::new();
    args.push(&JsValue::from_str(name));
    args.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        args.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &args)
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_layout_effect(
    react: &JsValue,
    effect: &JsValue,
    dependencies: &Array,
) -> Result<(), JsValue> {
    required_function(react, "useLayoutEffect", "React")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

fn dictionary(entries: [(&str, &str); 5]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, &JsValue::from_str(entry))?;
    }
    Ok(value)
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

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into()
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a number")).into())
}

fn computed_gap(styles: &JsValue) -> f64 {
    let column_gap = Reflect::get(styles, &JsValue::from_str("columnGap"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    let raw = if column_gap.is_empty() {
        Reflect::get(styles, &JsValue::from_str("gap"))
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default()
    } else {
        column_gap
    };
    let parsed = js_sys::Number::parse_float(&raw);
    if parsed.is_nan() { 0.0 } else { parsed }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn f64_to_usize(value: f64) -> Option<usize> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as usize)
}

fn f64_to_u64(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as u64)
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn js_error_from_display(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
