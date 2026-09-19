//! Process-local model-defined Cordis package registry and lifecycle types.

mod inspect_registry;
mod registry;
mod remote;
mod runner;
mod sandbox;

pub use inspect_registry::{
    CORDIS_INSPECT, CordisInspectRegistryService, HostCordisInspectProviderRegistration,
    HostCordisInspectQuery, HostCordisInspectQueryContext,
};
pub use registry::{
    DynamicCordisCode, DynamicCordisDefineReceipt, DynamicCordisDefineRequest,
    DynamicCordisPackageInspection, DynamicCordisPhysicalRun, DynamicCordisPluginInspection,
    DynamicCordisPluginSelector, DynamicCordisPluginState, DynamicCordisReference,
    DynamicCordisRegistry, DynamicCordisSnapshotActiveRun, DynamicCordisSnapshotRow,
};
pub use runner::{DYNAMIC_CORDIS_RUNNER, DynamicCordisRunner, DynamicCordisRunnerConfig};
pub use sandbox::{
    HOST_BUILTIN_INSPECTION, HostBuiltinInspection, SandboxCodeHalf, SandboxSyntaxError,
    syntax_error_context, validate_code, validate_host_code,
};
pub use seekdeep_cordis_dynamic_types::*;

/// Source: the package's generated `./typert` Host artifact (`cargo xtask typert-host-artifacts`).
/// The package's embedded Host typert artifact (`typert.host.json`), registered at load by
/// the shipping host and otherwise at plugin apply.
pub const TYPERT_HOST_ARTIFACT: &str = include_str!("../typert.host.json");

/// Cordis plugin entrypoint for production composition.
#[must_use]
pub fn plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new("cordis-host-runner", ["tools"], |context, config| {
        Box::pin(async move {
            let config = if config.is_null() {
                DynamicCordisRunnerConfig::default()
            } else {
                serde_json::from_value(config)?
            };
            context.own(seekdeep_typert_host_artifact::register(
                &context,
                TYPERT_HOST_ARTIFACT,
            )?)?;
            DynamicCordisRunner::try_install(&context, config)?;
            seekdeep_api_gateway::register_invocable_service_if_available(
                &context,
                DYNAMIC_CORDIS_RUNNER,
            )?;
            Ok(())
        })
    })
}
