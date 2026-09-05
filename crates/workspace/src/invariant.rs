//! Workspace cache-to-domain invariant companion.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use seekdeep_storage_domain::DomainChanged;

use crate::{WORKSPACE_REGISTRY, WorkspaceId};

const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-workspace";

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "workspace-invariant";

/// Registers the registry-cache ownership invariant.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["workspaceRegistry"], install),
    )
}

async fn install(context: Context, failure: InvariantFailure) -> anyhow::Result<()> {
    context.events().on_sync(
        &context,
        "domain/changed",
        move |context, args| validate(&context, &args, &failure),
        EventOptions {
            global: true,
            ..EventOptions::default()
        },
    )?;
    Ok(())
}

fn validate(
    context: &Context,
    args: &EventArgs,
    failure: &InvariantFailure,
) -> anyhow::Result<EventReply> {
    let change = args
        .get::<DomainChanged>(0)
        .ok_or_else(|| anyhow::anyhow!("domain/changed lacks change payload"))?;
    if change.domain() != "workspace" || change.table() != "workspaces" {
        return Ok(EventReply::Undefined);
    }
    let registry = context
        .get(WORKSPACE_REGISTRY)
        .ok_or_else(|| failure.fail("workspaceRegistry service is absent"))?;
    let id = WorkspaceId::new(change.key());
    match &*change {
        DomainChanged::Deleted { .. } if registry.get(&id).is_some() => Err(failure
            .fail(format!(
                "workspace record '{id}' was deleted while the registry cache still publishes it — some write path bypassed ctx.workspaceRegistry"
            ))
            .into()),
        DomainChanged::Put { .. } if registry.get(&id).is_none() => Err(failure
            .fail(format!(
                "workspace record '{id}' landed durably but the registry cache holds no entity for it — the cache and the domain table have diverged"
            ))
            .into()),
        DomainChanged::Deleted { .. } | DomainChanged::Put { .. } => Ok(EventReply::Undefined),
    }
}
