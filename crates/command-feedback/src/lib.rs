//! Model-agnostic `/feedback` command recording human remarks on a session.

use std::sync::Arc;

use seekdeep_anonymous_user_id::{AnonymousUserIdOptions, get_or_create_anonymous_user_id};
use seekdeep_commands::{
    COMMANDS, CommandDefinition, CommandHandler, CommandInvocation, CommandResult, CommandRuntime,
};
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_core::session::{AppendOptions, Session};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
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

/// Validate, record, and acknowledge one feedback entry.
fn execute_feedback_command(invocation: &CommandInvocation) -> anyhow::Result<CommandResult> {
    if invocation.raw_input.trim().is_empty() {
        return Ok(CommandResult::error(format!(
            "Feedback text is required. {USAGE}"
        )));
    }
    record_feedback(invocation.agent.session(), &invocation.raw_input)?;
    let user_id = get_or_create_anonymous_user_id(AnonymousUserIdOptions::default())?;
    Ok(CommandResult::success(Some(format!(
        "Feedback recorded for session {}
Anonymous user: {}. Session sharing is not configured.",
        invocation.agent.session().id(),
        user_id
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
    let handler: CommandHandler =
        Arc::new(|invocation| Box::pin(async move { execute_feedback_command(&invocation) }));
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
