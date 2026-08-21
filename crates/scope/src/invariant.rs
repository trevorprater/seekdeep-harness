//! Package-owned scoped-dispatch invariants.

use std::sync::Arc;

use seekdeep_cordis::{Context, DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

use crate::{
    scope_of,
    scoped_events::{ScopedSubjectRequirement, scoped_subject_requirement},
};

const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-scope";

/// Cordis invariant companion plugin name.
pub const INVARIANT_NAME: &str = "scope-invariant";
/// Service required before the companion can register.
pub const INVARIANT_INJECT: &[&str] = &["invariants"];

/// Registers the generated scoped-dispatch invariant.
///
/// # Errors
///
/// Returns ordinary invariant registration or installer failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(std::iter::empty::<String>(), |context, failure| async move {
            context.events().on_sync(
                &context,
                "internal/dispatch",
                move |_, args| {
                    args.get::<DispatchMode>(0).ok_or_else(|| {
                        anyhow::anyhow!("internal/dispatch lacks a dispatch mode")
                    })?;
                    let event = args.get::<String>(1).ok_or_else(|| {
                        anyhow::anyhow!("internal/dispatch lacks an event name")
                    })?;
                    let event_args = args.get::<EventArgs>(2).ok_or_else(|| {
                        anyhow::anyhow!("internal/dispatch lacks event arguments")
                    })?;
                    let dispatch = args.get::<Context>(3).ok_or_else(|| {
                        anyhow::anyhow!("internal/dispatch lacks a dispatch context")
                    })?;
                    let Some(requirement) = scoped_subject_requirement(&event) else {
                        return Ok(EventReply::Undefined);
                    };
                    let Some(carrier) = scope_of(&dispatch) else {
                        return Err(failure
                            .fail(format!(
                                "\"{event}\" is a scope-filtered event but was dispatched without a scope carrier — pass scopeTarget(base, subject) as the dispatch thisArg (agent events: use agentEvents(ctx, agent))"
                            ))
                            .into());
                    };
                    if requirement == ScopedSubjectRequirement::Subject
                        && event_args
                            .scope_subject()
                            .map(seekdeep_cordis::EventSubjectToken::as_uuid)
                            != Some(carrier.as_uuid())
                    {
                        return Err(failure
                            .fail(format!(
                                "\"{event}\" was dispatched with a scope carrier keyed to a DIFFERENT subject than its arguments name — the carrier key and the event's subject must be the same object (use agentEvents(ctx, agent))"
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

#[cfg(test)]
mod tests {
    use seekdeep_cordis::EventArgs;
    use seekdeep_invariants::InvariantConfig;

    use crate::{ScopeKey, scope_target, scoped_event_args};

    use super::*;

    fn emit(
        context: &Context,
        carrier: Option<ScopeKey>,
        event: &str,
        args: &EventArgs,
    ) -> anyhow::Result<()> {
        let dispatch =
            carrier.map_or_else(|| context.clone(), |key| scope_target(context, Some(key)));
        context.events().emit(&dispatch, event, args)
    }

    #[tokio::test]
    async fn validates_generated_subject_and_presence_requirements() {
        assert_eq!(INVARIANT_NAME, "scope-invariant");
        assert_eq!(INVARIANT_INJECT, ["invariants"]);
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let registration = register_invariant(&registry).unwrap();
        registration.await_ready().await.unwrap();
        let subject = ScopeKey::new();
        let other = ScopeKey::new();

        emit(&context, None, "ordinary/event", &EventArgs::new()).unwrap();
        let missing = emit(&context, None, "agent/status", &EventArgs::new()).unwrap_err();
        assert!(missing.to_string().contains("without a scope carrier"));
        emit(
            &context,
            Some(subject),
            "agent/status",
            &scoped_event_args(subject, EventArgs::new()),
        )
        .unwrap();
        let mismatched = emit(
            &context,
            Some(other),
            "agent/status",
            &scoped_event_args(subject, EventArgs::new()),
        )
        .unwrap_err();
        assert!(mismatched.to_string().contains("DIFFERENT subject"));

        emit(
            &context,
            Some(subject),
            "session/created",
            &EventArgs::new(),
        )
        .unwrap();
        let presence = emit(&context, None, "session/created", &EventArgs::new()).unwrap_err();
        assert!(presence.to_string().contains("without a scope carrier"));

        registration.dispose().await.unwrap();
        emit(&context, None, "agent/status", &EventArgs::new()).unwrap();
    }
}
