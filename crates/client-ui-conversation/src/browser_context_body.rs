//! Compiled context-form bodies and disclosure row.

use std::{cell::RefCell, collections::BTreeSet};

use js_sys::{Array, Function, JSON, JsString, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const BODY_CSS: &str =
    include_str!("../../../packages/client/ui-conversation/src/client/chat/ContextBody.module.css");
const ROW_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/chat/ContextInjectionRow.module.css"
);
const MAX_CHARS: u32 = 20_000;
const MAX_ENTRIES: usize = 200;

thread_local! {
    static COMPONENTS: RefCell<Option<ContextComponents>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fragment: JsValue,
    json_block: JsValue,
    disclosure_row: JsValue,
    browse_icon: JsValue,
}

#[derive(Clone)]
struct ContextComponents {
    opaque: JsValue,
    instructions: JsValue,
    catalog: JsValue,
    snapshot: JsValue,
    notice: JsValue,
    relay: JsValue,
    recall: JsValue,
    row: JsValue,
}

#[derive(Clone)]
enum ContentRun {
    Text(JsValue),
    Block(JsValue),
}

#[derive(Clone)]
struct InstructionChange {
    action: String,
    path: String,
    digest: Option<String>,
}

#[derive(Clone)]
struct CatalogEntry {
    name: String,
    description: String,
}

#[derive(Clone)]
struct SnapshotSection {
    name: String,
    text: JsValue,
}

#[derive(Clone)]
struct RecalledSession {
    label: String,
    retained: JsValue,
    omitted: JsValue,
    truncated: bool,
}

struct ResolvedBody {
    rendered: Option<&'static str>,
    summary: Option<String>,
    body: JsValue,
}

/// Configures all compiled context bodies and the disclosure row.
///
/// # Errors
///
/// Returns on missing React/primitives faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationContextBodies)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_context_bodies(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "useState"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        fragment: required_property(&react, "Fragment", "React")?,
        json_block: required_property(&ui_primitives, "JsonBlock", "ui-primitives")?,
        disclosure_row: required_property(&ui_primitives, "DisclosureRow", "ui-primitives")?,
        browse_icon: required_property(&ui_primitives, "IconBrowseOutline16", "ui-primitives")?,
        react,
    };
    inject_context_styles()?;
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules.clone()));
    let opaque_modules = modules.clone();
    let opaque = raw_component(move |props| render_opaque(&opaque_modules, props));
    let instructions_modules = modules.clone();
    let instructions =
        raw_component(move |props| render_instructions(&instructions_modules, props));
    let catalog_modules = modules.clone();
    let catalog = raw_component(move |props| render_catalog(&catalog_modules, props));
    let snapshot_modules = modules.clone();
    let snapshot = raw_component(move |props| render_snapshot(&snapshot_modules, props));
    let notice_modules = modules.clone();
    let notice = raw_component(move |props| render_notice(&notice_modules, props));
    let relay_modules = modules.clone();
    let relay = raw_component(move |props| render_relay(&relay_modules, props));
    let recall_modules = modules.clone();
    let recall = raw_component(move |props| render_recall(&recall_modules, props));
    let row_modules = modules;
    let row = raw_component(move |props| render_context_row(&row_modules, props));
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(ContextComponents {
            opaque,
            instructions,
            catalog,
            snapshot,
            notice,
            relay,
            recall,
            row,
        });
    });
    Ok(())
}

fn inject_context_styles() -> Result<(), JsValue> {
    inject_style(
        "ContextBody",
        BODY_CSS,
        &[
            (
                "catalogNotice",
                "seekdeep-conversation-contextBody-catalogNotice",
            ),
            ("entries", "seekdeep-conversation-contextBody-entries"),
            ("entry", "seekdeep-conversation-contextBody-entry"),
            (
                "entryDescription",
                "seekdeep-conversation-contextBody-entryDescription",
            ),
            ("entryName", "seekdeep-conversation-contextBody-entryName"),
            ("field", "seekdeep-conversation-contextBody-field"),
            ("fieldKey", "seekdeep-conversation-contextBody-fieldKey"),
            ("fieldValue", "seekdeep-conversation-contextBody-fieldValue"),
            ("fields", "seekdeep-conversation-contextBody-fields"),
            ("file", "seekdeep-conversation-contextBody-file"),
            ("fileAction", "seekdeep-conversation-contextBody-fileAction"),
            ("filePath", "seekdeep-conversation-contextBody-filePath"),
            ("files", "seekdeep-conversation-contextBody-files"),
            ("recall", "seekdeep-conversation-contextBody-recall"),
            (
                "recallCounts",
                "seekdeep-conversation-contextBody-recallCounts",
            ),
            (
                "recallLabel",
                "seekdeep-conversation-contextBody-recallLabel",
            ),
            ("recalls", "seekdeep-conversation-contextBody-recalls"),
            (
                "relaySender",
                "seekdeep-conversation-contextBody-relaySender",
            ),
            ("section", "seekdeep-conversation-contextBody-section"),
            (
                "sectionName",
                "seekdeep-conversation-contextBody-sectionName",
            ),
            (
                "sectionText",
                "seekdeep-conversation-contextBody-sectionText",
            ),
            ("sections", "seekdeep-conversation-contextBody-sections"),
            ("text", "seekdeep-conversation-contextBody-text"),
        ],
    )?;
    inject_style(
        "ContextInjectionRow",
        ROW_CSS,
        &[
            ("body", "seekdeep-conversation-contextRow-body"),
            ("chevron", "seekdeep-conversation-contextRow-chevron"),
            ("root", "seekdeep-conversation-contextRow-root"),
            ("sep", "seekdeep-conversation-contextRow-sep"),
            ("source", "seekdeep-conversation-contextRow-source"),
            ("summary", "seekdeep-conversation-contextRow-summary"),
        ],
    )?;
    Ok(())
}

fn raw_component<F>(renderer: F) -> JsValue
where
    F: 'static + FnMut(&JsValue) -> Result<JsValue, JsValue>,
{
    let mut renderer = renderer;
    Closure::wrap(Box::new(move |props: JsValue| renderer(&props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

macro_rules! component_getter {
    ($rust:ident, $js:literal, $field:ident) => {
        #[doc = concat!("Returns the compiled `", $js, "` component.")]
        ///
        /// # Errors
        ///
        /// Returns before configuration.
        #[wasm_bindgen(js_name = $js)]
        pub fn $rust() -> Result<JsValue, JsValue> {
            Ok(configured_components()?.$field)
        }
    };
}

component_getter!(opaque_body_component, "opaqueBodyComponent", opaque);
component_getter!(
    instructions_body_component,
    "instructionsBodyComponent",
    instructions
);
component_getter!(catalog_body_component, "catalogBodyComponent", catalog);
component_getter!(snapshot_body_component, "snapshotBodyComponent", snapshot);
component_getter!(notice_body_component, "noticeBodyComponent", notice);
component_getter!(relay_body_component, "relayBodyComponent", relay);
component_getter!(recall_body_component, "recallBodyComponent", recall);
component_getter!(
    context_injection_row_component,
    "contextInjectionRowComponent",
    row
);

/// Resolves one declared form to the body that can actually render it.
///
/// # Errors
///
/// Returns for an unknown closed-union form or malformed required props.
#[wasm_bindgen(js_name = contextBody)]
#[allow(clippy::needless_pass_by_value)]
pub fn context_body_browser(form: JsValue, props: JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let resolved = resolve_body(&modules, &form, &props)?;
    resolved_object(resolved)
}

#[allow(clippy::too_many_lines)]
fn render_context_row(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &JsValue::FALSE)?
        .dyn_into::<Array>()?;
    let open = state
        .get(0)
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("ContextInjectionRow open state must be boolean"))?;
    let set_open = state.get(1).dyn_into::<Function>()?;
    let content = required_property(props, "content", "ContextInjectionRow props")?;
    let source = Reflect::get(props, &JsValue::from_str("source"))?;
    let provenance = required_property(props, "provenance", "ContextInjectionRow props")?;
    let form = Reflect::get(props, &JsValue::from_str("form"))?;
    let translate = required_function(props, "t", "ContextInjectionRow props")?;
    let body_props = object(&[
        ("content", content),
        ("source", source),
        ("t", translate.clone().into()),
    ])?;
    let resolved = resolve_body(modules, &form, body_props.as_ref())?;
    let role = Reflect::get(&provenance, &JsValue::from_str("role"))?
        .as_string()
        .unwrap_or_default();
    let title_key = if role == "recall" {
        "message.contextRecall"
    } else {
        "message.contextInjection"
    };
    let title = translate_text(&translate, title_key, None)?;
    let label = Reflect::get(&provenance, &JsValue::from_str("label"))?;
    let collapsed = if label.is_null() {
        JsValue::UNDEFINED
    } else {
        let mut children = vec![
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-contextRow-sep"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-contextRow-source"),
                    ),
                    ("data-context-source", JsValue::TRUE),
                ])?),
                &[label],
            )?,
        ];
        if let Some(summary) = resolved.summary.as_ref() {
            children.push(create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-contextRow-sep"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[],
            )?);
            children.push(create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-contextRow-summary"),
                    ),
                    ("data-context-summary", JsValue::TRUE),
                ])?),
                &[JsValue::from_str(summary)],
            )?);
        }
        create_element(&modules.react, &modules.fragment, None, &children)?
    };
    let toggle_setter = set_open;
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let invert = Closure::wrap(
            Box::new(move |value: JsValue| !value.as_bool().unwrap_or(false))
                as Box<dyn FnMut(JsValue) -> bool>,
        )
        .into_js_value();
        toggle_setter.call1(&JsValue::UNDEFINED, &invert)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let body = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextRow-body"),
            ),
            ("data-context-injection-body", JsValue::TRUE),
            (
                "data-context-form",
                resolved
                    .rendered
                    .map_or(JsValue::UNDEFINED, JsValue::from_str),
            ),
        ])?),
        &[resolved.body],
    )?;
    create_element(
        &modules.react,
        &modules.disclosure_row,
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextRow-root"),
            ),
            (
                "icon",
                create_element(
                    &modules.react,
                    &modules.browse_icon,
                    Some(&object(&[("size", JsValue::from_f64(14.0))])?),
                    &[],
                )?,
            ),
            (
                "chevronClassName",
                JsValue::from_str("seekdeep-conversation-contextRow-chevron"),
            ),
            ("title", JsValue::from_str(&title)),
            ("collapsedContent", collapsed),
            ("keepContentWhenOpen", JsValue::TRUE),
            ("open", JsValue::from_bool(open)),
            ("expandable", JsValue::TRUE),
            ("expandOnRowClick", JsValue::TRUE),
            ("onToggle", toggle),
        ])?),
        &[body],
    )
}

fn resolve_body(
    modules: &BrowserModules,
    form: &JsValue,
    props: &JsValue,
) -> Result<ResolvedBody, JsValue> {
    let source = Reflect::get(props, &JsValue::from_str("source"))?;
    let opaque = || -> Result<ResolvedBody, JsValue> {
        Ok(ResolvedBody {
            rendered: None,
            summary: None,
            body: render_opaque(modules, props)?,
        })
    };
    if form.is_null() {
        return opaque();
    }
    match form.as_string().as_deref() {
        Some("instructions") => {
            if instruction_changes(&source)?.is_none() {
                opaque()
            } else {
                Ok(ResolvedBody {
                    rendered: Some("instructions"),
                    summary: None,
                    body: render_instructions(modules, props)?,
                })
            }
        }
        Some("catalog") => {
            if catalog_entries(&source)?.is_none() {
                opaque()
            } else {
                Ok(ResolvedBody {
                    rendered: Some("catalog"),
                    summary: None,
                    body: render_catalog(modules, props)?,
                })
            }
        }
        Some("snapshot") => {
            if snapshot_sections(&source)?.is_none() {
                opaque()
            } else {
                Ok(ResolvedBody {
                    rendered: Some("snapshot"),
                    summary: None,
                    body: render_snapshot(modules, props)?,
                })
            }
        }
        Some("notice") => {
            if let Some(summary) = notice_summary(&source)? {
                Ok(ResolvedBody {
                    rendered: Some("notice"),
                    summary: Some(summary),
                    body: render_notice(modules, props)?,
                })
            } else {
                opaque()
            }
        }
        Some("relay") => {
            if relay_sender(&source)?.is_none() {
                opaque()
            } else {
                Ok(ResolvedBody {
                    rendered: Some("relay"),
                    summary: None,
                    body: render_relay(modules, props)?,
                })
            }
        }
        Some("recall") => {
            if recalled_sessions(&source)?.is_none() {
                opaque()
            } else {
                Ok(ResolvedBody {
                    rendered: Some("recall"),
                    summary: None,
                    body: render_recall(modules, props)?,
                })
            }
        }
        _ => Err(js_sys::Error::new(&format!(
            "unreachable context form: {}",
            javascript_string(form)?
        ))
        .into()),
    }
}

fn resolved_object(resolved: ResolvedBody) -> Result<JsValue, JsValue> {
    Ok(object(&[
        (
            "rendered",
            resolved.rendered.map_or(JsValue::NULL, JsValue::from_str),
        ),
        (
            "summary",
            resolved
                .summary
                .map_or(JsValue::NULL, |value| JsValue::from_str(&value)),
        ),
        ("body", resolved.body),
    ])?
    .into())
}

fn render_opaque(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let content = required_property(props, "content", "context body props")?.dyn_into::<Array>()?;
    let source = Reflect::get(props, &JsValue::from_str("source"))?;
    let translate = required_function(props, "t", "context body props")?;
    let mut children = model_facing_content(modules, &content, &translate)?;
    if let Some(fields) = source_fields(modules, &source, false, &translate)? {
        children.push(fields);
    }
    fragment(modules, &children)
}

fn render_instructions(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let content = required_property(props, "content", "context body props")?.dyn_into::<Array>()?;
    let source = Reflect::get(props, &JsValue::from_str("source"))?;
    let translate = required_function(props, "t", "context body props")?;
    let Some(changes) = instruction_changes(&source)? else {
        return render_opaque(modules, props);
    };
    let baseline = record_property(&source, "baseline")?.as_bool() == Some(true);
    let mut rows = Vec::new();
    for change in changes {
        let key = instruction_action(&change.action, baseline);
        rows.push(create_element(
            &modules.react,
            &JsValue::from_str("li"),
            Some(&object(&[
                ("key", JsValue::from_str(&change.path)),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-contextBody-file"),
                ),
                (
                    "title",
                    change
                        .digest
                        .as_ref()
                        .map_or(JsValue::UNDEFINED, |value| JsValue::from_str(value)),
                ),
            ])?),
            &[
                span(
                    modules,
                    "seekdeep-conversation-contextBody-filePath",
                    JsValue::from_str(&change.path),
                )?,
                span(
                    modules,
                    "seekdeep-conversation-contextBody-fileAction",
                    JsValue::from_str(&translate_text(&translate, key, None)?),
                )?,
            ],
        )?);
    }
    let files = create_element(
        &modules.react,
        &JsValue::from_str("ul"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextBody-files"),
            ),
            ("data-context-files", JsValue::TRUE),
        ])?),
        &rows,
    )?;
    let mut children = vec![files];
    children.extend(model_facing_content(modules, &content, &translate)?);
    fragment(modules, &children)
}

#[allow(clippy::too_many_lines)]
fn render_catalog(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let content = required_property(props, "content", "context body props")?.dyn_into::<Array>()?;
    let source = Reflect::get(props, &JsValue::from_str("source"))?;
    let translate = required_function(props, "t", "context body props")?;
    let Some(entries) = catalog_entries(&source)? else {
        return render_opaque(modules, props);
    };
    let update = record_property(&source, "update")?.as_bool() == Some(true);
    let mut children = Vec::new();
    if update {
        children.push(paragraph(
            modules,
            "seekdeep-conversation-contextBody-catalogNotice",
            "data-context-catalog-update",
            JsValue::from_str(&translate_text(
                &translate,
                "message.context.catalog.replaced",
                None,
            )?),
        )?);
    }
    let shown_len = entries.len().min(MAX_ENTRIES);
    let mut rows = Vec::new();
    for (index, entry) in entries.iter().take(shown_len).enumerate() {
        rows.push(create_element(
            &modules.react,
            &JsValue::from_str("li"),
            Some(&object(&[
                ("key", index_value(index)?),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-contextBody-entry"),
                ),
            ])?),
            &[
                create_element(
                    &modules.react,
                    &JsValue::from_str("code"),
                    Some(&class_props("seekdeep-conversation-contextBody-entryName")?),
                    &[JsValue::from_str(&entry.name)],
                )?,
                span(
                    modules,
                    "seekdeep-conversation-contextBody-entryDescription",
                    JsValue::from_str(&entry.description),
                )?,
            ],
        )?);
    }
    children.push(create_element(
        &modules.react,
        &JsValue::from_str("ul"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextBody-entries"),
            ),
            ("data-context-entries", JsValue::TRUE),
        ])?),
        &rows,
    )?);
    if shown_len < entries.len() {
        children.push(paragraph(
            modules,
            "seekdeep-conversation-contextBody-catalogNotice",
            "data-context-entries-truncated",
            JsValue::from_str(&translate_text(
                &translate,
                "message.context.catalog.more",
                Some(&object(&[(
                    "count",
                    index_value(entries.len() - shown_len)?,
                )])?),
            )?),
        )?);
    }
    children.extend(unknown_blocks(modules, &content, &translate)?);
    fragment(modules, &children)
}

fn render_snapshot(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let source = Reflect::get(props, &JsValue::from_str("source"))?;
    let translate = required_function(props, "t", "context body props")?;
    let Some(sections) = snapshot_sections(&source)? else {
        return render_opaque(modules, props);
    };
    let notice = paragraph(
        modules,
        "seekdeep-conversation-contextBody-catalogNotice",
        "data-context-snapshot-supersedes",
        JsValue::from_str(&translate_text(
            &translate,
            "message.context.snapshot.supersedes",
            None,
        )?),
    )?;
    let mut rows = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        rows.push(create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("key", index_value(index)?),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-contextBody-section"),
                ),
            ])?),
            &[
                create_element(
                    &modules.react,
                    &JsValue::from_str("dt"),
                    Some(&class_props(
                        "seekdeep-conversation-contextBody-sectionName",
                    )?),
                    &[JsValue::from_str(&section.name)],
                )?,
                create_element(
                    &modules.react,
                    &JsValue::from_str("dd"),
                    Some(&class_props(
                        "seekdeep-conversation-contextBody-sectionText",
                    )?),
                    &[bounded_text(&section.text, &translate)?],
                )?,
            ],
        )?);
    }
    let list = create_element(
        &modules.react,
        &JsValue::from_str("dl"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextBody-sections"),
            ),
            ("data-context-sections", JsValue::TRUE),
        ])?),
        &rows,
    )?;
    fragment(modules, &[notice, list])
}

fn render_notice(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let content = required_property(props, "content", "context body props")?.dyn_into::<Array>()?;
    let translate = required_function(props, "t", "context body props")?;
    fragment(
        modules,
        &model_facing_content(modules, &content, &translate)?,
    )
}

fn render_relay(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let content = required_property(props, "content", "context body props")?.dyn_into::<Array>()?;
    let source = Reflect::get(props, &JsValue::from_str("source"))?;
    let translate = required_function(props, "t", "context body props")?;
    let Some(sender) = relay_sender(&source)? else {
        return render_opaque(modules, props);
    };
    let sender = paragraph(
        modules,
        "seekdeep-conversation-contextBody-relaySender",
        "data-context-relay-sender",
        JsValue::from_str(&translate_text(
            &translate,
            "message.context.relay.from",
            Some(&object(&[("session", JsValue::from_str(&sender))])?),
        )?),
    )?;
    let mut children = vec![sender];
    children.extend(model_facing_content(modules, &content, &translate)?);
    fragment(modules, &children)
}

fn render_recall(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let content = required_property(props, "content", "context body props")?.dyn_into::<Array>()?;
    let source = Reflect::get(props, &JsValue::from_str("source"))?;
    let translate = required_function(props, "t", "context body props")?;
    let Some(sessions) = recalled_sessions(&source)? else {
        return render_opaque(modules, props);
    };
    let mut rows = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        let mut children = vec![span(
            modules,
            "seekdeep-conversation-contextBody-recallLabel",
            JsValue::from_str(&session.label),
        )?];
        children.push(span(
            modules,
            "seekdeep-conversation-contextBody-recallCounts",
            JsValue::from_str(&translate_text(
                &translate,
                "message.context.recall.counts",
                Some(&object(&[
                    ("retained", session.retained.clone()),
                    ("omitted", session.omitted.clone()),
                ])?),
            )?),
        )?);
        if session.truncated {
            children.push(span(
                modules,
                "seekdeep-conversation-contextBody-recallCounts",
                JsValue::from_str(&translate_text(
                    &translate,
                    "message.context.recall.truncated",
                    None,
                )?),
            )?);
        }
        rows.push(create_element(
            &modules.react,
            &JsValue::from_str("li"),
            Some(&object(&[
                ("key", index_value(index)?),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-contextBody-recall"),
                ),
            ])?),
            &children,
        )?);
    }
    let list = create_element(
        &modules.react,
        &JsValue::from_str("ul"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextBody-recalls"),
            ),
            ("data-context-recalls", JsValue::TRUE),
        ])?),
        &rows,
    )?;
    let mut children = vec![list];
    children.extend(model_facing_content(modules, &content, &translate)?);
    fragment(modules, &children)
}

fn model_facing_content(
    modules: &BrowserModules,
    content: &Array,
    translate: &Function,
) -> Result<Vec<JsValue>, JsValue> {
    let mut children = Vec::new();
    for (index, run) in content_runs(content)?.into_iter().enumerate() {
        match run {
            ContentRun::Text(text) => {
                if JsString::from(text.clone()).length() > 0 {
                    children.push(create_element(
                        &modules.react,
                        &JsValue::from_str("pre"),
                        Some(&object(&[
                            ("key", index_value(index)?),
                            (
                                "className",
                                JsValue::from_str("seekdeep-conversation-contextBody-text"),
                            ),
                            ("data-context-text", JsValue::TRUE),
                        ])?),
                        &[bounded_text(&text, translate)?],
                    )?);
                }
            }
            ContentRun::Block(block) => {
                children.push(json_block(modules, block, index, translate)?);
            }
        }
    }
    Ok(children)
}

fn unknown_blocks(
    modules: &BrowserModules,
    content: &Array,
    translate: &Function,
) -> Result<Vec<JsValue>, JsValue> {
    let mut children = Vec::new();
    for run in content_runs(content)? {
        if let ContentRun::Block(block) = run {
            let index = children.len();
            children.push(json_block(modules, block, index, translate)?);
        }
    }
    Ok(children)
}

fn json_block(
    modules: &BrowserModules,
    block: JsValue,
    index: usize,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let label = translate_text(translate, "message.unknownBlock", None)?;
    let truncated_translate = translate.clone();
    let truncated = Closure::wrap(Box::new(move |total: JsValue| -> Result<JsValue, JsValue> {
        truncated_translate.apply(
            &JsValue::UNDEFINED,
            &Array::of2(
                &JsValue::from_str("json.truncated"),
                object(&[("total", total)])?.as_ref(),
            ),
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    create_element(
        &modules.react,
        &modules.json_block,
        Some(&object(&[
            ("key", index_value(index)?),
            ("label", JsValue::from_str(&label)),
            ("payload", block),
            ("truncatedLabel", truncated),
        ])?),
        &[],
    )
}

fn source_fields(
    modules: &BrowserModules,
    source: &JsValue,
    form_rendered: bool,
    translate: &Function,
) -> Result<Option<JsValue>, JsValue> {
    let Some(record) = as_record(source) else {
        return Ok(None);
    };
    let entries = Object::entries(&record);
    let mut rows = Vec::new();
    for index in 0..entries.length() {
        let pair = entries.get(index).dyn_into::<Array>()?;
        let key = pair.get(0).as_string().unwrap_or_default();
        if key == "kind" || (form_rendered && key == "form") {
            continue;
        }
        rows.push(create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("key", JsValue::from_str(&key)),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-contextBody-field"),
                ),
            ])?),
            &[
                create_element(
                    &modules.react,
                    &JsValue::from_str("dt"),
                    Some(&class_props("seekdeep-conversation-contextBody-fieldKey")?),
                    &[JsValue::from_str(&key)],
                )?,
                create_element(
                    &modules.react,
                    &JsValue::from_str("dd"),
                    Some(&class_props(
                        "seekdeep-conversation-contextBody-fieldValue",
                    )?),
                    &[field_value(&pair.get(1), translate)?],
                )?,
            ],
        )?);
    }
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(create_element(
        &modules.react,
        &JsValue::from_str("dl"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-contextBody-fields"),
            ),
            ("data-context-fields", JsValue::TRUE),
        ])?),
        &rows,
    )?))
}

fn field_value(value: &JsValue, translate: &Function) -> Result<JsValue, JsValue> {
    let text = if value.is_string() {
        value.clone()
    } else if value.as_f64().is_some() || value.as_bool().is_some() {
        JsValue::from_str(&javascript_string(value)?)
    } else {
        JSON::stringify(value)?.into()
    };
    bounded_text(&text, translate)
}

fn bounded_text(text: &JsValue, translate: &Function) -> Result<JsValue, JsValue> {
    let text = JsString::from(text.clone());
    let length = text.length();
    if length <= MAX_CHARS {
        return Ok(text.into());
    }
    let suffix = translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("json.truncated"),
            object(&[("total", JsValue::from_f64(f64::from(length)))])?.as_ref(),
        ),
    )?;
    let parts = Array::of3(
        text.slice(0, MAX_CHARS).as_ref(),
        &JsValue::from_str("\n"),
        &suffix,
    );
    Ok(parts.join("").into())
}

fn content_runs(content: &Array) -> Result<Vec<ContentRun>, JsValue> {
    let mut runs = Vec::new();
    for index in 0..content.length() {
        let block = content.get(index);
        let block_type = Reflect::get(&block, &JsValue::from_str("type"))?;
        if block_type.as_string().as_deref() != Some("text") {
            runs.push(ContentRun::Block(block));
            continue;
        }
        let text = required_property(&block, "text", "context text block")?;
        if let Some(ContentRun::Text(previous)) = runs.last_mut() {
            *previous = JsString::from(previous.clone()).concat(&text).into();
        } else {
            runs.push(ContentRun::Text(text));
        }
    }
    Ok(runs)
}

fn instruction_changes(source: &JsValue) -> Result<Option<Vec<InstructionChange>>, JsValue> {
    let Some(record) = as_record(source) else {
        return Ok(None);
    };
    let list = Reflect::get(&record, &JsValue::from_str("changes"))?;
    if !Array::is_array(&list) {
        return Ok(None);
    }
    let list = list.dyn_into::<Array>()?;
    let mut changes = Vec::new();
    let mut seen = BTreeSet::new();
    for index in 0..list.length() {
        let Some(change) = as_record(&list.get(index)) else {
            return Ok(None);
        };
        let path = Reflect::get(&change, &JsValue::from_str("path"))?;
        let Some(path) = path.as_string().filter(|path| !path.is_empty()) else {
            return Ok(None);
        };
        let action = Reflect::get(&change, &JsValue::from_str("action"))?;
        let Some(action) = action.as_string() else {
            return Ok(None);
        };
        if !matches!(action.as_str(), "set" | "replace" | "remove") {
            return Ok(None);
        }
        let digest = Reflect::get(&change, &JsValue::from_str("digest"))?.as_string();
        if !seen.insert(path.clone()) {
            continue;
        }
        changes.push(InstructionChange {
            action,
            path,
            digest,
        });
    }
    Ok((!changes.is_empty()).then_some(changes))
}

fn instruction_action(action: &str, baseline: bool) -> &'static str {
    if action == "remove" {
        "message.context.instructions.removed"
    } else if baseline {
        "message.context.instructions.loaded"
    } else if action == "set" {
        "message.context.instructions.added"
    } else {
        "message.context.instructions.updated"
    }
}

fn catalog_entries(source: &JsValue) -> Result<Option<Vec<CatalogEntry>>, JsValue> {
    let Some(record) = as_record(source) else {
        return Ok(None);
    };
    let list = Reflect::get(&record, &JsValue::from_str("entries"))?;
    if !Array::is_array(&list) {
        return Ok(None);
    }
    let list = list.dyn_into::<Array>()?;
    let mut entries = Vec::new();
    for index in 0..list.length() {
        let Some(entry) = as_record(&list.get(index)) else {
            return Ok(None);
        };
        let name = Reflect::get(&entry, &JsValue::from_str("name"))?;
        let description = Reflect::get(&entry, &JsValue::from_str("description"))?;
        let Some(name) = name.as_string().filter(|name| !name.is_empty()) else {
            return Ok(None);
        };
        let Some(description) = description.as_string() else {
            return Ok(None);
        };
        entries.push(CatalogEntry { name, description });
    }
    Ok(Some(entries))
}

fn snapshot_sections(source: &JsValue) -> Result<Option<Vec<SnapshotSection>>, JsValue> {
    let Some(record) = as_record(source) else {
        return Ok(None);
    };
    let list = Reflect::get(&record, &JsValue::from_str("sections"))?;
    if !Array::is_array(&list) {
        return Ok(None);
    }
    let list = list.dyn_into::<Array>()?;
    let mut sections = Vec::new();
    for index in 0..list.length() {
        let Some(section) = as_record(&list.get(index)) else {
            return Ok(None);
        };
        let name = Reflect::get(&section, &JsValue::from_str("name"))?;
        let text = Reflect::get(&section, &JsValue::from_str("text"))?;
        let Some(name) = name.as_string().filter(|name| !name.is_empty()) else {
            return Ok(None);
        };
        if !text.is_string() {
            return Ok(None);
        }
        sections.push(SnapshotSection { name, text });
    }
    Ok((!sections.is_empty()).then_some(sections))
}

fn relay_sender(source: &JsValue) -> Result<Option<String>, JsValue> {
    let sender = record_property(source, "senderSessionId")?;
    Ok(sender.as_string().filter(|sender| !sender.is_empty()))
}

fn recalled_sessions(source: &JsValue) -> Result<Option<Vec<RecalledSession>>, JsValue> {
    let Some(record) = as_record(source) else {
        return Ok(None);
    };
    let list = Reflect::get(&record, &JsValue::from_str("references"))?;
    if !Array::is_array(&list) {
        return Ok(None);
    }
    let list = list.dyn_into::<Array>()?;
    let mut sessions = Vec::new();
    for index in 0..list.length() {
        let Some(reference) = as_record(&list.get(index)) else {
            return Ok(None);
        };
        let label = Reflect::get(&reference, &JsValue::from_str("label"))?;
        let retained = Reflect::get(&reference, &JsValue::from_str("retainedMessages"))?;
        let omitted = Reflect::get(&reference, &JsValue::from_str("omittedMessages"))?;
        let truncated = Reflect::get(&reference, &JsValue::from_str("truncated"))?;
        let Some(label) = label.as_string().filter(|label| !label.is_empty()) else {
            return Ok(None);
        };
        if retained.as_f64().is_none() || omitted.as_f64().is_none() {
            return Ok(None);
        }
        let Some(truncated) = truncated.as_bool() else {
            return Ok(None);
        };
        sessions.push(RecalledSession {
            label,
            retained,
            omitted,
            truncated,
        });
    }
    Ok((!sessions.is_empty()).then_some(sessions))
}

fn notice_summary(source: &JsValue) -> Result<Option<String>, JsValue> {
    let summary = record_property(source, "summary")?;
    Ok(summary.as_string().filter(|summary| !summary.is_empty()))
}

fn as_record(value: &JsValue) -> Option<Object> {
    (value.is_object() && !value.is_null() && !Array::is_array(value))
        .then(|| value.clone().unchecked_into::<Object>())
}

fn record_property(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let Some(record) = as_record(value) else {
        return Ok(JsValue::UNDEFINED);
    };
    Reflect::get(&record, &JsValue::from_str(key))
}

fn paragraph(
    modules: &BrowserModules,
    class_name: &str,
    marker: &str,
    text: JsValue,
) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("p"),
        Some(&object(&[
            ("className", JsValue::from_str(class_name)),
            (marker, JsValue::TRUE),
        ])?),
        &[text],
    )
}

fn span(modules: &BrowserModules, class_name: &str, text: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&class_props(class_name)?),
        &[text],
    )
}

fn fragment(modules: &BrowserModules, children: &[JsValue]) -> Result<JsValue, JsValue> {
    create_element(&modules.react, &modules.fragment, None, children)
}

fn translate_text(
    translate: &Function,
    key: &str,
    parameters: Option<&Object>,
) -> Result<String, JsValue> {
    let value = if let Some(parameters) = parameters {
        translate.apply(
            &JsValue::UNDEFINED,
            &Array::of2(&JsValue::from_str(key), parameters.as_ref()),
        )?
    } else {
        translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))?
    };
    value
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{key} translation must be string")).into())
}

fn configured_components() -> Result<ContextComponents, JsValue> {
    COMPONENTS.with(|components| {
        components.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation context bodies were not configured").into()
        })
    })
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation context modules were not configured").into()
        })
    })
}

fn index_value(index: usize) -> Result<JsValue, JsValue> {
    let index = u32::try_from(index).map_err(|_| {
        js_sys::RangeError::new("context list index exceeds JavaScript array range")
    })?;
    Ok(JsValue::from_f64(f64::from(index)))
}

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required_property(value, key, owner)?.dyn_into()
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn create_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(kind);
    arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        arguments.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}

fn javascript_string(value: &JsValue) -> Result<String, JsValue> {
    required_function(&js_sys::global(), "String", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String() returned non-string").into())
}
