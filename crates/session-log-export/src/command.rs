//! Web-only `/export` command registration.

use std::sync::Arc;

use seekdeep_commands::{COMMANDS, CommandDefinition, CommandResult};
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};

/// Cordis plugin name.
pub const NAME: &str = "session-log-download";
/// Required host capability.
pub const INJECT: &[&str] = &["commands"];

/// Builds the pathless browser-observed export command plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            apply(&context)?;
            Ok(())
        })
    })
}

/// Registers the Web-only `/export` command.
///
/// # Errors
///
/// Returns missing-command-service, duplicate, metadata, or inactive-owner failures.
pub fn apply(context: &Context) -> anyhow::Result<EffectHandle> {
    let commands = context
        .get(COMMANDS)
        .ok_or_else(|| anyhow::anyhow!("session-log-download requires commands"))?;
    commands.register(
        context,
        CommandDefinition::new(
            "export",
            "Download this Session log as a ZIP archive",
            Arc::new(|invocation| {
                Box::pin(async move {
                    Ok(if invocation.raw_input.trim().is_empty() {
                        CommandResult::success(Some("Session log download requested."))
                    } else {
                        CommandResult::error("The Web /export command does not accept a path.")
                    })
                })
            }),
        ),
    )
}
