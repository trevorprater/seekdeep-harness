//! Host settings registration and pre-plugin palette transform.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin};
use seekdeep_host_webserver::WEB_SERVER;
use seekdeep_schemastery::Schema;
use seekdeep_settings::{SETTINGS, SettingsRegisterOptions, settings_namespace};
use serde_json::Value;

use crate::{THEME_PREFERENCE_FIELD, THEME_SETTINGS_NAMESPACE, ThemePreference, inject_boot_theme};

/// Durable built-in preference schema with the source `system` default.
#[must_use]
pub fn theme_settings_schema() -> Schema {
    Schema::object([(
        THEME_PREFERENCE_FIELD,
        Schema::union([
            Schema::constant("light"),
            Schema::constant("dark"),
            Schema::constant("system"),
        ])
        .with_default("system"),
    )])
}

/// Reads the current registered setting or returns the schema default.
#[must_use]
pub fn read_preference(context: &Context) -> ThemePreference {
    let Some(settings) = context.get(SETTINGS) else {
        return ThemePreference::System;
    };
    let Ok(namespace) = settings_namespace(THEME_SETTINGS_NAMESPACE) else {
        return ThemePreference::System;
    };
    settings
        .get(&namespace)
        .and_then(|section| section.get(THEME_PREFERENCE_FIELD).cloned())
        .and_then(|preference| preference.as_str().and_then(ThemePreference::parse))
        .unwrap_or(ThemePreference::System)
}

/// Host plugin with dynamically optional settings and web-server attachments.
#[must_use]
pub fn host_plugin() -> Plugin {
    Plugin::new(
        "client-ui-theme",
        std::iter::empty::<String>(),
        |context, _| {
            Box::pin(async move {
                let settings = Plugin::new(
                    "client-ui-theme:settings",
                    ["settings"],
                    |settings_context, _| {
                        Box::pin(async move {
                            let settings = settings_context.get(SETTINGS).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "settings service disappeared during theme attachment"
                                )
                            })?;
                            settings.register(
                                &settings_context,
                                &settings_namespace(THEME_SETTINGS_NAMESPACE)?,
                                theme_settings_schema(),
                                SettingsRegisterOptions::default(),
                            )?;
                            Ok(())
                        })
                    },
                );
                context.plugin(settings, Value::Null)?;

                let root = context.clone();
                let web = Plugin::new(
                    "client-ui-theme:web",
                    ["webServer"],
                    move |web_context, _| {
                        let root = root.clone();
                        Box::pin(async move {
                            let web_server = web_context.get(WEB_SERVER).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "webServer service disappeared during theme attachment"
                                )
                            })?;
                            web_server
                                .tap_index(Arc::new(move |html: String| {
                                    inject_boot_theme(&html, read_preference(&root))
                                }))
                                .own(&web_context, "client-ui-theme: initial theme bootstrap")?;
                            Ok(())
                        })
                    },
                );
                context.plugin(web, Value::Null)?;
                Ok(())
            })
        },
    )
}
