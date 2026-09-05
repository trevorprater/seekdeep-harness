//! Browser WASM facade, Cordis assembly, theme projection, and live `AppFrame`.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    DARK_ATTRIBUTE, DETAILS_DEFAULT, DETAILS_MAX, DETAILS_MIN, INJECT, LAYOUT_STYLES, LayoutState,
    SIDEBAR_AUTO_COLLAPSE, SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN, ThemeTokenLedger,
    clamp_width, compute_columns,
};

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    runtime: JsValue,
}

/// JavaScript-facing cross-plugin panel controller.
#[wasm_bindgen(js_name = LayoutController)]
pub struct WasmLayoutController {
    panels: Rc<RefCell<Option<JsValue>>>,
}

#[wasm_bindgen(js_class = LayoutController)]
impl WasmLayoutController {
    /// Creates an unwired panel face.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            panels: Rc::new(RefCell::new(None)),
        }
    }

    /// Adopts a root entry's bound store actions.
    #[wasm_bindgen(js_name = attachPanels)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn attach_panels(&self, actions: JsValue) {
        *self.panels.borrow_mut() = Some(actions);
    }

    /// Toggles the sidebar or fails loud before root-entry wiring.
    ///
    /// # Errors
    ///
    /// Returns the source boot-order diagnostic or delegated action failure.
    #[wasm_bindgen(js_name = toggleSidebar)]
    pub fn toggle_sidebar(&self) -> Result<(), JsValue> {
        self.dispatch("toggleSidebar")
    }

    /// Opens details or fails loud before root-entry wiring.
    ///
    /// # Errors
    ///
    /// Returns the source boot-order diagnostic or delegated action failure.
    #[wasm_bindgen(js_name = openDetails)]
    pub fn open_details(&self) -> Result<(), JsValue> {
        self.dispatch("openDetails")
    }

    /// Closes details or fails loud before root-entry wiring.
    ///
    /// # Errors
    ///
    /// Returns the source boot-order diagnostic or delegated action failure.
    #[wasm_bindgen(js_name = closeDetails)]
    pub fn close_details(&self) -> Result<(), JsValue> {
        self.dispatch("closeDetails")
    }
}

impl Default for WasmLayoutController {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmLayoutController {
    fn from_shared(panels: Rc<RefCell<Option<JsValue>>>) -> Self {
        Self { panels }
    }

    fn dispatch(&self, method: &str) -> Result<(), JsValue> {
        let panels =
            self.panels.borrow().clone().ok_or_else(|| {
                js_error("layout: panel actions not wired (root entry not mounted)")
            })?;
        call_method(&panels, method, &[])?;
        Ok(())
    }
}

/// Configures React and the compiled client-runtime Store engine.
///
/// # Errors
///
/// Returns DOM style-injection failures.
#[wasm_bindgen(js_name = configureClientUiLayout)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_layout(react: JsValue, runtime: JsValue) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules { react, runtime });
    });
    inject_styles()
}

/// Browser Client plugin apply function.
///
/// # Errors
///
/// Returns missing-service, registration, store, theme, React, or DOM failures.
#[wasm_bindgen(js_name = applyClientUiLayout)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_layout(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required_service(&ctx, "slots")?;
    let theme = required_service(&ctx, "theme")?;
    let store = create_layout_store(&modules.runtime)?;
    let component = app_frame_component(&modules.react);

    let panels = Rc::new(RefCell::new(None));
    let layout: JsValue = WasmLayoutController::from_shared(panels.clone()).into();
    let inject = Closure::wrap(
        Box::new(move |actions: JsValue| -> Result<JsValue, JsValue> {
            *panels.borrow_mut() = Some(actions);
            Ok(Object::new().into())
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    let children = Object::new();
    for (name, kind, scope) in [
        ("sidebar", "single", "root"),
        ("conversation", "single", "session-maybe"),
        ("details", "single", "session"),
        ("shell.overlay", "list", "root"),
    ] {
        set(
            &children,
            name,
            &object(&[
                ("kind", JsValue::from_str(kind)),
                ("scope", JsValue::from_str(scope)),
            ])?
            .into(),
        )?;
    }
    let options = object(&[
        ("name", JsValue::from_str("root")),
        ("children", children.into()),
        ("store", store),
        ("inject", inject.into_js_value()),
    ])?;

    own_layout_registration(&ctx, &slots, options, component, layout)?;
    own_theme_presentation(&ctx, &theme)?;
    Ok(())
}

/// Exact Client plugin inject list.
#[wasm_bindgen(js_name = layoutInject)]
pub fn layout_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

fn own_layout_registration(
    ctx: &JsValue,
    slots: &JsValue,
    options: Object,
    component: JsValue,
    layout: JsValue,
) -> Result<(), JsValue> {
    let caller = ctx.clone();
    let slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let reflect = required_property(&caller, "reflect", "Client Context")?;
        let dispose_service = call_method(
            &reflect,
            "provide",
            &[JsValue::from_str("layout"), layout.clone()],
        )?;
        let dispose_registration = match call_method(
            &slots,
            "register",
            &[options.clone().into(), component.clone()],
        ) {
            Ok(disposer) => disposer,
            Err(error) => {
                call_disposer(&dispose_service);
                return Err(error);
            }
        };
        let cleanup = Closure::wrap(Box::new(move || {
            call_disposer(&dispose_registration);
            call_disposer(&dispose_service);
        }) as Box<dyn FnMut()>);
        Ok(cleanup.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-layout: service + root registration"),
        ],
    )?;
    Ok(())
}

fn create_layout_store(runtime: &JsValue) -> Result<JsValue, JsValue> {
    let init = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        layout_state_object(LayoutState::default()).map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let actions = Object::new();

    let set_sidebar = Closure::wrap(Box::new(move |draft: JsValue, px: f64| {
        set_value(
            &draft,
            "sidebar",
            &JsValue::from_f64(clamp_width(px, SIDEBAR_MIN, SIDEBAR_MAX)),
        )
    })
        as Box<dyn FnMut(JsValue, f64) -> Result<(), JsValue>>);
    set(&actions, "setSidebar", &set_sidebar.into_js_value())?;

    let set_details = Closure::wrap(Box::new(move |draft: JsValue, px: f64| {
        set_value(
            &draft,
            "details",
            &JsValue::from_f64(clamp_width(px, DETAILS_MIN, DETAILS_MAX)),
        )
    })
        as Box<dyn FnMut(JsValue, f64) -> Result<(), JsValue>>);
    set(&actions, "setDetails", &set_details.into_js_value())?;

    let toggle_sidebar = Closure::wrap(Box::new(move |draft: JsValue| {
        if required_bool(&draft, "narrow")? {
            let expanded = required_bool(&draft, "narrowExpanded")?;
            set_value(&draft, "narrowExpanded", &JsValue::from_bool(!expanded))
        } else {
            let sidebar = required_number(&draft, "sidebar")?;
            set_value(
                &draft,
                "sidebar",
                &JsValue::from_f64(if sidebar == 0.0 { SIDEBAR_DEFAULT } else { 0.0 }),
            )
        }
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&actions, "toggleSidebar", &toggle_sidebar.into_js_value())?;

    let set_narrow = Closure::wrap(Box::new(move |draft: JsValue, narrow: bool| {
        if required_bool(&draft, "narrow")? == narrow {
            return Ok(());
        }
        set_value(&draft, "narrow", &JsValue::from_bool(narrow))?;
        set_value(&draft, "narrowExpanded", &JsValue::FALSE)
    })
        as Box<dyn FnMut(JsValue, bool) -> Result<(), JsValue>>);
    set(&actions, "setNarrow", &set_narrow.into_js_value())?;

    let open_details = Closure::wrap(Box::new(move |draft: JsValue| {
        if required_number(&draft, "details")? == 0.0 {
            set_value(&draft, "details", &JsValue::from_f64(DETAILS_DEFAULT))?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&actions, "openDetails", &open_details.into_js_value())?;

    let close_details = Closure::wrap(Box::new(move |draft: JsValue| {
        set_value(&draft, "details", &JsValue::from_f64(0.0))
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&actions, "closeDetails", &close_details.into_js_value())?;

    let declaration = object(&[("init", init.into_js_value()), ("actions", actions.into())])?;
    call_method(runtime, "defineStore", &[declaration.into()])
}

fn layout_state_object(state: LayoutState) -> Result<Object, JsValue> {
    object(&[
        ("sidebar", JsValue::from_f64(state.sidebar)),
        ("details", JsValue::from_f64(state.details)),
        ("narrow", JsValue::from_bool(state.narrow)),
        ("narrowExpanded", JsValue::from_bool(state.narrow_expanded)),
    ])
}

struct BrowserThemePresenter {
    applied: ThemeTokenLedger,
    meta: JsValue,
}

impl BrowserThemePresenter {
    fn new() -> Result<Self, JsValue> {
        let document = required_property(&js_sys::global(), "document", "global")?;
        let meta = call_method(&document, "createElement", &[JsValue::from_str("meta")])?;
        set_value(&meta, "name", &JsValue::from_str("theme-color"))?;
        Ok(Self {
            applied: ThemeTokenLedger::default(),
            meta,
        })
    }

    fn apply(&mut self, snapshot: &JsValue) -> Result<(), JsValue> {
        let active = required_property(snapshot, "active", "Theme snapshot")?;
        let scheme = required_property(&active, "colorScheme", "active Theme")?
            .as_string()
            .ok_or_else(|| js_error("ui-layout: active Theme colorScheme must be a string"))?;
        let tokens = required_property(&active, "tokens", "active Theme")?;
        let document = required_property(&js_sys::global(), "document", "global")?;
        let root = required_property(&document, "documentElement", "document")?;
        let root_style = required_property(&root, "style", "documentElement")?;
        set_value(&root_style, "colorScheme", &JsValue::from_str(&scheme))?;
        let body = required_property(&document, "body", "document")?;
        if scheme == "dark" {
            call_method(
                &body,
                "setAttribute",
                &[JsValue::from_str(DARK_ATTRIBUTE), JsValue::from_str("")],
            )?;
        } else {
            call_method(
                &body,
                "removeAttribute",
                &[JsValue::from_str(DARK_ATTRIBUTE)],
            )?;
        }
        let body_style = required_property(&body, "style", "body")?;
        let entries = Object::entries(&Object::from(tokens));
        let names = entries
            .iter()
            .filter_map(|entry| Array::from(&entry).get(0).as_string())
            .collect::<Vec<_>>();
        for name in self.applied.replace(names) {
            call_method(&body_style, "removeProperty", &[JsValue::from_str(&name)])?;
        }
        for entry in entries.iter() {
            let entry = Array::from(&entry);
            call_method(&body_style, "setProperty", &[entry.get(0), entry.get(1)])?;
        }
        let get_computed_style = function(&js_sys::global(), "getComputedStyle")?;
        let computed = get_computed_style.call1(&js_sys::global(), &body)?;
        let background = required_property(&computed, "backgroundColor", "computed body style")?;
        set_value(&self.meta, "content", &background)?;
        let connected = Reflect::get(&self.meta, &JsValue::from_str("isConnected"))?
            .as_bool()
            .unwrap_or(false);
        if !connected {
            let head = required_property(&document, "head", "document")?;
            call_method(&head, "append", std::slice::from_ref(&self.meta))?;
        }
        Ok(())
    }

    fn dispose(&mut self) -> Result<(), JsValue> {
        let document = required_property(&js_sys::global(), "document", "global")?;
        let root = required_property(&document, "documentElement", "document")?;
        let root_style = required_property(&root, "style", "documentElement")?;
        call_method(
            &root_style,
            "removeProperty",
            &[JsValue::from_str("color-scheme")],
        )?;
        let body = required_property(&document, "body", "document")?;
        call_method(
            &body,
            "removeAttribute",
            &[JsValue::from_str(DARK_ATTRIBUTE)],
        )?;
        let body_style = required_property(&body, "style", "body")?;
        for name in self.applied.drain() {
            call_method(&body_style, "removeProperty", &[JsValue::from_str(&name)])?;
        }
        call_method(&self.meta, "remove", &[])?;
        Ok(())
    }
}

fn own_theme_presentation(ctx: &JsValue, theme: &JsValue) -> Result<(), JsValue> {
    let listener_ctx = ctx.clone();
    let theme = theme.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let presenter = Rc::new(RefCell::new(BrowserThemePresenter::new()?));
        let initial = call_method(&theme, "getTheme", &[])?;
        presenter.borrow_mut().apply(&initial)?;
        let event_presenter = presenter.clone();
        let listener = Closure::wrap(Box::new(move |snapshot: JsValue| {
            event_presenter.borrow_mut().apply(&snapshot)
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let off = match call_method(
            &listener_ctx,
            "on",
            &[JsValue::from_str("theme/change"), listener.into_js_value()],
        ) {
            Ok(off) => off,
            Err(error) => {
                let _ = presenter.borrow_mut().dispose();
                return Err(error);
            }
        };
        let cleanup = Closure::wrap(Box::new(move || {
            call_disposer(&off);
            let _ = presenter.borrow_mut().dispose();
        }) as Box<dyn FnMut()>);
        Ok(cleanup.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-layout: theme presenter"),
        ],
    )?;
    Ok(())
}

#[derive(Clone)]
struct ReactUi {
    react: JsValue,
}

impl ReactUi {
    fn element(
        &self,
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
        function(&self.react, "createElement")?.apply(&self.react, &args)
    }

    fn tag(
        &self,
        name: &str,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        self.element(&JsValue::from_str(name), props, children)
    }
}

fn app_frame_component(react: &JsValue) -> JsValue {
    let ui = ReactUi {
        react: react.clone(),
    };
    let component = Closure::wrap(
        Box::new(move |props: JsValue| render_app_frame(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    component.into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_app_frame(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let panels_selector =
        Closure::wrap(Box::new(move |state: JsValue| state) as Box<dyn FnMut(JsValue) -> JsValue>);
    let panels = function(props, "useStore")?
        .call1(&JsValue::UNDEFINED, &panels_selector.into_js_value())?;
    let session_selector =
        Closure::wrap(
            Box::new(move |state: JsValue| selected_details_session(&state))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
    let details_session = function(props, "useSessions")?
        .call1(&JsValue::UNDEFINED, &session_selector.into_js_value())?;
    let actions = required_property(props, "actions", "AppFrame props")?;
    let frame_ref = use_ref(&ui.react, &JsValue::NULL)?;
    let window = required_property(&js_sys::global(), "window", "global")?;
    let initial_viewport = required_number(&window, "innerWidth")?;
    let (viewport, set_viewport) = use_state(&ui.react, &JsValue::from_f64(initial_viewport))?;
    let viewport = viewport.as_f64().unwrap_or(initial_viewport);

    let last_session = use_ref(&ui.react, &details_session)?;
    let session_actions = actions.clone();
    let session_last = last_session.clone();
    let next_session = details_session.clone();
    let session_effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if next_session.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let previous = ref_value(&session_last)?;
        if !previous.is_undefined() && !Object::is(&previous, &next_session) {
            call_method(&session_actions, "closeDetails", &[])?;
        }
        set_ref(&session_last, &next_session)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_layout_effect(
        &ui.react,
        &session_effect.into_js_value(),
        &[actions.clone(), details_session.clone()],
    )?;
    own_resize_observer(&ui.react, &frame_ref, &set_viewport)?;

    let narrow = viewport < SIDEBAR_AUTO_COLLAPSE;
    let narrow_actions = actions.clone();
    let narrow_effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        call_method(&narrow_actions, "setNarrow", &[JsValue::from_bool(narrow)])?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        &ui.react,
        &narrow_effect.into_js_value(),
        &[actions.clone(), JsValue::from_bool(narrow)],
    )?;

    let sidebar = required_number(&panels, "sidebar")?;
    let details = required_number(&panels, "details")?;
    let narrow_expanded = required_bool(&panels, "narrowExpanded")?;
    let sidebar_collapsed = if narrow {
        !narrow_expanded
    } else {
        sidebar == 0.0
    };
    let sidebar_preference = if sidebar_collapsed {
        0.0
    } else if sidebar == 0.0 {
        SIDEBAR_DEFAULT
    } else {
        sidebar
    };
    let columns = compute_columns(
        viewport,
        sidebar_preference,
        if details_session.is_undefined() {
            0.0
        } else {
            details
        },
    );
    let columns_ref = use_ref(&ui.react, &columns_object(columns)?.into())?;
    set_ref(&columns_ref, &columns_object(columns)?.into())?;
    let sidebar_base = use_ref(&ui.react, &JsValue::from_f64(0.0))?;
    let details_base = use_ref(&ui.react, &JsValue::from_f64(0.0))?;
    let sidebar_drag = use_ref(&ui.react, &drag_state_object()?.into())?;
    let details_drag = use_ref(&ui.react, &drag_state_object()?.into())?;
    let (dragging, set_dragging) = use_state(&ui.react, &JsValue::UNDEFINED)?;
    let dragging_side = dragging.as_string();

    let sidebar_owner = object(&[
        ("collapsed", JsValue::from_bool(sidebar_collapsed)),
        ("width", JsValue::from_f64(columns.sidebar)),
    ])?;
    let sidebar_slot = call_prop(
        props,
        "renderSlot",
        &[JsValue::from_str("sidebar"), sidebar_owner.into()],
    )?;
    let sidebar_column = ui.tag(
        "div",
        Some(&class_props("seekdeep-layout-sidebar-col")?),
        &[sidebar_slot],
    )?;
    let conversation = call_prop(
        props,
        "renderSlot",
        &[JsValue::from_str("conversation"), Object::new().into()],
    )?;
    let center_column = ui.tag(
        "div",
        Some(&class_props("seekdeep-layout-center-col")?),
        &[conversation],
    )?;
    let details_slot = call_prop(
        props,
        "renderSlot",
        &[JsValue::from_str("details"), Object::new().into()],
    )?;
    let details_column = ui.tag(
        "div",
        Some(&class_props("seekdeep-layout-details-col")?),
        &[details_slot],
    )?;
    let fragment = ui.element(
        &required_property(&ui.react, "Fragment", "React")?,
        None,
        &[center_column, details_column],
    )?;
    let overlay = call_prop(
        props,
        "renderSlot",
        &[JsValue::from_str("shell.overlay"), Object::new().into()],
    )?;
    let overlay = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-layout-overlay-layer"),
            ),
            ("data-shell-overlay", JsValue::TRUE),
        ])?),
        &[overlay],
    )?;

    let mut children = vec![sidebar_column, fragment, overlay];
    if !sidebar_collapsed {
        children.push(drag_handle(
            ui,
            "sidebar",
            columns.sidebar,
            &actions,
            &columns_ref,
            &sidebar_base,
            &sidebar_drag,
            &set_dragging,
            dragging_side.as_deref() == Some("sidebar"),
        )?);
    }
    if columns.details > 0.0 {
        children.push(drag_handle(
            ui,
            "details",
            viewport - columns.details,
            &actions,
            &columns_ref,
            &details_base,
            &details_drag,
            &set_dragging,
            dragging_side.as_deref() == Some("details"),
        )?);
    }

    let style = object(&[(
        "gridTemplateColumns",
        JsValue::from_str(&format!(
            "{}px minmax(0, 1fr) {}px",
            number_text(columns.sidebar),
            number_text(columns.details)
        )),
    )])?;
    ui.tag(
        "div",
        Some(&object(&[
            ("ref", frame_ref),
            ("className", JsValue::from_str("seekdeep-layout-frame")),
            ("style", style.into()),
            ("data-sidebar-collapsed", bool_attribute(sidebar_collapsed)),
            (
                "data-details-collapsed",
                bool_attribute(columns.details == 0.0),
            ),
            ("data-dragging", bool_attribute(dragging_side.is_some())),
        ])?),
        &children,
    )
}

fn selected_details_session(state: &JsValue) -> Result<JsValue, JsValue> {
    let current = Reflect::get(state, &JsValue::from_str("current"))?;
    if current.is_undefined() {
        return Ok(JsValue::UNDEFINED);
    }
    let by_id = required_property(state, "byId", "Session list")?;
    let row = Reflect::get(&by_id, &current)?;
    if row.is_undefined() || row.is_null() {
        return Ok(JsValue::UNDEFINED);
    }
    Ok(
        if Reflect::get(&row, &JsValue::from_str("blank"))?.as_bool() == Some(false) {
            current
        } else {
            JsValue::UNDEFINED
        },
    )
}

fn own_resize_observer(
    react: &JsValue,
    frame_ref: &JsValue,
    set_viewport: &Function,
) -> Result<(), JsValue> {
    let frame_ref = frame_ref.clone();
    let setter = set_viewport.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let element = ref_value(&frame_ref)?;
        if element.is_null() || element.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let pending: Rc<RefCell<Option<JsValue>>> = Rc::new(RefCell::new(None));
        let observer_pending = pending.clone();
        let observer_element = element.clone();
        let observer_setter = setter.clone();
        let observer_callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            if observer_pending.borrow().is_some() {
                return Ok(());
            }
            let frame_pending = observer_pending.clone();
            let frame_element = observer_element.clone();
            let frame_setter = observer_setter.clone();
            let frame = Closure::wrap(Box::new(move |_timestamp: JsValue| -> Result<(), JsValue> {
                *frame_pending.borrow_mut() = None;
                let bounds = call_method(&frame_element, "getBoundingClientRect", &[])?;
                let width = required_number(&bounds, "width")?;
                if width > 0.0 {
                    frame_setter.call1(&JsValue::UNDEFINED, &JsValue::from_f64(width))?;
                }
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            let id = call_global("requestAnimationFrame", &[frame.into_js_value()])?;
            *observer_pending.borrow_mut() = Some(id);
            Ok(())
        })
            as Box<dyn FnMut() -> Result<(), JsValue>>);
        let constructor = function(&js_sys::global(), "ResizeObserver")?;
        let args = Array::new();
        args.push(&observer_callback.into_js_value());
        let observer = Reflect::construct(&constructor, &args)?;
        call_method(&observer, "observe", std::slice::from_ref(&element))?;
        let cleanup_pending = pending;
        let cleanup_observer = observer;
        let cleanup = Closure::wrap(Box::new(move || {
            let _ = call_method(&cleanup_observer, "disconnect", &[]);
            if let Some(frame) = cleanup_pending.borrow_mut().take() {
                let _ = call_global("cancelAnimationFrame", &[frame]);
            }
        }) as Box<dyn FnMut()>);
        Ok(cleanup.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(react, &effect.into_js_value(), &[])
}

#[allow(clippy::too_many_arguments)]
fn drag_handle(
    ui: &ReactUi,
    side: &'static str,
    left: f64,
    actions: &JsValue,
    columns_ref: &JsValue,
    base_ref: &JsValue,
    drag_ref: &JsValue,
    set_dragging: &Function,
    active: bool,
) -> Result<JsValue, JsValue> {
    let down_columns = columns_ref.clone();
    let down_base = base_ref.clone();
    let down_drag = drag_ref.clone();
    let down_setter = set_dragging.clone();
    let pointer_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        call_method(&event, "preventDefault", &[])?;
        let target = required_property(&event, "currentTarget", "pointer event")?;
        let pointer_id = required_property(&event, "pointerId", "pointer event")?;
        call_method(&target, "setPointerCapture", &[pointer_id])?;
        let x = required_number(&event, "clientX")?;
        let drag = ref_value(&down_drag)?;
        set_value(&drag, "origin", &JsValue::from_f64(x))?;
        set_value(&drag, "latest", &JsValue::from_f64(x))?;
        set_value(&drag, "frame", &JsValue::UNDEFINED)?;
        let columns = ref_value(&down_columns)?;
        set_ref(&down_base, &required_property(&columns, side, "columns")?)?;
        down_setter.call1(&JsValue::UNDEFINED, &JsValue::from_str(side))?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let move_actions = actions.clone();
    let move_base = base_ref.clone();
    let move_drag = drag_ref.clone();
    let pointer_move = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let target = required_property(&event, "currentTarget", "pointer event")?;
        let pointer_id = required_property(&event, "pointerId", "pointer event")?;
        if call_method(&target, "hasPointerCapture", &[pointer_id])?.as_bool() != Some(true) {
            return Ok(());
        }
        let drag = ref_value(&move_drag)?;
        set_value(
            &drag,
            "latest",
            &JsValue::from_f64(required_number(&event, "clientX")?),
        )?;
        if !Reflect::get(&drag, &JsValue::from_str("frame"))?.is_undefined() {
            return Ok(());
        }
        let frame_drag = move_drag.clone();
        let frame_base = move_base.clone();
        let frame_actions = move_actions.clone();
        let frame = Closure::wrap(Box::new(move |_timestamp: JsValue| -> Result<(), JsValue> {
            let drag = ref_value(&frame_drag)?;
            set_value(&drag, "frame", &JsValue::UNDEFINED)?;
            dispatch_drag(side, &frame_actions, &frame_base, &drag)
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let id = call_global("requestAnimationFrame", &[frame.into_js_value()])?;
        set_value(&drag, "frame", &id)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let up_actions = actions.clone();
    let up_base = base_ref.clone();
    let up_drag = drag_ref.clone();
    let up_setter = set_dragging.clone();
    let pointer_up = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let target = required_property(&event, "currentTarget", "pointer event")?;
        let pointer_id = required_property(&event, "pointerId", "pointer event")?;
        if call_method(
            &target,
            "hasPointerCapture",
            std::slice::from_ref(&pointer_id),
        )?
        .as_bool()
            != Some(true)
        {
            return Ok(());
        }
        call_method(&target, "releasePointerCapture", &[pointer_id])?;
        let drag = ref_value(&up_drag)?;
        let frame = Reflect::get(&drag, &JsValue::from_str("frame"))?;
        if !frame.is_undefined() {
            call_global("cancelAnimationFrame", std::slice::from_ref(&frame))?;
            set_value(&drag, "frame", &JsValue::UNDEFINED)?;
        }
        dispatch_drag(side, &up_actions, &up_base, &drag)?;
        up_setter.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let style = object(&[("left", JsValue::from_f64(left))])?;
    ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-layout-handle")),
            ("style", style.into()),
            ("data-side", JsValue::from_str(side)),
            ("data-dragging", bool_attribute(active)),
            ("onPointerDown", pointer_down.into_js_value()),
            ("onPointerMove", pointer_move.into_js_value()),
            ("onPointerUp", pointer_up.into_js_value()),
        ])?),
        &[],
    )
}

fn dispatch_drag(
    side: &str,
    actions: &JsValue,
    base_ref: &JsValue,
    drag: &JsValue,
) -> Result<(), JsValue> {
    let delta = required_number(drag, "latest")? - required_number(drag, "origin")?;
    let base = ref_value(base_ref)?
        .as_f64()
        .ok_or_else(|| js_error("ui-layout: drag base must be a number"))?;
    let (method, width) = if side == "details" {
        ("setDetails", base - delta)
    } else {
        ("setSidebar", base + delta)
    };
    call_method(actions, method, &[JsValue::from_f64(width)])?;
    Ok(())
}

fn drag_state_object() -> Result<Object, JsValue> {
    object(&[
        ("origin", JsValue::from_f64(0.0)),
        ("latest", JsValue::from_f64(0.0)),
        ("frame", JsValue::UNDEFINED),
    ])
}

fn columns_object(columns: crate::Columns) -> Result<Object, JsValue> {
    object(&[
        ("sidebar", JsValue::from_f64(columns.sidebar)),
        ("center", JsValue::from_f64(columns.center)),
        ("details", JsValue::from_f64(columns.details)),
    ])
}

fn inject_styles() -> Result<(), JsValue> {
    let document = required_property(&js_sys::global(), "document", "global")?;
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin"),
            JsValue::from_str("@seekdeep-ai/seekdeep-client-ui-layout"),
        ],
    )?;
    set_value(&style, "textContent", &JsValue::from_str(LAYOUT_STYLES))?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_error("client-ui-layout module factory did not configure React and runtime")
        })
    })
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let state = Array::from(&function(react, "useState")?.call1(react, initial)?);
    Ok((state.get(0), state.get(1).dyn_into::<Function>()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    function(react, "useRef")?.call1(react, initial)
}

fn use_effect(react: &JsValue, effect: &JsValue, dependencies: &[JsValue]) -> Result<(), JsValue> {
    invoke_effect(react, "useEffect", effect, dependencies)
}

fn use_layout_effect(
    react: &JsValue,
    effect: &JsValue,
    dependencies: &[JsValue],
) -> Result<(), JsValue> {
    invoke_effect(react, "useLayoutEffect", effect, dependencies)
}

fn invoke_effect(
    react: &JsValue,
    name: &str,
    effect: &JsValue,
    dependencies: &[JsValue],
) -> Result<(), JsValue> {
    let deps = Array::new();
    for dependency in dependencies {
        deps.push(dependency);
    }
    function(react, name)?.call2(react, effect, &deps)?;
    Ok(())
}

fn ref_value(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_ref(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn call_prop(value: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = function(value, name)?;
    let arguments = Array::new();
    for argument in args {
        arguments.push(argument);
    }
    function.apply(&JsValue::UNDEFINED, &arguments)
}

fn call_global(name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let global = js_sys::global();
    let function = function(&global, name)?;
    let arguments = Array::new();
    for argument in args {
        arguments.push(argument);
    }
    function.apply(&global, &arguments)
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() {
        Err(js_error(&format!(
            "client-ui-layout requires Client Service {name:?}"
        )))
    } else {
        Ok(service)
    }
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_error(&format!(
            "ui-layout: {owner} omitted required property {key:?}"
        )))
    } else {
        Ok(property)
    }
}

fn required_number(value: &JsValue, key: &str) -> Result<f64, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_f64()
        .ok_or_else(|| js_error(&format!("ui-layout: missing number {key:?}")))
}

fn required_bool(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_bool()
        .ok_or_else(|| js_error(&format!("ui-layout: missing boolean {key:?}")))
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        set(&object, key, value)?;
    }
    Ok(object)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value).map(|_| ())
}

fn set_value(object: &JsValue, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value).map(|_| ())
}

fn function(value: &JsValue, name: &str) -> Result<Function, JsValue> {
    required_property(value, name, "JavaScript object")?.dyn_into::<Function>()
}

fn call_method(value: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = function(value, name)?;
    let arguments = Array::new();
    for argument in args {
        arguments.push(argument);
    }
    method.apply(value, &arguments)
}

fn call_disposer(value: &JsValue) {
    if let Ok(function) = value.clone().dyn_into::<Function>() {
        let _ = function.call0(&JsValue::UNDEFINED);
    }
}

fn bool_attribute(value: bool) -> JsValue {
    if value {
        JsValue::TRUE
    } else {
        JsValue::UNDEFINED
    }
}

fn number_text(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}
