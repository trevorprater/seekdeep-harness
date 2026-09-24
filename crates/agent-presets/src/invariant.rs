//! Package-owned standing-generation and model-address invariants.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_system_prompt::{AssembleContext, PromptAssembly};

use crate::AGENT_PRESETS;

const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-agent-presets";

/// Registers continuous service-leak and unjoined-Agent checks.
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
            let roster = context
                .get(AGENT_PRESETS)
                .ok_or_else(|| anyhow::anyhow!("agent preset invariant requires agentPresets"))?;

            let leak_roster = Arc::downgrade(&roster);
            let leak_failure = failure.clone();
            context.on_service_change_checked(move |service_name| {
                let Some(roster) = leak_roster.upgrade() else {
                    return Ok(());
                };
                if let Some((preset, leaked)) = roster.leaked_standing_services().into_iter().next()
                {
                    return Err(leak_failure
                        .fail(format!(
                            "preset \"{preset}\" published process-global service(s) [{}] after its mount was audited (observed while notifying \"{service_name}\") — a preset service must sit behind an `isolate` realm or move to the host composition",
                            leaked.join(", ")
                        ))
                        .into());
                }
                Ok(())
            })?;

            let assembly_roster = roster;
            context.events().on_waterfall(
                &context,
                "system-prompt/assemble",
                move |_, args, next| {
                    let roster = assembly_roster.clone();
                    let failure = failure.clone();
                    Box::pin(async move {
                        args.get::<PromptAssembly>(0).ok_or_else(|| {
                            anyhow::anyhow!("system-prompt/assemble lacks its assembly")
                        })?;
                        let assemble_context = args
                            .get::<AssembleContext>(1)
                            .ok_or_else(|| anyhow::anyhow!("system-prompt/assemble lacks its context"))?;
                        if !roster.roots().is_empty()
                            && let Some(session) = &assemble_context.agent_session
                            && assemble_context
                                .scope
                                .and_then(|scope| roster.composed_preset_for_scope(scope))
                                .is_none()
                        {
                            return Err(failure
                                .fail(format!(
                                    "agent \"{}\" addressed a model without joining any agent preset while a roster is composed; its tools, prompt sections, and skill catalog resolve against the empty global layer",
                                    session.id()
                                ))
                                .into());
                        }
                        next.run().await
                    })
                },
                seekdeep_cordis::EventOptions::default(),
            )?;
            Ok(())
        }),
    )
}
