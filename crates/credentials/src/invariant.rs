//! Credential commit-event lifecycle invariant.

use std::sync::Arc;

use seekdeep_cordis::{EventOptions, EventReply};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

use crate::{CREDENTIALS, CredentialRef};

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "credentials-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-credentials";

/// Registers the committed-update lifecycle check.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(std::iter::empty::<String>(), |context, failure| async move {
            let listener_context = context.clone();
            context.events().on_sync(
                &context,
                "credentials/updated",
                move |_, args| {
                    let reference = args
                        .get::<CredentialRef>(0)
                        .ok_or_else(|| anyhow::anyhow!("credentials/updated lacks a reference"))?;
                    if listener_context.get(CREDENTIALS).is_none() {
                        return Err(failure
                            .fail(format!(
                                "credentials/updated for \"{reference}\" emitted without a live credentials service"
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
