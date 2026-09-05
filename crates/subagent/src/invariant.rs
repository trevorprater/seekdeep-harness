//! Package-owned subagent registry and lifecycle invariants.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use parking_lot::Mutex;
use seekdeep_cordis::{DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_invariants::{InvariantFailure, InvariantInstaller, InvariantRegistry};

use crate::types::{SubagentProvider, SubagentRunEndInfo, SubagentRunInfo};

const PACKAGE_NAME: &str = "seekdeep-subagent";

/// Registers the subagent invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<seekdeep_invariants::InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["subagents"], move |context, fail| {
            Box::pin(async move {
                let state = Arc::new(Mutex::new(InvariantState {
                    providers: HashSet::new(),
                    runs: HashMap::new(),
                }));
                context.events().on_sync(
                    &context,
                    "internal/dispatch",
                    move |_, args| {
                        let Some(mode) = args.get::<DispatchMode>(0) else {
                            return Ok(EventReply::Undefined);
                        };
                        if mode.as_ref() != &DispatchMode::Emit {
                            return Ok(EventReply::Undefined);
                        }
                        let Some(name) = args.get::<String>(1) else {
                            return Ok(EventReply::Undefined);
                        };
                        let Some(dispatch_args) = args.get::<EventArgs>(2) else {
                            return Ok(EventReply::Undefined);
                        };
                        validate_dispatch(name.as_str(), dispatch_args.as_ref(), &state, &fail)?;
                        Ok(EventReply::Undefined)
                    },
                    global_events(),
                )?;
                Ok(())
            })
        }),
    )
}

#[derive(Default)]
struct InvariantState {
    providers: HashSet<String>,
    runs: HashMap<String, SubagentRunInfo>,
}

fn reject(failure: &InvariantFailure, message: impl Into<String>) -> anyhow::Result<()> {
    Err(failure.fail(message).into())
}

fn validate_dispatch(
    name: &str,
    args: &EventArgs,
    state: &Arc<Mutex<InvariantState>>,
    fail: &InvariantFailure,
) -> anyhow::Result<()> {
    let mut state = state.lock();
    match name {
        "subagent/provider-added" => {
            let Some(provider) = args.get::<Arc<dyn SubagentProvider>>(0) else {
                reject(fail, "subagent/provider-added lacks its provider")?;
                return Ok(());
            };
            if provider.name().is_empty() {
                reject(fail, "subagent provider names must be non-empty")?;
            }
            if state.providers.contains(provider.name()) {
                reject(
                    fail,
                    format!("subagent/provider-added repeated {:?}", provider.name()),
                )?;
            }
            state.providers.insert(provider.name().to_owned());
        }
        "subagent/provider-removed" => {
            let Some(provider_name) = args.get::<String>(0) else {
                reject(fail, "subagent/provider-removed lacks its name")?;
                return Ok(());
            };
            if !state.providers.contains(provider_name.as_str()) {
                reject(
                    fail,
                    format!("subagent/provider-removed names unknown provider {provider_name:?}"),
                )?;
            }
            state.providers.remove(provider_name.as_str());
        }
        "subagent/start" => {
            let Some(info) = args.get::<SubagentRunInfo>(0) else {
                reject(fail, "subagent/start lacks its info")?;
                return Ok(());
            };
            if info.provider.is_empty()
                || info.run_id.as_str().is_empty()
                || info.id.as_str().is_empty()
            {
                reject(
                    fail,
                    "subagent/start provider, runId, and child id must be non-empty",
                )?;
            }
            if state.runs.contains_key(info.run_id.as_str()) {
                reject(
                    fail,
                    format!("subagent/start repeated run id {:?}", info.run_id),
                )?;
            }
            state
                .runs
                .insert(info.run_id.as_str().to_owned(), info.as_ref().clone());
        }
        "subagent/end" => {
            let Some(info) = args.get::<SubagentRunEndInfo>(0) else {
                reject(fail, "subagent/end lacks its info")?;
                return Ok(());
            };
            let Some(start) = state.runs.get(info.run_id.as_str()) else {
                reject(
                    fail,
                    format!(
                        "subagent/end has no matching subagent/start for run {:?}",
                        info.run_id
                    ),
                )?;
                return Ok(());
            };
            if start.provider != info.provider || start.id != info.id || start.local != info.local {
                reject(
                    fail,
                    format!(
                        "subagent/end identity diverges from subagent/start for run {:?}",
                        info.run_id
                    ),
                )?;
            }
            state.runs.remove(info.run_id.as_str());
        }
        _ => {}
    }
    Ok(())
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}
