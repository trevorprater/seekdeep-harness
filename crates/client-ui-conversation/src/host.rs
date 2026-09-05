//! Host registration for durable conversation preferences.

use seekdeep_cordis::Plugin;
use seekdeep_schemastery::Schema;
use seekdeep_settings::{SETTINGS, SettingsRegisterOptions, settings_namespace};
use serde_json::Value;

use crate::{BUSY_ENTER_FIELD, CONVERSATION_SETTINGS_NAMESPACE};

/// Durable busy-Enter settings schema.
#[must_use]
pub fn conversation_settings_schema() -> Schema {
    Schema::object([(
        BUSY_ENTER_FIELD,
        Schema::union([Schema::constant("queue"), Schema::constant("steer")]).with_default("queue"),
    )])
}

/// Host plugin with dynamically optional settings registration.
#[must_use]
pub fn host_plugin() -> Plugin {
    Plugin::new(
        "client-ui-conversation",
        std::iter::empty::<String>(),
        |context, _| {
            Box::pin(async move {
                let registration = Plugin::new(
                    "client-ui-conversation:settings",
                    ["settings"],
                    |settings_context, _| {
                        Box::pin(async move {
                            let settings = settings_context.get(SETTINGS).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "settings service disappeared during conversation attachment"
                                )
                            })?;
                            settings.register(
                                &settings_context,
                                &settings_namespace(CONVERSATION_SETTINGS_NAMESPACE)?,
                                conversation_settings_schema(),
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
