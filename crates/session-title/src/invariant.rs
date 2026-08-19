//! Package-owned durable title-source invariants.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply};
use seekdeep_core::session::SessionEvent;
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use serde_json::Value;

const PACKAGE_NAME: &str = "seekdeep-session-title";

/// Registers the durable title-source invariant companion.
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
    let failure = failure.clone();
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
            let event = carried
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
            validate_event(&event, &failure)?;
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn validate_event(event: &SessionEvent, failure: &InvariantFailure) -> anyhow::Result<()> {
    if event.event_type != "session/title" {
        return Ok(());
    }
    let source_kind = event
        .data
        .get("source")
        .and_then(Value::as_object)
        .and_then(|source| source.get("kind"))
        .and_then(Value::as_str);
    let message_seqs = event
        .data
        .get("messageSeqs")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let is_user = source_kind == Some("user");
    if (message_seqs == 0) != is_user {
        let requirement = if is_user {
            "cite no message seqs"
        } else {
            "cite at least one message seq"
        };
        let rendered_kind = source_kind.map_or_else(|| "undefined".to_owned(), str::to_owned);
        return Err(failure
            .fail(format!(
                "session/title event {} with source \"{rendered_kind}\" must {requirement}; got {message_seqs}",
                event.seq
            ))
            .into());
    }
    Ok(())
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}
