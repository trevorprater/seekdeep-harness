//! Package-owned settings event invariant.

use std::sync::Arc;

use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

use crate::{SETTINGS, SETTINGS_UPDATED_EVENT, SettingsNamespace};

/// Full package identity reserved in the invariant registry.
pub const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-settings";
/// Stable companion name.
pub const INVARIANT_NAME: &str = "settings-invariant";

/// Registers the settings invariant companion.
///
/// # Errors
///
/// Returns invariant-registry or event-listener registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(std::iter::empty::<String>(), |context, failure| async move {
            let listener_context = context.clone();
            let event_context = context.clone();
            context.events().on_sync(
                &listener_context,
                SETTINGS_UPDATED_EVENT,
                move |_, args| {
                    let ns = args
                        .get::<SettingsNamespace>(0)
                        .ok_or_else(|| failure.fail("settings/updated omitted its namespace"))?;
                    let next = args
                        .get::<serde_json::Value>(1)
                        .ok_or_else(|| failure.fail("settings/updated omitted its next value"))?;
                    let prev = args
                        .get::<serde_json::Value>(2)
                        .ok_or_else(|| failure.fail("settings/updated omitted its previous value"))?;
                    let settings = event_context.get(SETTINGS).ok_or_else(|| {
                        failure.fail(format!(
                            "settings/updated for \"{ns}\" emitted without a live settings service"
                        ))
                    })?;
                    let current = settings.get(&ns).ok_or_else(|| {
                        failure.fail(format!(
                            "settings/updated for \"{ns}\" emitted while the namespace is unregistered"
                        ))
                    })?;
                    if current != *next {
                        return Err(failure
                            .fail(format!(
                                "settings/updated for \"{ns}\" does not match the authoritative resolved value"
                            ))
                            .into());
                    }
                    if *next == *prev {
                        return Err(failure
                            .fail(format!(
                                "settings/updated for \"{ns}\" emitted without a resolved-value change"
                            ))
                            .into());
                    }
                    Ok(EventReply::Undefined)
                },
                EventOptions {
                    global: true,
                    ..EventOptions::default()
                },
            )?;
            Ok(())
        }),
    )
}
