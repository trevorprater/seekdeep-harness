//! Host settings registration for the browser locale preference.

use seekdeep_cordis::Plugin;
use seekdeep_schemastery::Schema;
use seekdeep_settings::{SETTINGS, SettingsRegisterOptions, settings_namespace};
use serde_json::Value;

use crate::{LOCALE_PREFERENCE_FIELD, LOCALE_SETTINGS_NAMESPACE};

/// Source-compatible optional locale preference schema.
#[must_use]
pub fn locale_settings_schema() -> Schema {
    Schema::object([(
        LOCALE_PREFERENCE_FIELD,
        Schema::union([Schema::constant("zh"), Schema::constant("en")]),
    )])
}

/// Host-side package plugin with dynamically optional settings registration.
#[must_use]
pub fn host_plugin() -> Plugin {
    Plugin::new(
        "client-locale",
        std::iter::empty::<String>(),
        |context, _| {
            Box::pin(async move {
                let registration = Plugin::new(
                    "client-locale:settings",
                    ["settings"],
                    |settings_context, _| {
                        Box::pin(async move {
                            let settings = settings_context.get(SETTINGS).ok_or_else(|| {
                                anyhow::anyhow!("settings service disappeared during attachment")
                            })?;
                            let namespace = settings_namespace(LOCALE_SETTINGS_NAMESPACE)?;
                            settings.register(
                                &settings_context,
                                &namespace,
                                locale_settings_schema(),
                                SettingsRegisterOptions::default(),
                            )?;
                            Ok(())
                        })
                    },
                );
                context.plugin(registration, Value::Null)?;
                Ok(())
            })
        },
    )
}
