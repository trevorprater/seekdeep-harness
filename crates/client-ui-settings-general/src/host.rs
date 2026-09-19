//! Host registration for durable product-wide GUI onboarding facts.

use seekdeep_cordis::Plugin;
use seekdeep_schemastery::Schema;
use seekdeep_settings::{SETTINGS, SettingsRegisterOptions, settings_namespace};
use serde_json::Value;

/// Durable product-wide GUI onboarding namespace.
pub const ONBOARDING_SETTINGS_NAMESPACE: &str = "ui-onboarding";
/// Last acknowledged product-welcome version field.
pub const WELCOME_NOTICE_VERSION_FIELD: &str = "welcomeNoticeVersion";

/// Optional onboarding settings schema.
#[must_use]
pub fn onboarding_settings_schema() -> Schema {
    Schema::object([(WELCOME_NOTICE_VERSION_FIELD, Schema::string())])
}

/// Host-side plugin with dynamically optional settings registration.
#[must_use]
pub fn host_plugin() -> Plugin {
    Plugin::new(
        "client-ui-settings-general",
        std::iter::empty::<String>(),
        |context, _| {
            Box::pin(async move {
                let registration = Plugin::new(
                    "client-ui-settings-general:onboarding-settings",
                    ["settings"],
                    |settings_context, _| {
                        Box::pin(async move {
                            let settings = settings_context.get(SETTINGS).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "settings service disappeared during onboarding attachment"
                                )
                            })?;
                            let namespace = settings_namespace(ONBOARDING_SETTINGS_NAMESPACE)?;
                            settings.register(
                                &settings_context,
                                &namespace,
                                onboarding_settings_schema(),
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
