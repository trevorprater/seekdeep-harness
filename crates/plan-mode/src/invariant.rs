//! Package-owned durable plan-mode invariants.

use std::sync::Arc;

use seekdeep_cordis::{Context, DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent},
    session_store::SESSIONS,
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};

const PACKAGE_NAME: &str = "seekdeep-plan-mode";

/// Registers the plan-mode invariant companion.
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
            install(&context, &failure)?;
            Ok(())
        }),
    )
}

fn install(context: &Context, failure: &InvariantFailure) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-plan-mode invariant requires sessions"))?;

    for session in sessions.list() {
        seed_session(&session, failure)?;
    }

    let created_failure = failure.clone();
    context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            let session = args
                .get::<Session>(0)
                .ok_or_else(|| anyhow::anyhow!("session/created lacks its session"))?;
            seed_session(&session, &created_failure)?;
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;

    let dispatch_failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            args.get::<DispatchMode>(0)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks a dispatch mode"))?;
            let event_name = args
                .get::<String>(1)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
            let event_args = args
                .get::<EventArgs>(2)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
            if event_name.as_str() != "session/event" {
                return Ok(EventReply::Undefined);
            }
            let event = event_args
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
            validate_event(&event, &dispatch_failure)?;
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

fn seed_session(session: &Arc<Session>, failure: &InvariantFailure) -> anyhow::Result<()> {
    for event in &session.events() {
        validate_event(event, failure)?;
    }
    Ok(())
}

/// Validates one plan/mode event before it reaches the durable log.
fn validate_event(event: &SessionEvent, failure: &InvariantFailure) -> anyhow::Result<()> {
    if event.event_type != "plan/mode" {
        return Ok(());
    }
    let active = event.data.get("active");
    if !active.is_some_and(serde_json::Value::is_boolean) {
        return Err(failure
            .fail(format!(
                "plan/mode carries invalid active state {}; expected a boolean",
                active.map_or_else(|| "undefined".to_owned(), std::string::ToString::to_string,)
            ))
            .into());
    }
    Ok(())
}
