//! Compiled controlled dropdown with portal placement, nested rows, and pointer grace.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    ICON_DEFINITIONS, browser_icons::render_icon, configure_client_ui_primitive_hooks,
    use_pointer_grace,
};

const MENU_CSS: &str = include_str!("../../../packages/client/ui-primitives/src/Menu.module.css");
const VIEWPORT_MARGIN: f64 = 12.0;
const CARD_GAP: f64 = 4.0;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    react_dom: JsValue,
}

/// Configures React/ReactDOM and installs the `Menu` stylesheet.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveMenu)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_menu(
    react: JsValue,
    react_dom: JsValue,
) -> Result<(), JsValue> {
    configure_client_ui_primitive_hooks(react.clone());
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules { react, react_dom });
    });
    inject_style()
}

/// Returns the compiled `Menu` component.
///
/// # Errors
///
/// Returns before the browser modules are configured.
#[wasm_bindgen(js_name = menuComponent)]
pub fn menu_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_menu(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

#[allow(clippy::too_many_lines)]
fn render_menu(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &modules.react;
    let open = property_truthy(props, "open")?;
    let anchor = Reflect::get(props, &JsValue::from_str("anchor"))?;
    let items = required_array(props, "items", "Menu props")?;
    let footer = optional_array(props, "footer")?;
    let selected_id = optional_string(props, "selectedId")?;
    let selected_ids = optional_array(props, "selectedIds")?;
    let on_select = required_function(props, "onSelect", "Menu props")?;
    let on_close = required_function(props, "onClose", "Menu props")?;
    let align = optional_string(props, "align")?.unwrap_or_else(|| "start".to_owned());
    let side = optional_string(props, "side")?.unwrap_or_else(|| "bottom".to_owned());
    let portal = property_truthy(props, "portal")?;
    let close_on_pointer_leave = property_truthy(props, "closeOnPointerLeave")?;
    let dense = property_truthy(props, "dense")?;
    let compact = property_truthy(props, "compact")?;
    let get_anchor_rect = optional_function(props, "getAnchorRect")?;
    let class_name = optional_string(props, "className")?;

    let root_ref = use_ref(react, &JsValue::NULL)?;
    let list_ref = use_ref(react, &JsValue::NULL)?;
    let (open_submenu, set_open_submenu) = use_state(react, &JsValue::NULL)?;
    let (fixed_position, set_fixed_position) = use_state(react, &JsValue::NULL)?;
    let grace = use_pointer_grace(on_close.clone())?;
    let arm_close = required_function(&grace, "arm", "pointer grace")?;
    let cancel_close = required_function(&grace, "cancel", "pointer grace")?;

    install_placement_effect(
        react,
        PlacementInputs {
            open,
            portal,
            align: align.clone(),
            side: side.clone(),
            get_anchor_rect: get_anchor_rect.clone(),
            root_ref: root_ref.clone(),
            list_ref: list_ref.clone(),
            set_fixed_position,
        },
    )?;
    install_dismiss_effect(
        react,
        open,
        &root_ref,
        &list_ref,
        &set_open_submenu,
        &on_close,
    )?;
    install_grace_disarm_effect(react, open, &cancel_close)?;

    let scrollable = !items.iter().any(|entry| {
        !is_separator(&entry)
            && !is_label(&entry)
            && optional_array(&entry, "submenu")
                .ok()
                .flatten()
                .is_some_and(|submenu| submenu.length() > 0)
    });

    let entry_context = EntryContext {
        modules: modules.clone(),
        selected_id,
        selected_ids,
        open_submenu,
        set_open_submenu,
        on_select,
        compact,
    };
    let list = if open {
        let item_nodes = render_entries(&items, &entry_context)?;
        let viewport = create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-menu-viewport"),
                ),
                ("role", JsValue::from_str("presentation")),
            ])?),
            &item_nodes,
        )?;
        let mut children = vec![viewport];
        if let Some(footer) = footer.filter(|footer| footer.length() > 0) {
            children.push(create_element(
                react,
                &JsValue::from_str("div"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-primitive-menu-footer"),
                    ),
                    ("role", JsValue::from_str("presentation")),
                ])?),
                &render_entries(&footer, &entry_context)?,
            )?);
        }
        let mut classes = vec!["seekdeep-primitive-menu-list"];
        if dense {
            classes.push("seekdeep-primitive-menu-denseList");
        }
        if compact {
            classes.push("seekdeep-primitive-menu-compactList");
        }
        if scrollable {
            classes.push("seekdeep-primitive-menu-scrollable");
        }
        if portal {
            classes.push("seekdeep-primitive-menu-portal");
        }
        if side == "top" && !portal {
            classes.push("seekdeep-primitive-menu-sideTop");
        }
        if align == "end" && !portal {
            classes.push("seekdeep-primitive-menu-alignEnd");
        }
        let stop = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            call_method(&event, "stopPropagation", &[])?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let style = if portal {
            if fixed_position.is_null() {
                object(&[
                    ("visibility", JsValue::from_str("hidden")),
                    ("left", JsValue::from_f64(0.0)),
                    ("top", JsValue::from_f64(0.0)),
                ])?
                .into()
            } else {
                fixed_position.clone()
            }
        } else {
            JsValue::UNDEFINED
        };
        create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("ref", list_ref.clone()),
                ("className", JsValue::from_str(&classes.join(" "))),
                ("style", style),
                ("role", JsValue::from_str("menu")),
                ("onClick", stop.into_js_value()),
            ])?),
            &children,
        )?
    } else {
        JsValue::FALSE
    };

    let mut wrapper_children = vec![anchor];
    if portal {
        if !list.is_falsy() {
            let document = required_property(&js_sys::global(), "document", "global")?;
            let body = required_property(&document, "body", "document")?;
            wrapper_children.push(
                required_function(&modules.react_dom, "createPortal", "ReactDOM")?.call2(
                    &modules.react_dom,
                    &list,
                    &body,
                )?,
            );
        }
    } else if !list.is_falsy() {
        wrapper_children.push(list);
    }

    let pointer_enter = if close_on_pointer_leave {
        cancel_close.into()
    } else {
        JsValue::UNDEFINED
    };
    let pointer_leave = if close_on_pointer_leave {
        let arm_close = arm_close;
        Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            if open {
                arm_close.call0(&JsValue::UNDEFINED)?;
            }
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value()
    } else {
        JsValue::UNDEFINED
    };
    let mut root_classes = vec!["seekdeep-primitive-menu-root".to_owned()];
    if let Some(class_name) = class_name {
        root_classes.push(class_name);
    }
    create_element(
        react,
        &JsValue::from_str("span"),
        Some(&object(&[
            ("ref", root_ref),
            ("className", JsValue::from_str(&root_classes.join(" "))),
            ("onPointerEnter", pointer_enter),
            ("onPointerLeave", pointer_leave),
        ])?),
        &wrapper_children,
    )
}

struct PlacementInputs {
    open: bool,
    portal: bool,
    align: String,
    side: String,
    get_anchor_rect: Option<Function>,
    root_ref: JsValue,
    list_ref: JsValue,
    set_fixed_position: Function,
}

#[allow(clippy::too_many_lines)]
fn install_placement_effect(react: &JsValue, inputs: PlacementInputs) -> Result<(), JsValue> {
    let dependencies = Array::new();
    dependencies.push(&JsValue::from_bool(inputs.open));
    dependencies.push(&JsValue::from_bool(inputs.portal));
    dependencies.push(&JsValue::from_str(&inputs.align));
    dependencies.push(&JsValue::from_str(&inputs.side));
    dependencies.push(
        inputs
            .get_anchor_rect
            .as_ref()
            .map_or(&JsValue::UNDEFINED, AsRef::as_ref),
    );
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !inputs.open || !inputs.portal {
            set_state(&inputs.set_fixed_position, &JsValue::NULL)?;
            return Ok(JsValue::UNDEFINED);
        }
        let align = inputs.align.clone();
        let side = inputs.side.clone();
        let get_anchor_rect = inputs.get_anchor_rect.clone();
        let root_ref = inputs.root_ref.clone();
        let list_ref = inputs.list_ref.clone();
        let set_fixed_position = inputs.set_fixed_position.clone();
        let place = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let bounds = if let Some(get_anchor_rect) = &get_anchor_rect {
                get_anchor_rect.call0(&JsValue::UNDEFINED)?
            } else {
                let root = current(&root_ref)?;
                if root.is_null() {
                    return Ok(());
                }
                call_method(&root, "getBoundingClientRect", &[])?
            };
            if bounds.is_null() {
                return Ok(());
            }
            let left = required_number(&bounds, "left", "Menu anchor DOMRect")?;
            let right = required_number(&bounds, "right", "Menu anchor DOMRect")?;
            let top = required_number(&bounds, "top", "Menu anchor DOMRect")?;
            let bottom = required_number(&bounds, "bottom", "Menu anchor DOMRect")?;
            let list = current(&list_ref)?;
            let width = if list.is_null() {
                0.0
            } else {
                required_number(&list, "offsetWidth", "Menu list")?
            };
            let height = if list.is_null() {
                0.0
            } else {
                required_number(&list, "offsetHeight", "Menu list")?
            };
            let (mut x, mut y) = if side == "right" {
                (right + CARD_GAP, top)
            } else {
                let x = if align == "start" {
                    left
                } else {
                    right - width
                };
                let y = if side == "bottom" {
                    bottom + CARD_GAP
                } else {
                    top - height - CARD_GAP
                };
                (x, y)
            };
            let window = required_property(&js_sys::global(), "window", "global")?;
            let viewport_width = required_number(&window, "innerWidth", "window")?;
            let viewport_height = required_number(&window, "innerHeight", "window")?;
            if width > 0.0 {
                x = x
                    .max(VIEWPORT_MARGIN)
                    .min(viewport_width - width - VIEWPORT_MARGIN);
            }
            if height > 0.0 {
                y = y
                    .max(VIEWPORT_MARGIN)
                    .min(viewport_height - height - VIEWPORT_MARGIN);
            }
            set_state(
                &set_fixed_position,
                &object(&[
                    ("left", JsValue::from_f64(x)),
                    ("top", JsValue::from_f64(y)),
                ])?
                .into(),
            )
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let place = place.into_js_value().dyn_into::<Function>()?;
        place.call0(&JsValue::UNDEFINED)?;
        let window = required_property(&js_sys::global(), "window", "global")?;
        call_method(
            &window,
            "addEventListener",
            &[
                JsValue::from_str("scroll"),
                place.clone().into(),
                JsValue::TRUE,
            ],
        )?;
        call_method(
            &window,
            "addEventListener",
            &[JsValue::from_str("resize"), place.clone().into()],
        )?;
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(
                &window,
                "removeEventListener",
                &[
                    JsValue::from_str("scroll"),
                    place.clone().into(),
                    JsValue::TRUE,
                ],
            )?;
            call_method(
                &window,
                "removeEventListener",
                &[JsValue::from_str("resize"), place.clone().into()],
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_layout_effect(react, &effect.into_js_value(), &dependencies)
}

fn install_dismiss_effect(
    react: &JsValue,
    open: bool,
    root_ref: &JsValue,
    list_ref: &JsValue,
    set_open_submenu: &Function,
    on_close: &Function,
) -> Result<(), JsValue> {
    let root_ref = root_ref.clone();
    let list_ref = list_ref.clone();
    let set_open_submenu = set_open_submenu.clone();
    let on_close = on_close.clone();
    let on_close_dependency = on_close.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open {
            set_state(&set_open_submenu, &JsValue::NULL)?;
            return Ok(JsValue::UNDEFINED);
        }
        let pointer_root = root_ref.clone();
        let pointer_list = list_ref.clone();
        let pointer_close = on_close.clone();
        let pointer_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let target = required_property(&event, "target", "pointer event")?;
            if !target.is_instance_of::<web_sys::Node>() {
                return Ok(());
            }
            if node_contains(&current(&pointer_root)?, &target)
                || node_contains(&current(&pointer_list)?, &target)
            {
                return Ok(());
            }
            pointer_close.call0(&JsValue::UNDEFINED)?;
            Ok(())
        })
            as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        let key_close = on_close.clone();
        let key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if required_property(&event, "key", "keyboard event")?
                .as_string()
                .as_deref()
                == Some("Escape")
            {
                key_close.call0(&JsValue::UNDEFINED)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        let document = required_property(&js_sys::global(), "document", "global")?;
        call_method(
            &document,
            "addEventListener",
            &[JsValue::from_str("pointerdown"), pointer_down.clone()],
        )?;
        call_method(
            &document,
            "addEventListener",
            &[JsValue::from_str("keydown"), key_down.clone()],
        )?;
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(
                &document,
                "removeEventListener",
                &[JsValue::from_str("pointerdown"), pointer_down.clone()],
            )?;
            call_method(
                &document,
                "removeEventListener",
                &[JsValue::from_str("keydown"), key_down.clone()],
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(open), on_close_dependency.as_ref()),
    )
}

fn install_grace_disarm_effect(
    react: &JsValue,
    open: bool,
    cancel_close: &Function,
) -> Result<(), JsValue> {
    let cancel_dependency = cancel_close.clone();
    let cancel_close = cancel_close.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if !open {
            cancel_close.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(open), cancel_dependency.as_ref()),
    )
}

#[derive(Clone)]
struct EntryContext {
    modules: BrowserModules,
    selected_id: Option<String>,
    selected_ids: Option<Array>,
    open_submenu: JsValue,
    set_open_submenu: Function,
    on_select: Function,
    compact: bool,
}

fn render_entries(entries: &Array, context: &EntryContext) -> Result<Vec<JsValue>, JsValue> {
    entries
        .iter()
        .map(|entry| render_entry(&entry, context))
        .collect()
}

#[allow(clippy::too_many_lines)]
fn render_entry(entry: &JsValue, context: &EntryContext) -> Result<JsValue, JsValue> {
    let react = &context.modules.react;
    let id = required_string(entry, "id", "Menu entry")?;
    if is_separator(entry) {
        return create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("key", JsValue::from_str(&id)),
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-menu-separator"),
                ),
                ("role", JsValue::from_str("separator")),
            ])?),
            &[],
        );
    }
    if is_label(entry) {
        return create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("key", JsValue::from_str(&id)),
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-menu-label"),
                ),
                ("role", JsValue::from_str("presentation")),
            ])?),
            &[JsValue::from_str(&required_string(
                entry,
                "text",
                "Menu label",
            )?)],
        );
    }
    let label = required_property(entry, "label", "Menu item")?;
    let submenu = optional_array(entry, "submenu")?;
    let has_submenu = submenu.as_ref().is_some_and(|submenu| submenu.length() > 0);
    let submenu_open = has_submenu && context.open_submenu.as_string().as_deref() == Some(&id);
    let selected = context.selected_id.as_deref() == Some(&id)
        || context.selected_ids.as_ref().is_some_and(|ids| {
            ids.iter()
                .any(|value| value.as_string().as_deref() == Some(&id))
        });
    let set_enter = context.set_open_submenu.clone();
    let enter_id = id.clone();
    let mouse_enter = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        set_state(
            &set_enter,
            &if has_submenu {
                JsValue::from_str(&enter_id)
            } else {
                JsValue::NULL
            },
        )
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let set_leave = context.set_open_submenu.clone();
    let mouse_leave = Closure::wrap(Box::new(move || set_state(&set_leave, &JsValue::NULL))
        as Box<dyn FnMut() -> Result<(), JsValue>>);
    let set_focus = context.set_open_submenu.clone();
    let focus_id = id.clone();
    let focus = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        set_state(
            &set_focus,
            &if has_submenu {
                JsValue::from_str(&focus_id)
            } else {
                JsValue::NULL
            },
        )
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let set_click = context.set_open_submenu.clone();
    let on_select = context.on_select.clone();
    let click_id = id.clone();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if has_submenu {
            set_state(&set_click, &JsValue::from_str(&click_id))?;
        } else {
            on_select.call1(&JsValue::UNDEFINED, &JsValue::from_str(&click_id))?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let mut button_classes = vec!["seekdeep-primitive-menu-item"];
    if selected {
        button_classes.push("seekdeep-primitive-menu-selected");
    }
    if property_truthy(entry, "danger")? {
        button_classes.push("seekdeep-primitive-menu-danger");
    }
    let mut button_children = Vec::new();
    let icon = Reflect::get(entry, &JsValue::from_str("icon"))?;
    if !icon.is_null() && !icon.is_undefined() {
        button_children.push(create_element(
            react,
            &JsValue::from_str("span"),
            Some(&class_props("seekdeep-primitive-menu-itemIcon")?),
            &[icon],
        )?);
    }
    button_children.push(create_element(
        react,
        &JsValue::from_str("span"),
        Some(&class_props("seekdeep-primitive-menu-itemLabel")?),
        &[label],
    )?);
    if selected {
        button_children.push(check_icon(react)?);
    }
    let button = create_element(
        react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("role", JsValue::from_str("menuitem")),
            ("className", JsValue::from_str(&button_classes.join(" "))),
            (
                "disabled",
                JsValue::from_bool(property_truthy(entry, "disabled")?),
            ),
            (
                "aria-haspopup",
                if has_submenu {
                    JsValue::from_str("menu")
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-expanded",
                if has_submenu {
                    JsValue::from_bool(submenu_open)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("onFocus", focus.into_js_value()),
            ("onClick", click.into_js_value()),
        ])?),
        &button_children,
    )?;
    let mut children = vec![button];
    if submenu_open {
        let submenu = submenu.expect("non-empty submenu must be present");
        let mut submenu_nodes = Vec::new();
        for child in submenu.iter() {
            let child_id = required_string(&child, "id", "Menu submenu item")?;
            let child_select = context.on_select.clone();
            let selected_id = child_id.clone();
            let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                child_select.call1(&JsValue::UNDEFINED, &JsValue::from_str(&selected_id))?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            let mut child_content = Vec::new();
            let icon = Reflect::get(&child, &JsValue::from_str("icon"))?;
            if !icon.is_null() && !icon.is_undefined() {
                child_content.push(create_element(
                    react,
                    &JsValue::from_str("span"),
                    Some(&class_props("seekdeep-primitive-menu-itemIcon")?),
                    &[icon],
                )?);
            }
            child_content.push(create_element(
                react,
                &JsValue::from_str("span"),
                Some(&class_props("seekdeep-primitive-menu-itemLabel")?),
                &[required_property(&child, "label", "Menu submenu item")?],
            )?);
            submenu_nodes.push(create_element(
                react,
                &JsValue::from_str("button"),
                Some(&object(&[
                    ("key", JsValue::from_str(&child_id)),
                    ("type", JsValue::from_str("button")),
                    ("role", JsValue::from_str("menuitem")),
                    (
                        "className",
                        JsValue::from_str("seekdeep-primitive-menu-item"),
                    ),
                    (
                        "disabled",
                        JsValue::from_bool(property_truthy(&child, "disabled")?),
                    ),
                    ("onClick", on_click.into_js_value()),
                ])?),
                &child_content,
            )?);
        }
        let class_name = if context.compact {
            "seekdeep-primitive-menu-submenu seekdeep-primitive-menu-compactList"
        } else {
            "seekdeep-primitive-menu-submenu"
        };
        children.push(create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("className", JsValue::from_str(class_name)),
                ("role", JsValue::from_str("menu")),
            ])?),
            &submenu_nodes,
        )?);
    }
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("key", JsValue::from_str(&id)),
            (
                "className",
                JsValue::from_str("seekdeep-primitive-menu-itemWrap"),
            ),
            ("onMouseEnter", mouse_enter.into_js_value()),
            ("onMouseLeave", mouse_leave.into_js_value()),
        ])?),
        &children,
    )
}

fn check_icon(react: &JsValue) -> Result<JsValue, JsValue> {
    let definition = ICON_DEFINITIONS
        .iter()
        .find(|definition| definition.name == "IconCheckOutline16")
        .ok_or_else(|| js_sys::Error::new("IconCheckOutline16 is missing from the catalog"))?;
    render_icon(
        react,
        *definition,
        &class_props("seekdeep-primitive-menu-check")?.into(),
    )
}

fn node_contains(container: &JsValue, target: &JsValue) -> bool {
    container
        .dyn_ref::<web_sys::Node>()
        .zip(target.dyn_ref::<web_sys::Node>())
        .is_some_and(|(container, target)| container.contains(Some(target)))
}

fn is_separator(entry: &JsValue) -> bool {
    Reflect::get(entry, &JsValue::from_str("type"))
        .ok()
        .and_then(|value| value.as_string())
        .as_deref()
        == Some("separator")
}

fn is_label(entry: &JsValue) -> bool {
    Reflect::get(entry, &JsValue::from_str("type"))
        .ok()
        .and_then(|value| value.as_string())
        .as_deref()
        == Some("label")
}

fn inject_style() -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = "@seekdeep-ai/seekdeep-client-ui-primitives/Menu.module.css";
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        && !query
            .call1(
                &document,
                &JsValue::from_str(&format!("style[data-plugin-css=\"{tag}\"]")),
            )?
            .is_null()
    {
        return Ok(());
    }
    let replacements = [
        ("compactList", "seekdeep-primitive-menu-compactList"),
        ("denseList", "seekdeep-primitive-menu-denseList"),
        ("scrollable", "seekdeep-primitive-menu-scrollable"),
        ("separator", "seekdeep-primitive-menu-separator"),
        ("itemWrap", "seekdeep-primitive-menu-itemWrap"),
        ("itemIcon", "seekdeep-primitive-menu-itemIcon"),
        ("itemLabel", "seekdeep-primitive-menu-itemLabel"),
        ("selected", "seekdeep-primitive-menu-selected"),
        ("submenu", "seekdeep-primitive-menu-submenu"),
        ("viewport", "seekdeep-primitive-menu-viewport"),
        ("alignEnd", "seekdeep-primitive-menu-alignEnd"),
        ("sideTop", "seekdeep-primitive-menu-sideTop"),
        ("footer", "seekdeep-primitive-menu-footer"),
        ("danger", "seekdeep-primitive-menu-danger"),
        ("portal", "seekdeep-primitive-menu-portal"),
        ("label", "seekdeep-primitive-menu-label"),
        ("check", "seekdeep-primitive-menu-check"),
        ("root", "seekdeep-primitive-menu-root"),
        ("list", "seekdeep-primitive-menu-list"),
        ("item", "seekdeep-primitive-menu-item"),
    ];
    let mut css = MENU_CSS.to_owned();
    for (source, target) in replacements {
        css = css.replace(&format!(".{source}"), &format!(".{target}"));
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin-css"), JsValue::from_str(tag)],
    )?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin"),
            JsValue::from_str("@seekdeep-ai/seekdeep-client-ui-primitives"),
        ],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(&css),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives Menu module was not configured").into()
        })
    })
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
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

fn use_layout_effect(
    react: &JsValue,
    effect: &JsValue,
    dependencies: &Array,
) -> Result<(), JsValue> {
    required_function(react, "useLayoutEffect", "React")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn optional_array(value: &JsValue, key: &str) -> Result<Option<Array>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else if Array::is_array(&property) {
        Ok(Some(Array::from(&property)))
    } else {
        Err(js_sys::TypeError::new(&format!("{key} must be an array")).into())
    }
}

fn required_array(value: &JsValue, key: &str, owner: &str) -> Result<Array, JsValue> {
    optional_array(value, key)?
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} omitted {key}")).into())
}

fn optional_function(value: &JsValue, key: &str) -> Result<Option<Function>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property.dyn_into().map(Some)
    }
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property
            .as_string()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a string")).into())
    }
}

fn property_truthy(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    Ok(Reflect::get(value, &JsValue::from_str(key))?.is_truthy())
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
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

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
