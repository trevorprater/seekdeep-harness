//! Human-facing `/compact` command over the backend-independent compaction seam.

use std::sync::{Arc, Mutex};

use seekdeep_agent::Agent;
use seekdeep_commands::{COMMANDS, CommandDefinition, CommandInvocation, CommandResult};
use seekdeep_compaction::service::{
    COMPACTION, CompactionRoutingOptions, MaintenanceRunner, MaintenanceTask,
    ManualCompactAgentContext, ManualCompactionError, ManualCompactionErrorCode,
};
use seekdeep_cordis::{Context, fiber::EffectHandle};

/// Cordis plugin name.
pub const NAME: &str = "command-compact";

/// Services required by the compaction command.
pub const INJECT: &[&str] = &["commands", "compaction"];

const USAGE: &str = "Usage: /compact (no arguments)";

/// Converts expected capability failures into concise human-only outcomes.
fn expected_failure(error: &ManualCompactionError) -> CommandResult {
    let text = match error.code {
        ManualCompactionErrorCode::Busy => {
            "Compaction is unavailable because this process has an active compaction, or the agent is not idle."
        }
        ManualCompactionErrorCode::Cancelled => "Compaction cancelled.",
        ManualCompactionErrorCode::Changed => {
            "The history selected for compaction changed before it could be replaced. The conversation is unchanged; the attempt is recorded in the session log."
        }
        ManualCompactionErrorCode::Summary => {
            "Compaction could not produce a useful summary. The conversation is unchanged; the attempt is recorded in the session log."
        }
        ManualCompactionErrorCode::Commit => {
            "Compaction did not finish cleanly; some session history may have changed. Inspect the current session state before retrying."
        }
        ManualCompactionErrorCode::Persistence => {
            "Compaction finished, but the session could not be saved."
        }
    };
    CommandResult::error(text)
}

/// Wraps the agent's idle-only maintenance runner for the compaction engine.
fn maintenance_runner(agent: &Arc<Agent>) -> MaintenanceRunner {
    let agent = agent.clone();
    Arc::new(move |task: MaintenanceTask| {
        let agent = agent.clone();
        Box::pin(async move {
            match agent.run_maintenance(task) {
                Ok(future) => future.await,
                Err(error) => Err(anyhow::anyhow!(error.to_string())),
            }
        })
    })
}

/// Builds the exact engine context the compaction seam consumes.
fn manual_context(agent: &Arc<Agent>) -> ManualCompactAgentContext {
    ManualCompactAgentContext {
        session: agent.session().clone(),
        options: CompactionRoutingOptions {
            provider: agent
                .options()
                .provider
                .as_ref()
                .map(|p| p.as_str().to_owned()),
            model: agent
                .options()
                .model
                .as_ref()
                .map(|m| m.as_str().to_owned()),
        },
        run_maintenance: maintenance_runner(agent),
    }
}

/// Executes one argument-free manual compaction request.
async fn execute_compact(
    ctx: &Context,
    invocation: &CommandInvocation,
) -> anyhow::Result<CommandResult> {
    if !invocation.raw_input.trim().is_empty() {
        return Ok(CommandResult::error(USAGE));
    }
    let compaction = ctx
        .get(COMPACTION)
        .ok_or_else(|| anyhow::anyhow!("command-compact requires compaction"))?;
    let context = manual_context(&invocation.agent);
    match compaction
        .compact_now(&context, &invocation.signal, Some(&invocation.command_id))
        .await
    {
        Ok(Some(result)) => Ok(CommandResult::success_linked(
            Some(format!(
                "Compacted {} history items (~{} tokens).",
                result.shadowed_seqs.len(),
                result.shadowed_token_count
            )),
            result.summary_seq,
        )),
        Ok(None) => Ok(CommandResult::success(Some(
            "No compactable history yet.".to_owned(),
        ))),
        Err(error) => {
            if invocation.signal.is_aborted() {
                return Ok(CommandResult::error("Compaction cancelled."));
            }
            if let Some(error) = error.downcast_ref::<ManualCompactionError>() {
                return Ok(expected_failure(error));
            }
            Err(error)
        }
    }
}

/// Registers `/compact` for every composed human-command adapter.
///
/// # Errors
///
/// Returns missing-service or registration failures.
///
/// # Panics
///
/// Panics only if the in-flight-operation lock is poisoned, which cannot
/// happen because its guard is never held across a panic.
pub fn apply(context: &Context) -> anyhow::Result<()> {
    let commands = context
        .get(COMMANDS)
        .ok_or_else(|| anyhow::anyhow!("command-compact requires commands"))?;
    let handler_ctx = context.clone();
    let active: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));

    // Own the drain before registration so composite teardown is LIFO: the
    // command is unregistered first, then in-flight operations quiesce.
    let drain = active.clone();
    context.own(EffectHandle::new("command-compact lifecycle", move || {
        let drain = drain.clone();
        Box::pin(async move {
            let handles = std::mem::take(&mut *drain.lock().expect("lock"));
            for handle in handles {
                let _ = handle.await;
            }
            Ok(())
        })
    }))?;

    let definition = CommandDefinition::new(
        "compact",
        "Compact older conversation history",
        Arc::new(move |invocation| {
            let ctx = handler_ctx.clone();
            let active = active.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let handle = tokio::spawn(async move {
                let result = execute_compact(&ctx, &invocation).await;
                let _ = tx.send(result);
            });
            {
                let mut active_guard = active.lock().expect("lock");
                active_guard.retain(|handle| !handle.is_finished());
                active_guard.push(handle);
            }
            Box::pin(async move {
                rx.await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("command-compact task dropped")))
            })
        }),
    );
    commands.register(context, definition)?;
    Ok(())
}

/// Builds the loader-compatible compaction command plugin.
#[must_use]
pub fn plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, INJECT.iter().copied(), move |context, _config| {
        Box::pin(async move {
            apply(&context)?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_expected_failure_code() {
        let cases = [
            (ManualCompactionErrorCode::Busy, "agent is not idle"),
            (ManualCompactionErrorCode::Cancelled, "Compaction cancelled"),
            (
                ManualCompactionErrorCode::Changed,
                "history selected for compaction changed",
            ),
            (
                ManualCompactionErrorCode::Summary,
                "could not produce a useful summary",
            ),
            (ManualCompactionErrorCode::Commit, "did not finish cleanly"),
            (ManualCompactionErrorCode::Persistence, "could not be saved"),
        ];
        for (code, expected) in cases {
            let error = ManualCompactionError::new(code, "detail");
            let CommandResult::Error { text } = expected_failure(&error) else {
                panic!("expected an error result");
            };
            assert!(text.contains(expected), "{text:?} !~ {expected:?}");
        }
    }
}
