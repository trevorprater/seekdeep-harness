//! Package-owned durable sandbox-mode invariant.

use std::sync::Arc;

use seekdeep_cordis::{Context, DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent},
    session_store::SESSIONS,
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use seekdeep_sandbox::SandboxMode;

const PACKAGE_NAME: &str = "seekdeep-sandbox-policy";

/// Registers replay and pre-commit validation of `sandbox/mode` events.
///
/// # Errors
///
/// Returns ordinary invariant registration or installer failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["sessions"], |context, failure| async move {
            install(&context, &failure)
        }),
    )
}

fn install(context: &Context, failure: &InvariantFailure) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-sandbox-policy invariant requires sessions"))?;
    for session in sessions.list() {
        for event in session.events() {
            validate_event(&event, failure)?;
        }
    }
    let failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            validate_internal_dispatch(&args, &failure)?;
            Ok(EventReply::Undefined)
        },
        EventOptions {
            global: true,
            ..EventOptions::default()
        },
    )?;
    Ok(())
}

fn validate_internal_dispatch(args: &EventArgs, failure: &InvariantFailure) -> anyhow::Result<()> {
    args.get::<DispatchMode>(0)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks a dispatch mode"))?;
    let name = args
        .get::<String>(1)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
    if name.as_str() != "session/event" {
        return Ok(());
    }
    let carried = args
        .get::<EventArgs>(2)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
    carried
        .get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its session"))?;
    let event = carried
        .get::<SessionEvent>(1)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
    validate_event(&event, failure)
}

fn validate_event(event: &SessionEvent, failure: &InvariantFailure) -> anyhow::Result<()> {
    if event.event_type != "sandbox/mode" {
        return Ok(());
    }
    let mode = event.data.get("mode").and_then(serde_json::Value::as_str);
    if mode.is_none_or(|mode| SandboxMode::parse(mode).is_none()) {
        let rendered = event
            .data
            .get("mode")
            .map_or_else(|| "undefined".to_owned(), javascript_stringify);
        return Err(failure
            .fail(format!("sandbox/mode carries unknown mode {rendered}"))
            .into());
    }
    Ok(())
}

fn javascript_stringify(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_owned())
}
