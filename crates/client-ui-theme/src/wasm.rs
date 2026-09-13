//! Browser WASM facade for the theme service and Appearance surface.

use std::{cell::RefCell, rc::Rc};

use indexmap::IndexMap;
use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    APPEARANCE_STYLES, ColorScheme, INJECT, SETTINGS_NS, THEME_EN, THEME_PREFERENCE_FIELD,
    THEME_SETTINGS_NAMESPACE, THEME_ZH, ThemeDefinition, ThemeId, ThemeOverrideSource,
    ThemeOverrideToken, ThemePreference, ThemeRegistrationToken, ThemeRegistry, ThemeRegistryError,
    ThemeSnapshot, ThemeTokenModes, ThemeTokenOverrides,
};

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    primitives: JsValue,
    runtime: JsValue,
}

struct BrowserThemeRuntime {
    registry: ThemeRegistry,
    ctx: JsValue,
    host: JsValue,
    snapshot_cache: Option<(Rc<ThemeSnapshot>, JsValue)>,
}

impl BrowserThemeRuntime {
    fn snapshot_value(&mut self) -> Result<JsValue, JsValue> {
        let snapshot = self.registry.snapshot();
        if let Some((current, value)) = &self.snapshot_cache
            && Rc::ptr_eq(current, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = theme_snapshot_to_js(&snapshot)?;
        self.snapshot_cache = Some((snapshot, value.clone()));
        Ok(value)
    }
}

/// JavaScript-facing DOM-free theme registry and preference owner.
#[wasm_bindgen(js_name = ThemeRuntime)]
pub struct WasmThemeRuntime {
    inner: Rc<RefCell<BrowserThemeRuntime>>,
}

#[wasm_bindgen(js_class = ThemeRuntime)]
impl WasmThemeRuntime {
    /// Constructs the registry and owns its media/settings listeners through `ctx.effect`.
    ///
    /// # Errors
    ///
    /// Returns malformed context, settings-scope, or media-query faces.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(ctx: JsValue, host: JsValue) -> Result<WasmThemeRuntime, JsValue> {
        Self::construct(&ctx, &host)
    }

    /// Returns the stable immutable snapshot until the next publish.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(js_name = getTheme)]
    pub fn get_theme(&self) -> Result<JsValue, JsValue> {
        self.inner.borrow_mut().snapshot_value()
    }

    /// Exports a sorted defensive inspection directory.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(js_name = exportInspectTokens)]
    pub fn export_inspect_tokens(&self) -> Result<Array, JsValue> {
        let output = Array::new();
        for token in self.inner.borrow().registry.export_inspect_tokens() {
            let row = object(&[
                ("name", JsValue::from_str(&token.name)),
                ("description", JsValue::from_str(&token.description)),
                ("valueType", JsValue::from_str(&token.value_type)),
                (
                    "requiresLightAndDark",
                    JsValue::from_bool(token.requires_light_and_dark),
                ),
            ])?;
            if let Some(css_variable) = token.css_variable {
                set(&row, "cssVariable", &JsValue::from_str(&css_variable))?;
            }
            output.push(&row);
        }
        Ok(output)
    }

    /// Switches to a registered theme or system preference.
    ///
    /// # Errors
    ///
    /// Rejects unknown IDs or Host-scope write failures.
    #[wasm_bindgen(js_name = setTheme)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn set_theme(&self, id: String) -> Result<(), JsValue> {
        let (persist, host) = {
            let mut inner = self.inner.borrow_mut();
            let Some(mutation) = inner
                .registry
                .set_theme(crate::ThemeSelection::new(id))
                .map_err(registry_error)?
            else {
                return Ok(());
            };
            inner.snapshot_cache = None;
            (mutation.persist, inner.host.clone())
        };
        if let Some(preference) = persist {
            call_method(
                &host,
                "set",
                &[
                    JsValue::from_str(THEME_PREFERENCE_FIELD),
                    JsValue::from_str(preference.as_str()),
                ],
            )?;
        }
        emit_inner(&self.inner)
    }

    /// Registers a concrete theme and returns an exact-generation disposer.
    ///
    /// # Errors
    ///
    /// Rejects malformed, duplicate, or reserved definitions.
    #[allow(clippy::needless_pass_by_value)]
    pub fn register(&self, definition: JsValue) -> Result<Function, JsValue> {
        let definition = parse_definition(&definition)?;
        let token = {
            let mut inner = self.inner.borrow_mut();
            let (token, _) = inner
                .registry
                .register(definition)
                .map_err(registry_error)?;
            inner.snapshot_cache = None;
            token
        };
        emit_inner(&self.inner)?;
        Ok(registration_disposer(self.inner.clone(), token))
    }

    /// Installs/replaces one source-owned token override layer.
    ///
    /// # Errors
    ///
    /// Rejects a bare value or a layer omitting either palette mode.
    #[wasm_bindgen(js_name = overrideTokens)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn override_tokens(&self, source: String, tokens: JsValue) -> Result<Function, JsValue> {
        let tokens = parse_overrides(&source, &tokens)?;
        let source = ThemeOverrideSource::new(source);
        let token = {
            let mut inner = self.inner.borrow_mut();
            let (token, _) = inner.registry.override_tokens(source.clone(), tokens);
            inner.snapshot_cache = None;
            token
        };
        emit_inner(&self.inner)?;
        Ok(override_disposer(self.inner.clone(), source, token))
    }
}

impl WasmThemeRuntime {
    fn construct(ctx: &JsValue, host: &JsValue) -> Result<Self, JsValue> {
        let media = optional_global_function("matchMedia")?
            .map(|match_media| {
                match_media.call1(
                    &js_sys::global(),
                    &JsValue::from_str("(prefers-color-scheme: dark)"),
                )
            })
            .transpose()?;
        let system_dark = media
            .as_ref()
            .and_then(|media| Reflect::get(media, &JsValue::from_str("matches")).ok())
            .and_then(|matches| matches.as_bool())
            .unwrap_or(false);
        let inner = Rc::new(RefCell::new(BrowserThemeRuntime {
            registry: ThemeRegistry::new(system_dark),
            ctx: ctx.clone(),
            host: host.clone(),
            snapshot_cache: None,
        }));

        if let Some(media) = media {
            own_media_listener(ctx, &media, inner.clone())?;
        }
        own_scope_adoption(ctx, host, inner.clone())?;
        adopt_inner(&inner)?;
        Ok(Self { inner })
    }

    fn from_inner(inner: Rc<RefCell<BrowserThemeRuntime>>) -> Self {
        Self { inner }
    }
}

fn own_media_listener(
    ctx: &JsValue,
    media: &JsValue,
    inner: Rc<RefCell<BrowserThemeRuntime>>,
) -> Result<(), JsValue> {
    let media = media.clone();
    let listener_media = media.clone();
    let listener = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let dark = Reflect::get(&listener_media, &JsValue::from_str("matches"))?
            .as_bool()
            .unwrap_or(false);
        let changed = {
            let mut inner = inner.borrow_mut();
            let changed = inner.registry.set_system_dark(dark).is_some();
            if changed {
                inner.snapshot_cache = None;
            }
            changed
        };
        if changed {
            emit_inner(&inner)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let listener = listener.into_js_value();
    let installer_media = media;
    let installer_listener = listener;
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        call_method(
            &installer_media,
            "addEventListener",
            &[JsValue::from_str("change"), installer_listener.clone()],
        )?;
        let cleanup_media = installer_media.clone();
        let cleanup_listener = installer_listener.clone();
        let cleanup = Closure::wrap(Box::new(move || {
            let _ = call_method(
                &cleanup_media,
                "removeEventListener",
                &[JsValue::from_str("change"), cleanup_listener.clone()],
            );
        }) as Box<dyn FnMut()>);
        Ok(cleanup.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-theme: prefers-color-scheme listener"),
        ],
    )?;
    Ok(())
}

fn own_scope_adoption(
    ctx: &JsValue,
    host: &JsValue,
    inner: Rc<RefCell<BrowserThemeRuntime>>,
) -> Result<(), JsValue> {
    let host = host.clone();
    let listener = Closure::wrap(
        Box::new(move || adopt_inner(&inner)) as Box<dyn FnMut() -> Result<(), JsValue>>
    );
    let listener = listener.into_js_value();
    let installer = Closure::wrap(Box::new(move || {
        call_method(&host, "subscribe", std::slice::from_ref(&listener))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-theme: settings scope adoption"),
        ],
    )?;
    Ok(())
}

fn adopt_inner(inner: &Rc<RefCell<BrowserThemeRuntime>>) -> Result<(), JsValue> {
    let host = inner.borrow().host.clone();
    let snapshot = call_method(&host, "getSnapshot", &[])?;
    let value = Reflect::get(&snapshot, &JsValue::from_str("value"))?;
    if value.is_undefined() || value.is_null() {
        return Ok(());
    }
    let preference = Reflect::get(&value, &JsValue::from_str(THEME_PREFERENCE_FIELD))?
        .as_string()
        .and_then(|value| ThemePreference::parse(&value));
    let changed = {
        let mut inner = inner.borrow_mut();
        let changed = preference
            .and_then(|preference| inner.registry.adopt(preference))
            .is_some();
        if changed {
            inner.snapshot_cache = None;
        }
        changed
    };
    if changed {
        emit_inner(inner)?;
    }
    Ok(())
}

fn emit_inner(inner: &Rc<RefCell<BrowserThemeRuntime>>) -> Result<(), JsValue> {
    let (ctx, snapshot) = {
        let mut inner = inner.borrow_mut();
        (inner.ctx.clone(), inner.snapshot_value()?)
    };
    call_method(&ctx, "emit", &[JsValue::from_str("theme/change"), snapshot])?;
    Ok(())
}

fn registration_disposer(
    inner: Rc<RefCell<BrowserThemeRuntime>>,
    token: ThemeRegistrationToken,
) -> Function {
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let changed = {
            let mut inner = inner.borrow_mut();
            let changed = inner.registry.dispose_registration(token).is_some();
            if changed {
                inner.snapshot_cache = None;
            }
            changed
        };
        if changed {
            emit_inner(&inner)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .unchecked_into()
}

fn override_disposer(
    inner: Rc<RefCell<BrowserThemeRuntime>>,
    source: ThemeOverrideSource,
    token: ThemeOverrideToken,
) -> Function {
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let changed = {
            let mut inner = inner.borrow_mut();
            let changed = inner.registry.dispose_override(&source, token).is_some();
            if changed {
                inner.snapshot_cache = None;
            }
            changed
        };
        if changed {
            emit_inner(&inner)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .unchecked_into()
}

/// Configures React, icons, and the compiled Store engine.
///
/// # Errors
///
/// Returns DOM style-injection failures.
#[wasm_bindgen(js_name = configureClientUiTheme)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_theme(
    react: JsValue,
    primitives: JsValue,
    runtime: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            primitives,
            runtime,
        });
    });
    inject_styles()
}

/// Browser Client plugin apply function.
///
/// # Errors
///
/// Returns missing-service, scope, locale, Slot, Store, or React failures.
#[wasm_bindgen(js_name = applyClientUiTheme)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_theme(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required_service(&ctx, "slots")?;
    let locale = required_service(&ctx, "locale")?;
    required_service(&ctx, "connection")?;
    required_service(&ctx, "remote")?;
    let settings_scope = required_service(&ctx, "settingsScope")?;
    let host = call_method(
        &settings_scope,
        "bind",
        &[object(&[("namespace", JsValue::from_str(THEME_SETTINGS_NAMESPACE))])?.into()],
    )?;
    let theme = WasmThemeRuntime::construct(&ctx, &host)?;
    let theme_inner = theme.inner.clone();
    let theme_face: JsValue = WasmThemeRuntime::from_inner(theme.inner).into();
    call_method(
        &ctx,
        "provide",
        &[JsValue::from_str("theme"), theme_face.clone()],
    )?;

    own_locale_dictionaries(&ctx, &locale)?;
    let store = create_appearance_store(&modules.runtime)?;
    let bound = Rc::new(RefCell::new(None::<JsValue>));
    let sync_bound = bound.clone();
    let sync = Closure::wrap(Box::new(move |snapshot: JsValue| -> Result<(), JsValue> {
        sync_appearance(&sync_bound, &snapshot)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    call_method(
        &ctx,
        "on",
        &[JsValue::from_str("theme/change"), sync.into_js_value()],
    )?;

    let inject_bound = bound;
    let inject_theme = theme_face;
    let inject_inner = theme_inner;
    let inject = Closure::wrap(
        Box::new(move |actions: JsValue| -> Result<JsValue, JsValue> {
            *inject_bound.borrow_mut() = Some(actions);
            let snapshot = inject_inner.borrow_mut().snapshot_value()?;
            sync_appearance(&inject_bound, &snapshot)?;
            let set_theme_face = inject_theme.clone();
            let set_theme = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
                call_method(&set_theme_face, "setTheme", &[JsValue::from_str(&id)])?;
                Ok(())
            })
                as Box<dyn FnMut(String) -> Result<(), JsValue>>);
            object(&[("setTheme", set_theme.into_js_value())]).map(Into::into)
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );

    let component = appearance_component(&modules);
    let registration = object(&[
        ("name", JsValue::from_str("settings.general.item")),
        ("id", JsValue::from_str("appearance")),
        ("order", JsValue::from_f64(10.0)),
        ("store", store),
        ("locale", JsValue::from_str(SETTINGS_NS)),
        ("inject", inject.into_js_value()),
    ])?;
    let inject_slots = slots.clone();
    let declaration = Closure::wrap(Box::new(move || {
        call_method(
            &inject_slots,
            "register",
            &[registration.clone().into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("settings.general.item"),
            declaration.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Exact Client plugin inject list.
#[wasm_bindgen(js_name = themeInject)]
pub fn theme_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Public Appearance locale namespace constant.
#[wasm_bindgen(js_name = themeSettingsNamespace)]
pub fn theme_settings_namespace() -> String {
    SETTINGS_NS.to_owned()
}

fn sync_appearance(bound: &RefCell<Option<JsValue>>, snapshot: &JsValue) -> Result<(), JsValue> {
    let Some(actions) = bound.borrow().clone() else {
        return Ok(());
    };
    call_method(
        &actions,
        "sync",
        &[
            required_property(snapshot, "preference", "Theme snapshot")?,
            required_property(snapshot, "revision", "Theme snapshot")?,
        ],
    )?;
    Ok(())
}

fn create_appearance_store(runtime: &JsValue) -> Result<JsValue, JsValue> {
    let init = Closure::wrap(Box::new(move || {
        object(&[
            ("preference", JsValue::from_str("system")),
            ("revision", JsValue::from_f64(-1.0)),
        ])
        .map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let actions = Object::new();
    let sync = Closure::wrap(Box::new(
        move |draft: JsValue, preference: String, revision: f64| -> Result<(), JsValue> {
            let current = required_number(&draft, "revision")?;
            if revision <= current {
                return Ok(());
            }
            set_value(&draft, "preference", &JsValue::from_str(&preference))?;
            set_value(&draft, "revision", &JsValue::from_f64(revision))
        },
    )
        as Box<dyn FnMut(JsValue, String, f64) -> Result<(), JsValue>>);
    set(&actions, "sync", &sync.into_js_value())?;
    let declaration = object(&[("init", init.into_js_value()), ("actions", actions.into())])?;
    call_method(runtime, "defineStore", &[declaration.into()])
}

#[derive(Clone)]
struct ReactUi {
    react: JsValue,
    primitives: JsValue,
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

    fn primitive(&self, name: &str) -> Result<JsValue, JsValue> {
        self.element(
            &required_property(&self.primitives, name, "UI primitives")?,
            None,
            &[],
        )
    }
}

fn appearance_component(modules: &BrowserModules) -> JsValue {
    let ui = ReactUi {
        react: modules.react.clone(),
        primitives: modules.primitives.clone(),
    };
    let component = Closure::wrap(
        Box::new(move |props: JsValue| render_appearance(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    component.into_js_value()
}

fn render_appearance(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let selector = Closure::wrap(Box::new(move |state: JsValue| {
        Reflect::get(&state, &JsValue::from_str("preference"))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let preference =
        function(props, "useStore")?.call1(&JsValue::UNDEFINED, &selector.into_js_value())?;
    let preference = preference
        .as_string()
        .unwrap_or_else(|| "system".to_owned());
    let title = ui.tag(
        "div",
        Some(&class_props("seekdeep-theme-title")?),
        &[translated(props, "appearance.title")?],
    )?;
    let set_theme = function(props, "setTheme")?;
    let mut cubes = Vec::new();
    for (id, label, icon) in [
        ("light", "appearance.light", "IconLightOutline16"),
        ("dark", "appearance.dark", "IconDarkOutline16"),
        ("system", "appearance.system", "IconFollowsystemOutline16"),
    ] {
        let selected = preference == id;
        let click_theme = set_theme.clone();
        let click_id = id.to_owned();
        let click = Closure::wrap(Box::new(move || {
            click_theme.call1(&JsValue::UNDEFINED, &JsValue::from_str(&click_id))
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        cubes.push(ui.tag(
            "button",
            Some(&object(&[
                ("key", JsValue::from_str(id)),
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(if selected {
                        "seekdeep-theme-cube seekdeep-theme-selected"
                    } else {
                        "seekdeep-theme-cube"
                    }),
                ),
                ("aria-pressed", JsValue::from_bool(selected)),
                ("onClick", click.into_js_value()),
            ])?),
            &[ui.primitive(icon)?, translated(props, label)?],
        )?);
    }
    let row = ui.tag(
        "div",
        Some(&class_props("seekdeep-theme-cube-row")?),
        &cubes,
    )?;
    ui.tag(
        "div",
        Some(&class_props("seekdeep-theme-group")?),
        &[title, row],
    )
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let dictionaries = Object::new();
    set(&dictionaries, "zh", &dictionary(THEME_ZH)?)?;
    set(&dictionaries, "en", &dictionary(THEME_EN)?)?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(SETTINGS_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-theme: settings row dictionaries"),
        ],
    )?;
    Ok(())
}

fn dictionary(entries: &[(&str, &str)]) -> Result<JsValue, JsValue> {
    let output = Object::new();
    for (key, value) in entries {
        set(&output, key, &JsValue::from_str(value))?;
    }
    Ok(output.into())
}

fn parse_definition(value: &JsValue) -> Result<ThemeDefinition, JsValue> {
    let id = required_string(value, "id", "Theme definition")?;
    let color_scheme = required_string(value, "colorScheme", "Theme definition")?;
    let color_scheme = ColorScheme::parse(&color_scheme)
        .ok_or_else(|| js_error("theme colorScheme must be \"light\" or \"dark\""))?;
    let tokens = required_property(value, "tokens", "Theme definition")?;
    let mut parsed = IndexMap::new();
    for entry in Object::entries(&Object::from(tokens)).iter() {
        let entry = Array::from(&entry);
        let name = entry
            .get(0)
            .as_string()
            .ok_or_else(|| js_error("theme token name must be a string"))?;
        let value = entry
            .get(1)
            .as_string()
            .ok_or_else(|| js_error(&format!("theme token {name:?} must be a string")))?;
        parsed.insert(name, value);
    }
    Ok(ThemeDefinition {
        id: ThemeId::new(id),
        color_scheme,
        tokens: parsed,
    })
}

fn parse_overrides(source: &str, value: &JsValue) -> Result<ThemeTokenOverrides, JsValue> {
    if !value.is_object() || value.is_null() {
        return Err(type_error(&format!(
            "theme overrides from {source:?} must be an object"
        )));
    }
    let mut parsed = IndexMap::new();
    for entry in Object::entries(&Object::from(value.clone())).iter() {
        let entry = Array::from(&entry);
        let name = entry
            .get(0)
            .as_string()
            .ok_or_else(|| type_error("theme override name must be a string"))?;
        let modes = entry.get(1);
        if let Some(single) = modes.as_string() {
            return Err(type_error(
                &ThemeRegistryError::BareOverride {
                    layer_source: source.to_owned(),
                    name,
                    value: single,
                }
                .to_string(),
            ));
        }
        if !modes.is_object() || modes.is_null() {
            return Err(type_error(
                &ThemeRegistryError::InvalidOverridePair {
                    layer_source: source.to_owned(),
                    name,
                }
                .to_string(),
            ));
        }
        let light = Reflect::get(&modes, &JsValue::from_str("light"))?.as_string();
        let dark = Reflect::get(&modes, &JsValue::from_str("dark"))?.as_string();
        let (Some(light), Some(dark)) = (light, dark) else {
            return Err(type_error(
                &ThemeRegistryError::InvalidOverridePair {
                    layer_source: source.to_owned(),
                    name,
                }
                .to_string(),
            ));
        };
        parsed.insert(name, ThemeTokenModes { light, dark });
    }
    Ok(parsed)
}

fn theme_snapshot_to_js(snapshot: &ThemeSnapshot) -> Result<JsValue, JsValue> {
    let definitions = snapshot
        .themes
        .iter()
        .map(theme_definition_to_js)
        .collect::<Result<Vec<_>, _>>()?;
    let themes = Array::new();
    for definition in &definitions {
        themes.push(definition);
    }
    Object::freeze(&themes);
    let active = snapshot
        .themes
        .iter()
        .position(|definition| definition == &snapshot.active)
        .map_or_else(
            || theme_definition_to_js(&snapshot.active),
            |index| Ok(definitions[index].clone()),
        )?;
    let output = object(&[
        (
            "preference",
            JsValue::from_str(snapshot.preference.as_str()),
        ),
        ("active", active),
        ("themes", themes.into()),
        ("revision", u64_number(snapshot.revision)),
    ])?;
    Object::freeze(&output);
    Ok(output.into())
}

fn theme_definition_to_js(definition: &ThemeDefinition) -> Result<JsValue, JsValue> {
    let tokens = Object::new();
    for (name, value) in &definition.tokens {
        set(&tokens, name, &JsValue::from_str(value))?;
    }
    Object::freeze(&tokens);
    let output = object(&[
        ("id", JsValue::from_str(definition.id.as_str())),
        (
            "colorScheme",
            JsValue::from_str(definition.color_scheme.as_str()),
        ),
        ("tokens", tokens.into()),
    ])?;
    Object::freeze(&output);
    Ok(output.into())
}

fn inject_styles() -> Result<(), JsValue> {
    let document = required_property(&js_sys::global(), "document", "global")?;
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin"),
            JsValue::from_str("@seekdeep-ai/seekdeep-client-ui-theme"),
        ],
    )?;
    set_value(&style, "textContent", &JsValue::from_str(APPEARANCE_STYLES))?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_error("client-ui-theme module factory did not configure browser modules")
        })
    })
}

fn translated(props: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    function(props, "t")?.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() {
        Err(js_error(&format!(
            "client-ui-theme requires Client Service {name:?}"
        )))
    } else {
        Ok(service)
    }
}

fn optional_global_function(name: &str) -> Result<Option<Function>, JsValue> {
    let value = Reflect::get(&js_sys::global(), &JsValue::from_str(name))?;
    if value.is_undefined() {
        Ok(None)
    } else {
        value.dyn_into::<Function>().map(Some)
    }
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_error(&format!(
            "ui-theme: {owner} omitted required property {key:?}"
        )))
    } else {
        Ok(property)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| {
            js_error(&format!(
                "ui-theme: {owner} property {key:?} must be a string"
            ))
        })
}

fn required_number(value: &JsValue, key: &str) -> Result<f64, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_f64()
        .ok_or_else(|| js_error(&format!("ui-theme: missing number {key:?}")))
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let output = Object::new();
    for (key, value) in entries {
        set(&output, key, value)?;
    }
    Ok(output)
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

#[allow(clippy::needless_pass_by_value)]
fn registry_error(error: ThemeRegistryError) -> JsValue {
    js_error(&error.to_string())
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

fn type_error(message: &str) -> JsValue {
    js_sys::TypeError::new(message).into()
}

fn u64_number(value: u64) -> JsValue {
    JsValue::from_f64(
        value
            .to_string()
            .parse()
            .expect("u64 decimal text is a finite JavaScript number"),
    )
}
