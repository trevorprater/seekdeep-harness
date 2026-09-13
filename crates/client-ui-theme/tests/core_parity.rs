//! Portable registry, settings mirror, bootstrap, and stylesheet parity.

#![cfg(not(target_arch = "wasm32"))]

use seekdeep_client_ui_theme::*;

fn definition(id: &str, color_scheme: ColorScheme, tokens: &[(&str, &str)]) -> ThemeDefinition {
    ThemeDefinition {
        id: ThemeId::new(id),
        color_scheme,
        tokens: tokens
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

fn overrides(rows: &[(&str, &str, &str)]) -> ThemeTokenOverrides {
    rows.iter()
        .map(|(name, light, dark)| {
            (
                (*name).to_owned(),
                ThemeTokenModes {
                    light: (*light).to_owned(),
                    dark: (*dark).to_owned(),
                },
            )
        })
        .collect()
}

#[test]
fn defaults_resolution_stable_snapshots_adoption_and_revision_are_exact() {
    let mut registry = ThemeRegistry::new(false);
    let initial = registry.snapshot();
    assert_eq!(initial.preference.as_str(), "system");
    assert_eq!(initial.active.id.as_str(), "light");
    assert_eq!(initial.active.color_scheme, ColorScheme::Light);
    assert_eq!(
        initial
            .themes
            .iter()
            .map(|theme| theme.id.as_str())
            .collect::<Vec<_>>(),
        ["light", "dark"]
    );
    assert!(std::rc::Rc::ptr_eq(&initial, &registry.snapshot()));

    let dark = registry
        .set_theme(ThemeSelection::new("dark"))
        .unwrap()
        .unwrap();
    assert_eq!(dark.persist, Some(ThemePreference::Dark));
    assert_eq!(dark.snapshot.revision, 1);
    assert_eq!(dark.snapshot.active.color_scheme, ColorScheme::Dark);
    assert!(
        registry
            .set_theme(ThemeSelection::new("dark"))
            .unwrap()
            .is_none()
    );
    assert!(registry.adopt(ThemePreference::Dark).is_none());
    assert_eq!(registry.adopt(ThemePreference::Light).unwrap().revision, 2);

    assert!(registry.set_system_dark(true).is_none());
    let system = registry
        .set_theme(ThemeSelection::new("system"))
        .unwrap()
        .unwrap();
    assert_eq!(system.snapshot.active.id.as_str(), "dark");
    assert_eq!(system.snapshot.revision, 3);
    assert_eq!(
        registry.set_system_dark(false).unwrap().active.id.as_str(),
        "light"
    );
}

#[test]
fn registration_errors_disposal_and_custom_persistence_boundary_match_source() {
    let mut registry = ThemeRegistry::default();
    assert_eq!(
        registry
            .set_theme(ThemeSelection::new("sepia"))
            .unwrap_err()
            .to_string(),
        "theme \"sepia\" is not registered"
    );
    assert!(
        registry
            .register(definition("light", ColorScheme::Light, &[]))
            .unwrap_err()
            .to_string()
            .contains("already registered")
    );
    assert!(
        registry
            .register(definition("system", ColorScheme::Light, &[]))
            .unwrap_err()
            .to_string()
            .contains("preference")
    );

    let (token, arrived) = registry
        .register(definition(
            "sepia",
            ColorScheme::Light,
            &[("--dsw-alias-bg-base", "red")],
        ))
        .unwrap();
    assert_eq!(arrived.revision, 1);
    let selected = registry
        .set_theme(ThemeSelection::new("sepia"))
        .unwrap()
        .unwrap();
    assert_eq!(selected.persist, None);
    assert_eq!(
        selected.snapshot.active.tokens["--dsw-alias-bg-base"],
        "red"
    );
    let disposed = registry.dispose_registration(token).unwrap();
    assert_eq!(disposed.preference.as_str(), "system");
    assert_eq!(disposed.revision, 3);
    assert!(registry.dispose_registration(token).is_none());

    let (inactive, _) = registry
        .register(definition("paper", ColorScheme::Light, &[]))
        .unwrap();
    registry.set_theme(ThemeSelection::new("dark")).unwrap();
    registry.dispose_registration(inactive).unwrap();
    assert_eq!(registry.snapshot().preference.as_str(), "dark");
}

#[test]
fn override_layers_stack_replace_validate_and_dispose_by_exact_generation() {
    let mut registry = ThemeRegistry::default();
    let (first, _) = registry.override_tokens(
        ThemeOverrideSource::new("first"),
        overrides(&[
            ("--shared", "first-light", "first-dark"),
            ("--first", "first-only-light", "first-only-dark"),
        ]),
    );
    let (second, _) = registry.override_tokens(
        ThemeOverrideSource::new("second"),
        overrides(&[("--shared", "second-light", "second-dark")]),
    );
    assert_eq!(
        registry.snapshot().active.tokens["--shared"],
        "second-light"
    );
    assert_eq!(
        registry.snapshot().active.tokens["--first"],
        "first-only-light"
    );
    registry.set_theme(ThemeSelection::new("dark")).unwrap();
    assert_eq!(registry.snapshot().active.tokens["--shared"], "second-dark");
    registry
        .dispose_override(&ThemeOverrideSource::new("second"), second)
        .unwrap();
    assert_eq!(registry.snapshot().active.tokens["--shared"], "first-dark");
    registry
        .dispose_override(&ThemeOverrideSource::new("first"), first)
        .unwrap();
    assert!(!registry.snapshot().active.tokens.contains_key("--shared"));

    let (stale, _) = registry.override_tokens(
        ThemeOverrideSource::new("package"),
        overrides(&[("--old", "old-light", "old-dark")]),
    );
    let (current, _) = registry.override_tokens(
        ThemeOverrideSource::new("package"),
        overrides(&[("--new", "new-light", "new-dark")]),
    );
    assert!(
        registry
            .dispose_override(&ThemeOverrideSource::new("package"), stale)
            .is_none()
    );
    assert!(registry.snapshot().active.tokens.contains_key("--new"));
    registry
        .dispose_override(&ThemeOverrideSource::new("package"), current)
        .unwrap();
    assert!(
        registry
            .dispose_override(&ThemeOverrideSource::new("package"), current)
            .is_none()
    );

    assert!(
        ThemeRegistryError::BareOverride {
            layer_source: "package".into(),
            name: "--bad".into(),
            value: "red".into(),
        }
        .to_string()
        .contains("bare string")
    );
    assert!(
        ThemeRegistryError::InvalidOverridePair {
            layer_source: "package".into(),
            name: "--bad".into(),
        }
        .to_string()
        .contains("{ light, dark } pair")
    );
}

#[test]
fn inspection_is_sorted_deduplicated_dynamic_and_defensive() {
    let mut registry = ThemeRegistry::default();
    registry
        .register(definition(
            "custom",
            ColorScheme::Light,
            &[
                ("--dsw-alias-bg-base", "duplicate"),
                ("--registered", "registered"),
            ],
        ))
        .unwrap();
    registry.override_tokens(
        ThemeOverrideSource::new("package"),
        overrides(&[
            ("--registered", "duplicate", "duplicate"),
            ("semanticAccent", "pink", "red"),
        ]),
    );
    let mut tokens = registry.export_inspect_tokens();
    let names = tokens
        .iter()
        .map(|token| token.name.clone())
        .collect::<Vec<_>>();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(
        tokens
            .iter()
            .filter(|token| token.name == "--dsw-alias-bg-base")
            .count(),
        1
    );
    assert_eq!(
        tokens
            .iter()
            .find(|token| token.name == "--registered")
            .unwrap()
            .css_variable
            .as_deref(),
        Some("--registered")
    );
    assert!(
        tokens
            .iter()
            .find(|token| token.name == "semanticAccent")
            .unwrap()
            .css_variable
            .is_none()
    );
    tokens[0].description = "caller mutation".into();
    assert_ne!(
        registry.export_inspect_tokens()[0].description,
        "caller mutation"
    );
}

#[test]
fn appearance_mirror_boot_script_locales_and_stylesheets_are_exact() {
    assert_eq!(
        ThemePreference::parse("light"),
        Some(ThemePreference::Light)
    );
    assert_eq!(ThemePreference::parse("sepia"), None);
    let mut appearance = AppearanceRowState::default();
    assert_eq!(appearance.preference, ThemePreference::System);
    assert_eq!(appearance.revision, -1);
    appearance.sync(ThemePreference::Dark, 3);
    appearance.sync(ThemePreference::Light, 2);
    appearance.sync(ThemePreference::System, 3);
    assert_eq!(appearance.preference, ThemePreference::Dark);
    assert_eq!(appearance.revision, 3);

    let html = inject_boot_theme(
        "<html><body class=\"app\"><div id=\"root\"></div></body></html>",
        ThemePreference::Dark,
    );
    assert!(html.contains("const preference = \"dark\""));
    for exact in [
        "typeof matchMedia !== 'undefined'",
        "matchMedia('(prefers-color-scheme: dark)').matches",
        "const dark = preference === 'dark' || systemDark",
        "style.colorScheme = dark ? 'dark' : 'light'",
        "toggleAttribute('data-ds-dark-theme', dark)",
    ] {
        assert!(html.contains(exact), "{exact:?}");
    }
    assert!(html.find("<script>").unwrap() < html.find("<div id=\"root\">").unwrap());
    assert!(
        inject_boot_theme(
            "<HTML><BODY><main>x</main></BODY></HTML>",
            ThemePreference::Light
        )
        .starts_with("<HTML><BODY><script>")
    );
    assert!(
        inject_boot_theme("<main>loading</main>", ThemePreference::Dark)
            .starts_with("<main>loading</main><script>")
    );

    assert_eq!(
        INJECT,
        ["slots", "locale", "connection", "remote", "settingsScope"]
    );
    assert_eq!(INVARIANT_NAME, "client-ui-theme-invariant");
    assert_eq!(THEME_ZH.len(), 4);
    assert_eq!(THEME_EN.len(), 4);
    assert!(APPEARANCE_STYLES.contains(".seekdeep-theme-cube-row"));
    assert!(DESIGN_PLATFORM_STYLES.contains("--dsw-alias-scrollbar-bg-l1"));
    assert!(SCROLLBAR_STYLES.contains("--seekdeep-scrollbar-thumb"));
    assert!(BASE_STYLES.contains("--dsw-font-family"));
    assert!(!GRADIENT_SHADOW_TEXT_STYLES.is_empty());
    assert!(!SHIKI_STYLES.is_empty());
}
