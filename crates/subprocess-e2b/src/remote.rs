//! Shared remote-control options, poll ticks, and tolerant process-group signalling.

use std::{collections::BTreeMap, time::Duration};

use seekdeep_e2b::{E2bCommandExit, E2bCommands, E2bSandboxNotFound, e2b_control_envs};
use seekdeep_llm::AbortSignal;

/// Adds the isolated control-shell home to explicit command environment entries.
#[must_use]
pub fn command_environment(environment: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    e2b_control_envs(environment)
}

/// Resolves after one duration.
pub async fn delay(milliseconds: u64) {
    tokio::time::sleep(Duration::from_millis(milliseconds)).await;
}

/// Waits one poll interval or until cancellation wins.
#[must_use]
pub async fn wait_tick(poll_ms: u64, signal: Option<&AbortSignal>) -> bool {
    if signal.is_some_and(AbortSignal::is_aborted) {
        return false;
    }
    let tick = tokio::time::sleep(Duration::from_millis(poll_ms));
    tokio::pin!(tick);
    if let Some(signal) = signal {
        tokio::select! {
            () = &mut tick => true,
            () = signal.cancelled() => false,
        }
    } else {
        tick.await;
        true
    }
}

/// Signals remote process groups while tolerating already-gone groups or sandboxes.
///
/// # Errors
///
/// Propagates transport and provider failures other than command exit or
/// sandbox disappearance.
pub async fn signal_remote_groups(
    commands: &dyn E2bCommands,
    environment: &BTreeMap<String, String>,
    groups: &[i64],
    signal: &str,
) -> anyhow::Result<()> {
    let targets = groups
        .iter()
        .map(|group| format!("-{group}"))
        .collect::<Vec<_>>()
        .join(" ");
    let result = commands
        .run(
            &format!("kill -{signal} -- {targets}"),
            command_environment(environment),
            None,
        )
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(error)
            if error.downcast_ref::<E2bCommandExit>().is_some()
                || error.downcast_ref::<E2bSandboxNotFound>().is_some() =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}
