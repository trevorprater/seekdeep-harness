//! Model-agnostic `/feedback` command recording human remarks on a session.

use std::sync::Arc;

use seekdeep_anonymous_user_id::{AnonymousUserIdOptions, get_or_create_anonymous_user_id};
use seekdeep_commands::{
    COMMANDS, CommandDefinition, CommandHandler, CommandInvocation, CommandResult, CommandRuntime,
};
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_core::session::{AppendOptions, Session};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_session_telemetry::{SESSION_TELEMETRY, SessionTelemetrySharingStatus};
use serde_json::json;

const USAGE: &str = "Usage: /feedback <text>";
/// Loader-facing Cordis plugin name.
pub const NAME: &str = "command-feedback";
/// Runtime services required by the feedback command.
pub const INJECT: &[&str] = &["commands"];

/// Records feedback independently of any UI trigger.
///
/// # Errors
///
/// Rejects empty normalized text or an invalid append.
pub fn record_feedback(session: &Session, text: &str) -> anyhow::Result<()> {
    let normalized = text.trim();
    anyhow::ensure!(!normalized.is_empty(), "feedback text must not be empty");
    session.append(
        "feedback/record",
        json!({"text": normalized}),
        AppendOptions::default(),
    )?;
    Ok(())
}

/// The acknowledgement's sharing sentence for a disclosed policy.
const fn sharing_sentence(sharing: SessionTelemetrySharingStatus) -> &'static str {
    match sharing {
        SessionTelemetrySharingStatus::Full => "Session sharing is enabled.",
        SessionTelemetrySharingStatus::FeedbackOnly => {
            "Session sharing is feedback-gated; recording feedback releases the session prefix for sharing."
        }
        SessionTelemetrySharingStatus::Disabled => "Session sharing is disabled.",
    }
}

/// The sharing disclosure appended to the acknowledgement: the mounted telemetry service's
/// deployment-selected policy, or the not-configured sentence when the service is absent.
fn sharing_disclosure(context: &Context) -> &'static str {
    context
        .get(SESSION_TELEMETRY)
        .map_or("Session sharing is not configured.", |telemetry| {
            sharing_sentence(telemetry.backend().sharing())
        })
}

/// Validate, record, and acknowledge one feedback entry. Returning an error leaves no
/// `feedback/record` event.
fn execute_feedback_command(
    invocation: &CommandInvocation,
    context: &Context,
) -> anyhow::Result<CommandResult> {
    if invocation.raw_input.trim().is_empty() {
        return Ok(CommandResult::error(format!(
            "Feedback text is required. {USAGE}"
        )));
    }
    record_feedback(invocation.agent.session(), &invocation.raw_input)?;
    let user_id = get_or_create_anonymous_user_id(AnonymousUserIdOptions::default())?;
    Ok(CommandResult::success(Some(format!(
        "Feedback recorded for session {}
Anonymous user: {}. {}",
        invocation.agent.session().id(),
        user_id,
        sharing_disclosure(context)
    ))))
}

/// Registers the global `/feedback` command.
///
/// # Errors
///
/// Returns when the command runtime is absent or registration fails.
pub fn apply(context: &Context) -> anyhow::Result<EffectHandle> {
    let commands: Arc<CommandRuntime> = context
        .get(COMMANDS)
        .ok_or_else(|| anyhow::anyhow!("command-feedback requires commands"))?;
    let handler_context = context.clone();
    let handler: CommandHandler = Arc::new(move |invocation| {
        let context = handler_context.clone();
        Box::pin(async move { execute_feedback_command(&invocation, &context) })
    });
    let definition =
        CommandDefinition::new("feedback", "record feedback about this session", handler)
            .with_input("<text>")
            .record_input(false);
    commands.register(context, definition)
}

/// Builds the loader-compatible command plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            apply(&context)?;
            Ok(())
        })
    })
}

/// Registers the package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-command-feedback", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::{Session, SessionId};
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    #[test]
    fn record_feedback_trims_and_rejects_empty_text() {
        let session = Session::create(&SessionId::new("feedback"), None, None).expect("session");
        record_feedback(&session, "  helpful  ").expect("record");
        let events = session.events();
        assert_eq!(events.last().expect("event").event_type, "feedback/record");
        assert_eq!(events.last().expect("event").data["text"], json!("helpful"));
        assert!(record_feedback(&session, "   ").is_err());
    }

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
        let registration = register_invariant(&registry).expect("register");
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.expect("dispose");
        register_invariant(&registry).expect("replacement");
    }
}

#[cfg(test)]
mod disclosure_tests {
    use super::*;

    #[test]
    fn sharing_sentences_follow_the_source_switch() {
        assert_eq!(
            sharing_sentence(SessionTelemetrySharingStatus::Full),
            "Session sharing is enabled."
        );
        assert_eq!(
            sharing_sentence(SessionTelemetrySharingStatus::FeedbackOnly),
            "Session sharing is feedback-gated; recording feedback releases the session prefix for sharing."
        );
        assert_eq!(
            sharing_sentence(SessionTelemetrySharingStatus::Disabled),
            "Session sharing is disabled."
        );
    }

    #[test]
    fn absent_telemetry_discloses_not_configured() {
        let context = Context::new();
        assert_eq!(
            sharing_disclosure(&context),
            "Session sharing is not configured."
        );
    }
}
