//! Package-owned permission-preset event invariants.

use std::sync::Arc;

use seekdeep_cordis::{Context, DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_core::{session::SessionEvent, session_store::SESSIONS};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use serde_json::Value;

use crate::index::PERMISSION_PRESETS;

/// Package name reserved by this companion.
pub const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-permission-presets";

/// Cordis companion plugin name.
pub const NAME: &str = "permission-presets-invariant";

/// Services required before the companion can reserve package ownership.
pub const INJECT: &[&str] = &["invariants"];

fn fail_invariant(fail: &InvariantFailure, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(fail.fail(message))
}

fn validate_event(
    context: &Context,
    event: &SessionEvent,
    fail: &InvariantFailure,
) -> anyhow::Result<()> {
    if event.event_type != "permission/preset" {
        return Ok(());
    }
    let preset = event
        .data
        .get("preset")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let known = context
        .get(PERMISSION_PRESETS)
        .is_some_and(|service| service.names().iter().any(|name| name == preset));
    if !known {
        return Err(fail_invariant(
            fail,
            format!(
                "permission/preset names unknown preset {}",
                serde_json::to_string(preset).unwrap_or_default()
            ),
        ));
    }
    Ok(())
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

/// Registers the permission-preset invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration or installer failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(
            ["permissionPresets", "sessions"],
            |context, failure| async move {
                if let Some(store) = context.get(SESSIONS) {
                    for session in store.list() {
                        for event in session.events() {
                            validate_event(&context, &event, &failure)?;
                        }
                    }
                }
                let dispatch_failure = failure.clone();
                let dispatch_context = context.clone();
                context.events().on_sync(
                    &context,
                    "internal/dispatch",
                    move |_, args| {
                        args.get::<DispatchMode>(0).ok_or_else(|| {
                            anyhow::anyhow!("internal/dispatch lacks a dispatch mode")
                        })?;
                        let event_name = args.get::<String>(1).ok_or_else(|| {
                            anyhow::anyhow!("internal/dispatch lacks an event name")
                        })?;
                        let event_args = args.get::<EventArgs>(2).ok_or_else(|| {
                            anyhow::anyhow!("internal/dispatch lacks event arguments")
                        })?;
                        if event_name.as_str() != "session/event" {
                            return Ok(EventReply::Undefined);
                        }
                        let event = event_args
                            .get::<SessionEvent>(1)
                            .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
                        validate_event(&dispatch_context, &event, &dispatch_failure)?;
                        Ok(EventReply::Undefined)
                    },
                    global_events(),
                )?;
                Ok(())
            },
        ),
    )
}
