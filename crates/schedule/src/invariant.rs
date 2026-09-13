//! Package-owned strict Schedule stream invariant.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent},
    session_store::SESSIONS,
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};

use crate::domain::{ScheduleLogError, fold_schedule_events};

const PACKAGE_NAME: &str = "seekdeep-schedule";

/// Cordis invariant-companion plugin name.
pub const NAME: &str = "tool-schedule-invariant";

/// Registers the package-owned strict Schedule stream invariant companion.
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
        .map(|store| store.list())
        .unwrap_or_default();
    for session in &sessions {
        validate(
            &session.events(),
            session.header().seed_length.unwrap_or(0),
            failure,
        )?;
    }

    let created_failure = failure.clone();
    context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            let session = args
                .get::<Session>(0)
                .ok_or_else(|| anyhow::anyhow!("session/created lacks a session"))?;
            validate(
                &session.events(),
                session.header().seed_length.unwrap_or(0),
                &created_failure,
            )?;
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    let dispatch_failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            let event_name = args
                .get::<String>(1)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
            if event_name.as_str() != "session/event" {
                return Ok(EventReply::Undefined);
            }
            let carried = args
                .get::<EventArgs>(2)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
            let session = carried
                .get::<Session>(0)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its session"))?;
            let event = carried
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
            if event.event_type != "schedule/change" {
                return Ok(EventReply::Undefined);
            }
            let mut events = session.events();
            events.push(event.as_ref().clone());
            validate(
                &events,
                session.header().seed_length.unwrap_or(0),
                &dispatch_failure,
            )?;
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn validate(
    events: &[SessionEvent],
    seed_length: u64,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let seed_length = usize::try_from(seed_length)
        .map_err(|_| anyhow::anyhow!("schedule seedLength exceeds the supported event count"))?;
    match fold_schedule_events(events, seed_length) {
        Ok(_) => Ok(()),
        Err(error @ ScheduleLogError { .. }) => Err(failure.fail(error.message).into()),
    }
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}
