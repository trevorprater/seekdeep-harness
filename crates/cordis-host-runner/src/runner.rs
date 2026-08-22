//! Definition, ownership, source-free inventory, and explicit inspection.

use std::{collections::HashMap, sync::Arc};

use seekdeep_agent::AGENTS;
use seekdeep_cordis::{
    Context, EventArgs, EventOptions, EventReply, Plugin, PluginFiber, ServiceKey,
};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, SessionId, UserMessage};
use seekdeep_loader::{
    DynamicHostGuardFailure, DynamicHostRuntime, compile_dynamic_host_runtime_named,
};
use serde::{Deserialize, Serialize};

use crate::{
    ApprovalRequestId, CORDIS_INSPECT, CordisDiagnosticPhase, CordisDynamicPackageId,
    CordisDynamicPluginId, CordisDynamicPluginRunId, CordisErrorDetails, CordisHalfState,
    CordisHalfStatus, CordisInspectProviderManifest, CordisInspectQueryResolution,
    CordisInspectRegistryService, CordisInspectRequestId, CordisInspectResolveAck, CordisRunStatus,
    DynamicCordisClientSource, DynamicCordisCode, DynamicCordisDefineReceipt,
    DynamicCordisDefineRequest, DynamicCordisDefinition, DynamicCordisHostHalfResult,
    DynamicCordisInventoryPackage, DynamicCordisInventoryRow, DynamicCordisInvokeErrorCode,
    DynamicCordisInvokeResult, DynamicCordisPackage, DynamicCordisPackageInspection,
    DynamicCordisPhysicalRun, DynamicCordisPluginInspection, DynamicCordisPluginSelector,
    DynamicCordisPluginState, DynamicCordisReference, DynamicCordisRegistry,
    DynamicCordisRenderFailure, DynamicCordisRequestResolved, DynamicCordisResolveAck,
    DynamicCordisRetracted, DynamicCordisRunAttempt, DynamicCordisRunFailureReason,
    DynamicCordisRunMode, DynamicCordisRunRequest, DynamicCordisRunResolution,
    DynamicCordisRunResponse, DynamicCordisRunSuccessStatus, DynamicCordisSnapshotActiveRun,
    DynamicCordisSnapshotRow, DynamicCordisStopFailureReason, DynamicCordisStopResponse,
    RequestRunOutcome, SandboxCodeHalf, validate_code,
};

/// Cordis slot for the process-local dynamic Plugin authority.
pub const DYNAMIC_CORDIS_RUNNER: ServiceKey<DynamicCordisRunner> =
    ServiceKey::new("dynamicCordisRunner");

/// Dynamic Host Runner configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisRunnerConfig {
    /// Maximum synchronous interpreter work budget in milliseconds.
    pub vm_timeout_ms: u64,
}

impl Default for DynamicCordisRunnerConfig {
    fn default() -> Self {
        Self {
            vm_timeout_ms: 5_000,
        }
    }
}

/// Process-local authority over model-defined plugins and immutable versions.
#[derive(Debug)]
pub struct DynamicCordisRunner {
    registry: Arc<DynamicCordisRegistry>,
    inspect_registry: Option<Arc<CordisInspectRegistryService>>,
    starting: parking_lot::Mutex<
        HashMap<CordisDynamicPluginId, Arc<tokio::sync::OnceCell<DynamicCordisHostHalfResult>>>,
    >,
    group: parking_lot::Mutex<Option<Arc<PluginFiber>>>,
    context: Option<Context>,
    vm_timeout_ms: u64,
}

enum HostPlan {
    Attach(DynamicCordisHostHalfResult),
    Start {
        context: Context,
        plugin: Arc<parking_lot::Mutex<DynamicCordisPluginState>>,
        definition: DynamicCordisDefinition,
    },
}

enum AttachPolicy {
    Never,
    SamePackage,
    ExactRun(CordisDynamicPluginRunId),
}

struct HostStartOptions {
    attach_policy: AttachPolicy,
    forced_run_id: Option<CordisDynamicPluginRunId>,
    started_for_request: Option<ApprovalRequestId>,
}

struct FreshHostPlan {
    context: Context,
    plugin: Arc<parking_lot::Mutex<DynamicCordisPluginState>>,
    definition: DynamicCordisDefinition,
    plugin_id: CordisDynamicPluginId,
    package_id: CordisDynamicPackageId,
    mode: DynamicCordisRunMode,
    forced_run_id: Option<CordisDynamicPluginRunId>,
    started_for_request: Option<ApprovalRequestId>,
}

struct ClientFailure<'a> {
    reason: DynamicCordisRunFailureReason,
    plugin_run_id: Option<&'a CordisDynamicPluginRunId>,
    started_here: Option<bool>,
    message: Option<&'a str>,
    stack: Option<&'a str>,
    request_id: Option<&'a ApprovalRequestId>,
}

struct StartedHost {
    fiber: Option<Arc<PluginFiber>>,
    runtime: Option<Arc<DynamicHostRuntime>>,
}

struct StartedHostCommit {
    started: StartedHost,
    plugin_run_id: CordisDynamicPluginRunId,
    package_id: CordisDynamicPackageId,
    waiting_for: Vec<String>,
    started_for_request: Option<ApprovalRequestId>,
}

impl StartedHost {
    fn missing_services(&self, context: &Context) -> Vec<String> {
        self.fiber
            .as_ref()
            .map_or_else(Vec::new, |fiber| missing_services(context, fiber))
    }
}

impl DynamicCordisRunner {
    /// Creates a runner over an empty registry.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: DynamicCordisRegistry::new(),
            inspect_registry: None,
            starting: parking_lot::Mutex::new(HashMap::new()),
            group: parking_lot::Mutex::new(None),
            context: None,
            vm_timeout_ms: 5_000,
        })
    }

    /// Creates a runner capable of starting Host halves in `context`.
    ///
    /// # Panics
    ///
    /// Panics when `cordisInspect` is already provided or the owning context
    /// is inactive. The application installer must install this service once.
    #[must_use]
    pub fn install(context: &Context, vm_timeout_ms: u64) -> Arc<Self> {
        Self::try_install(context, DynamicCordisRunnerConfig { vm_timeout_ms })
            .expect("dynamicCordisRunner must be valid and uniquely installed")
    }

    /// Validates configuration and installs the Runner plus its control Services.
    ///
    /// # Errors
    ///
    /// Rejects a zero timeout, duplicate Services, or inactive lifecycle owner.
    pub fn try_install(
        context: &Context,
        config: DynamicCordisRunnerConfig,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            config.vm_timeout_ms > 0,
            "cordis-host-runner vmTimeoutMs must be at least 1"
        );
        let inspect_registry = CordisInspectRegistryService::new(context.clone());
        context.provide(CORDIS_INSPECT, inspect_registry.clone())?;
        let runner = Arc::new(Self {
            registry: DynamicCordisRegistry::new(),
            inspect_registry: Some(inspect_registry),
            starting: parking_lot::Mutex::new(HashMap::new()),
            group: parking_lot::Mutex::new(None),
            context: Some(context.clone()),
            vm_timeout_ms: config.vm_timeout_ms,
        });
        let weak = Arc::downgrade(&runner);
        context.events().on_sync(
            context,
            "cordis/dynamic-host-guard-failure",
            move |_, args| {
                if let (Some(runner), Some(failure)) =
                    (weak.upgrade(), args.get::<DynamicHostGuardFailure>(0))
                {
                    runner.report_host_guard_failure(&failure);
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        context.provide(DYNAMIC_CORDIS_RUNNER, runner.clone())?;
        Ok(runner)
    }

    /// Returns the underlying process-local registry.
    #[must_use]
    pub fn registry(&self) -> &Arc<DynamicCordisRegistry> {
        &self.registry
    }

    /// Returns the process-wide inspection registry when Host services are installed.
    #[must_use]
    pub fn inspect_registry(&self) -> Option<&Arc<CordisInspectRegistryService>> {
        self.inspect_registry.as_ref()
    }

    /// Replaces the mirrored browser inspection directory.
    ///
    /// # Errors
    ///
    /// Rejects invalid manifests or a registry-only runner.
    pub fn sync_inspect_manifest(
        &self,
        providers: &[CordisInspectProviderManifest],
    ) -> anyhow::Result<()> {
        self.inspect_registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("dynamic Cordis runner has no Host context"))?
            .sync_client_manifest(providers)
    }

    /// Claims one pending browser inspection query for its owning Session.
    #[must_use]
    pub fn resolve_inspect_query(
        &self,
        session: &SessionId,
        request_id: &CordisInspectRequestId,
        resolution: CordisInspectQueryResolution,
    ) -> CordisInspectResolveAck {
        self.inspect_registry
            .as_ref()
            .map_or(CordisInspectResolveAck { accepted: false }, |registry| {
                registry.resolve_client_query(session, request_id, resolution)
            })
    }

    /// Defines a first package or appends an immutable version.
    ///
    /// Syntax validation finishes before registry mutation.
    ///
    /// # Errors
    ///
    /// Rejects invalid metadata, absent code, syntax errors, missing plugins,
    /// and cross-session modification.
    pub fn define(
        &self,
        request: DynamicCordisDefineRequest,
    ) -> anyhow::Result<DynamicCordisDefineReceipt> {
        let name = request.name.trim().to_owned();
        let purpose = request.purpose.trim().to_owned();
        anyhow::ensure!(!name.is_empty(), "cordis_define needs a non-empty `name`");
        anyhow::ensure!(
            !purpose.is_empty(),
            "cordis_define needs a non-empty `purpose`"
        );
        anyhow::ensure!(
            request.code.host.is_some() || request.code.client.is_some(),
            "cordis_define needs `code.host`, `code.client`, or both"
        );
        if let Some(code) = &request.code.host {
            validate_code(code, SandboxCodeHalf::Host).map_err(anyhow::Error::new)?;
        }
        if let Some(code) = &request.code.client {
            validate_code(code, SandboxCodeHalf::Client).map_err(anyhow::Error::new)?;
        }

        let plugin = match request.plugin {
            DynamicCordisPluginSelector::New { id_prefix } => {
                let prefix = id_prefix.trim();
                anyhow::ensure!(
                    (3..=6).contains(&prefix.len())
                        && prefix.bytes().all(|byte| byte.is_ascii_lowercase()),
                    "cordis_define `plugin.idPrefix` must contain 3–6 lowercase English letters"
                );
                let plugin_id = self.registry.mint_plugin_id(prefix);
                let state =
                    DynamicCordisPluginState::new(plugin_id.clone(), request.session_id.clone());
                self.registry.add(state);
                self.registry
                    .get(&plugin_id)
                    .ok_or_else(|| anyhow::anyhow!("new dynamic plugin was not retained"))?
            }
            DynamicCordisPluginSelector::Existing { plugin_id } => {
                let plugin = self
                    .registry
                    .get(&plugin_id)
                    .ok_or_else(|| anyhow::anyhow!(missing_plugin_message(&plugin_id)))?;
                anyhow::ensure!(
                    plugin.lock().session_id == request.session_id,
                    missing_plugin_message(&plugin_id)
                );
                plugin
            }
        };
        let package_id = self.registry.mint_package_id();
        let has_host_half = request.code.host.is_some();
        let has_client_half = request.code.client.is_some();
        plugin.lock().packages.insert(
            package_id.clone(),
            DynamicCordisDefinition {
                package_id: package_id.clone(),
                name: name.clone(),
                purpose: purpose.clone(),
                host_code: request.code.host,
                client_code: request.code.client,
            },
        );
        let plugin_id = plugin.lock().plugin_id.clone();
        Ok(DynamicCordisDefineReceipt {
            plugin_id,
            package_id,
            name,
            purpose,
            has_host_half,
            has_client_half,
        })
    }

    /// Lists every plugin without returning source code.
    #[must_use]
    pub fn inventory(&self) -> Vec<DynamicCordisInventoryRow> {
        self.registry
            .all()
            .into_iter()
            .map(|plugin| {
                let plugin = plugin.lock();
                DynamicCordisInventoryRow {
                    plugin_id: plugin.plugin_id.clone(),
                    agent_id: plugin.session_id.clone(),
                    packages: package_summaries(&plugin),
                    current_package_id: plugin.current_package_id.clone(),
                    next_package_id: plugin.next_package_id.clone(),
                    active_run: plugin.active_run.clone(),
                    latest_run: plugin.latest_run.clone(),
                }
            })
            .collect()
    }

    /// Returns one Session's Host-rich state for inspection and rendering.
    #[must_use]
    pub fn snapshot(&self, session: &SessionId) -> Vec<DynamicCordisSnapshotRow> {
        self.registry
            .of_session(session)
            .into_iter()
            .map(|plugin| {
                let plugin = plugin.lock();
                DynamicCordisSnapshotRow {
                    plugin_id: plugin.plugin_id.clone(),
                    current_package_id: plugin.current_package_id.clone(),
                    next_package_id: plugin.next_package_id.clone(),
                    packages: package_summaries(&plugin),
                    active_run: plugin
                        .run
                        .as_ref()
                        .map(|run| DynamicCordisSnapshotActiveRun {
                            plugin_run_id: run.plugin_run_id.clone(),
                            package_id: run.package_id.clone(),
                            fiber: run.fiber.clone(),
                            handlers: run
                                .host_runtime
                                .as_ref()
                                .map_or_else(Vec::new, |runtime| runtime.handler_names().to_vec()),
                            render_failure: run.render_failure.clone(),
                        }),
                    latest_run: plugin.latest_run.clone(),
                }
            })
            .collect()
    }

    /// Returns the preferred modification base for one owned plugin.
    #[must_use]
    pub fn reference(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
    ) -> Option<DynamicCordisReference> {
        let plugin = self.registry.get(plugin_id)?;
        let plugin = plugin.lock();
        if plugin.session_id != *session {
            return None;
        }
        let package_id = plugin
            .next_package_id
            .clone()
            .or_else(|| plugin.current_package_id.clone())
            .or_else(|| plugin.packages.last().map(|(id, _)| id.clone()))?;
        let definition = plugin.packages.get(&package_id)?;
        Some(DynamicCordisReference {
            plugin_id: plugin.plugin_id.clone(),
            package_id,
            name: definition.name.clone(),
            purpose: definition.purpose.clone(),
            current_package_id: plugin.current_package_id.clone(),
            next_package_id: plugin.next_package_id.clone(),
            active_run: plugin.active_run.clone(),
            latest_run: plugin.latest_run.clone(),
        })
    }

    /// Lists source-free plugin inspections for one session.
    #[must_use]
    pub fn list_plugins(&self, session: &SessionId) -> Vec<DynamicCordisPluginInspection> {
        self.registry
            .of_session(session)
            .into_iter()
            .filter_map(|plugin| {
                let plugin_id = plugin.lock().plugin_id.clone();
                self.inspect_plugin(session, &plugin_id).ok()
            })
            .collect()
    }

    /// Inspects one owned plugin without source code.
    ///
    /// # Errors
    ///
    /// Returns the memory-only missing-plugin diagnostic.
    pub fn inspect_plugin(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
    ) -> anyhow::Result<DynamicCordisPluginInspection> {
        let plugin = self.owned(session, plugin_id)?;
        let reference = self
            .reference(session, plugin_id)
            .ok_or_else(|| anyhow::anyhow!("dynamic plugin \"{plugin_id}\" has no package"))?;
        let plugin = plugin.lock();
        Ok(DynamicCordisPluginInspection {
            reference,
            packages: package_summaries(&plugin),
        })
    }

    /// Reads one exact owned package with source.
    ///
    /// # Errors
    ///
    /// Returns missing-plugin, ownership, or missing-package diagnostics.
    pub fn inspect_package(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
    ) -> anyhow::Result<DynamicCordisPackageInspection> {
        let plugin = self.owned(session, plugin_id)?;
        let mut reference = self
            .reference(session, plugin_id)
            .ok_or_else(|| anyhow::anyhow!("dynamic plugin \"{plugin_id}\" has no package"))?;
        let plugin = plugin.lock();
        let definition = plugin.packages.get(package_id).ok_or_else(|| {
            anyhow::anyhow!(
                "dynamic package \"{package_id}\" does not exist on plugin \"{plugin_id}\""
            )
        })?;
        reference.package_id = package_id.clone();
        reference.name.clone_from(&definition.name);
        reference.purpose.clone_from(&definition.purpose);
        Ok(DynamicCordisPackageInspection {
            reference,
            code: DynamicCordisCode {
                host: definition.host_code.clone(),
                client: definition.client_code.clone(),
            },
        })
    }

    /// Records the last Client render failure for one exact active run.
    pub fn report_render_failure(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        plugin_run_id: &CordisDynamicPluginRunId,
        failure: &DynamicCordisRenderFailure,
    ) {
        let Ok(plugin) = self.owned(session, plugin_id) else {
            return;
        };
        let mut plugin = plugin.lock();
        let Some(run) = &mut plugin.run else {
            return;
        };
        if run.plugin_run_id != *plugin_run_id {
            return;
        }
        let should_steer = run.render_failure.is_none();
        let package_id = run.package_id.clone();
        run.render_failure = Some(failure.clone());
        if let Some(attempt) = &mut plugin.latest_run
            && attempt.plugin_run_id == *plugin_run_id
        {
            attempt.error = Some(crate::CordisRunDiagnostic {
                phase: CordisDiagnosticPhase::ClientRender,
                message: failure.message.clone(),
                stack: failure.stack.clone(),
                plugin_id: plugin_id.clone(),
                package_id: attempt.package_id.clone(),
                plugin_run_id: plugin_run_id.clone(),
            });
            attempt.client.status = CordisHalfStatus::Failed;
            attempt.client.error = Some(failure.message.clone());
            attempt.status = CordisRunStatus::Failed;
        }
        drop(plugin);
        if should_steer {
            self.steer_render_failure(session, plugin_id, &package_id, plugin_run_id, failure);
        }
    }

    /// Reports one post-activation Client guard failure for the exact active run.
    pub fn report_client_guard_failure(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        plugin_run_id: &CordisDynamicPluginRunId,
        failure: &CordisErrorDetails,
    ) {
        let key = format!("Client\0guard\0{}", failure.message);
        if self.claim_runtime_failure(plugin_id, plugin_run_id, &key) {
            self.steer_guard_failure(session, plugin_id, plugin_run_id, "Client", failure);
        }
    }

    fn report_host_guard_failure(&self, failure: &DynamicHostGuardFailure) {
        let plugin_id = CordisDynamicPluginId::new(failure.plugin_id.clone());
        let Some(plugin) = self.registry.get(&plugin_id) else {
            return;
        };
        let (session, plugin_run_id) = {
            let plugin = plugin.lock();
            let Some(run) = &plugin.run else {
                return;
            };
            (plugin.session_id.clone(), run.plugin_run_id.clone())
        };
        let details = CordisErrorDetails {
            message: failure.message.clone(),
            stack: None,
        };
        let key = format!("Host\0guard\0{}", failure.message);
        if self.claim_runtime_failure(&plugin_id, &plugin_run_id, &key) {
            self.steer_guard_failure(&session, &plugin_id, &plugin_run_id, "Host", &details);
        }
    }

    fn resolve_host_plan(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
        mode: DynamicCordisRunMode,
        attach_policy: &AttachPolicy,
    ) -> Result<HostPlan, anyhow::Error> {
        let context = self
            .context
            .clone()
            .ok_or_else(|| anyhow::anyhow!("dynamic Cordis runner has no Host context"))?;
        let plugin = self.owned(session, plugin_id)?;
        let definition = {
            let plugin = plugin.lock();
            let definition = plugin.packages.get(package_id).cloned().ok_or_else(|| {
                anyhow::anyhow!("plugin \"{plugin_id}\" has no package \"{package_id}\"")
            })?;
            if let Some(message) = invalid_mode_message(
                plugin_id,
                package_id,
                mode,
                plugin.current_package_id.as_ref(),
            ) {
                anyhow::bail!(message);
            }
            if let Some(run) = &plugin.run
                && run.package_id == *package_id
                && match attach_policy {
                    AttachPolicy::Never => false,
                    AttachPolicy::SamePackage => true,
                    AttachPolicy::ExactRun(expected) => run.plugin_run_id == *expected,
                }
            {
                return Ok(HostPlan::Attach(DynamicCordisHostHalfResult::Success {
                    plugin_id: plugin_id.clone(),
                    package_id: package_id.clone(),
                    plugin_run_id: run.plugin_run_id.clone(),
                    waiting_for: run
                        .fiber
                        .as_ref()
                        .map_or_else(Vec::new, |fiber| missing_services(&context, fiber)),
                    started_here: false,
                }));
            }
            definition
        };
        Ok(HostPlan::Start {
            context,
            plugin,
            definition,
        })
    }

    async fn start_host_fiber(
        &self,
        context: &Context,
        plugin: &Arc<parking_lot::Mutex<DynamicCordisPluginState>>,
        plugin_id: &CordisDynamicPluginId,
        code: Option<String>,
    ) -> Result<StartedHost, anyhow::Error> {
        let Some(code) = code else {
            return Ok(StartedHost {
                fiber: None,
                runtime: None,
            });
        };
        let compiled =
            compile_dynamic_host_runtime_named(&code, self.vm_timeout_ms, plugin_id.as_str())
                .inspect_err(|error| record_host_failure(plugin, &error.to_string()))?;
        let (compiled, runtime) = compiled.into_parts();
        let group = {
            let mut group = self.group.lock();
            if let Some(group) = group.as_ref() {
                group.clone()
            } else {
                let created = context.plugin(
                    Plugin::new("cordis-dynamic", std::iter::empty::<String>(), |_, _| {
                        Box::pin(async { Ok(()) })
                    }),
                    serde_json::Value::Null,
                )?;
                *group = Some(created.clone());
                created
            }
        };
        group.await_settled().await?;
        let fiber = group
            .context()
            .plugin(compiled, serde_json::Value::Null)
            .inspect_err(|error| record_host_failure(plugin, &error.to_string()))?;
        if let Err(error) = fiber.await_settled().await {
            let _ = fiber.dispose().await;
            let message = error
                .to_string()
                .replace("is already provided in this scope", "is already registered");
            let message = if message.contains("already registered") {
                format!(
                    "{message} — to REPLACE something an earlier dynamic package registered, first cordis_stop that package's id, then run the new version."
                )
            } else {
                message
            };
            record_host_failure(plugin, &message);
            return Err(anyhow::anyhow!(message));
        }
        Ok(StartedHost {
            fiber: Some(fiber),
            runtime: Some(runtime),
        })
    }

    /// Starts or attaches to one exact Host half.
    ///
    /// # Errors
    ///
    /// Returns ownership and missing-package failures as structured results;
    /// registry installation is required before Host activation.
    pub async fn run_host_half(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
        mode: DynamicCordisRunMode,
    ) -> DynamicCordisHostHalfResult {
        let plugin = match self.owned(session, plugin_id) {
            Ok(plugin) => plugin,
            Err(error) => return host_failure(&error),
        };
        let has_client = {
            let plugin = plugin.lock();
            let Some(definition) = plugin.packages.get(package_id) else {
                return host_failure(&anyhow::anyhow!(
                    "plugin \"{plugin_id}\" has no package \"{package_id}\""
                ));
            };
            if let Some(message) = invalid_mode_message(
                plugin_id,
                package_id,
                mode,
                plugin.current_package_id.as_ref(),
            ) {
                return host_failure(&anyhow::anyhow!(message));
            }
            definition.client_code.is_some()
        };
        if let Some(request_id) = self.registry.pending_request_for(plugin_id) {
            return host_failure(&anyhow::anyhow!(
                "dynamic plugin \"{plugin_id}\" has pending run request {request_id}"
            ));
        }
        if has_client {
            plugin
                .lock()
                .approved_client_packages
                .insert(package_id.clone());
        }
        self.run_host_half_with_id(
            session,
            plugin_id,
            package_id,
            mode,
            HostStartOptions {
                attach_policy: AttachPolicy::SamePackage,
                forced_run_id: None,
                started_for_request: None,
            },
        )
        .await
    }

    async fn run_host_half_with_id(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
        mode: DynamicCordisRunMode,
        options: HostStartOptions,
    ) -> DynamicCordisHostHalfResult {
        let attach_policy = options.attach_policy;
        let (context, plugin, definition) =
            match self.resolve_host_plan(session, plugin_id, package_id, mode, &attach_policy) {
                Ok(HostPlan::Attach(result)) => return result,
                Ok(HostPlan::Start {
                    context,
                    plugin,
                    definition,
                }) => (context, plugin, definition),
                Err(error) => return host_failure(&error),
            };
        let flight = {
            let mut starting = self.starting.lock();
            starting
                .entry(plugin_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
                .clone()
        };
        let result = flight
            .get_or_init(|| {
                self.start_host_fresh(FreshHostPlan {
                    context,
                    plugin,
                    definition,
                    plugin_id: plugin_id.clone(),
                    package_id: package_id.clone(),
                    mode,
                    forced_run_id: options.forced_run_id,
                    started_for_request: options.started_for_request,
                })
            })
            .await
            .clone();
        let mut starting = self.starting.lock();
        if starting
            .get(plugin_id)
            .is_some_and(|active| Arc::ptr_eq(active, &flight))
        {
            starting.remove(plugin_id);
        }
        result
    }

    async fn start_host_fresh(&self, plan: FreshHostPlan) -> DynamicCordisHostHalfResult {
        let FreshHostPlan {
            context,
            plugin,
            definition,
            plugin_id,
            package_id,
            mode,
            forced_run_id,
            started_for_request,
        } = plan;
        self.retract(&plugin_id, &plugin).await;
        let plugin_run_id = forced_run_id.unwrap_or_else(|| self.registry.mint_plugin_run_id());
        {
            let mut plugin = plugin.lock();
            plugin.next_package_id = Some(package_id.clone());
            if let Some(attempt) = &mut plugin.latest_run
                && attempt.plugin_run_id == plugin_run_id
            {
                attempt.status = CordisRunStatus::StartingHost;
                if attempt.host.status != CordisHalfStatus::Absent {
                    attempt.host = CordisHalfState {
                        status: CordisHalfStatus::Pending,
                        waiting_for: Vec::new(),
                        error: None,
                    };
                }
            } else {
                plugin.latest_run = Some(pending_attempt(
                    plugin_run_id.clone(),
                    package_id.clone(),
                    mode,
                    definition.host_code.is_some(),
                    definition.client_code.is_some(),
                ));
            }
        }
        let started = match self
            .start_host_fiber(&context, &plugin, &plugin_id, definition.host_code.clone())
            .await
        {
            Ok(started) => started,
            Err(error) => return host_failure(&error),
        };
        let waiting_for = started.missing_services(&context);
        record_started_host(
            &plugin,
            &definition,
            StartedHostCommit {
                started,
                plugin_run_id: plugin_run_id.clone(),
                package_id: package_id.clone(),
                waiting_for: waiting_for.clone(),
                started_for_request,
            },
        );
        announce_package(
            &context,
            &plugin_id,
            &package_id,
            &plugin_run_id,
            definition.name,
        );
        DynamicCordisHostHalfResult::Success {
            plugin_id,
            package_id,
            plugin_run_id,
            waiting_for,
            started_here: true,
        }
    }

    /// Starts the Host half authorized by one pending Client request.
    pub async fn run_host_half_for_request(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
        mode: DynamicCordisRunMode,
        request_id: &ApprovalRequestId,
        approve_future_versions: bool,
    ) -> DynamicCordisHostHalfResult {
        let pending = match self.registry.peek_request(request_id) {
            Some(pending)
                if pending.agent_id == *session
                    && pending.plugin_id == *plugin_id
                    && pending.package_id == *package_id
                    && pending.mode == mode =>
            {
                pending
            }
            _ => {
                let error = anyhow::anyhow!(
                    "run request \"{request_id}\" does not authorize {plugin_id}/{package_id}"
                );
                return host_failure(&error);
            }
        };
        let plugin = match self.owned(session, plugin_id) {
            Ok(plugin) => plugin,
            Err(error) => return host_failure(&error),
        };
        {
            let mut plugin = plugin.lock();
            let latest_matches = plugin.latest_run.as_ref().is_some_and(|attempt| {
                attempt.plugin_run_id == pending.plugin_run_id
                    && if pending.requires_approval {
                        attempt.status == CordisRunStatus::AwaitingApproval
                    } else {
                        matches!(
                            attempt.status,
                            CordisRunStatus::StartingHost | CordisRunStatus::ClientPending
                        )
                    }
            });
            if !latest_matches {
                let error = anyhow::anyhow!(
                    "run request \"{request_id}\" no longer identifies the latest run of {plugin_id}"
                );
                return host_failure(&error);
            }
            if pending.requires_approval {
                plugin.approved_client_packages.insert(package_id.clone());
                if approve_future_versions {
                    plugin.client_version_updates_approved = true;
                }
            }
        }
        self.run_host_half_with_id(
            session,
            plugin_id,
            package_id,
            mode,
            HostStartOptions {
                attach_policy: AttachPolicy::ExactRun(pending.plugin_run_id.clone()),
                forced_run_id: Some(pending.plugin_run_id),
                started_for_request: Some(request_id.clone()),
            },
        )
        .await
    }

    /// Returns Client source only for the exact active run.
    ///
    /// # Errors
    ///
    /// Returns ownership, stale-run, or absent-Client diagnostics.
    pub fn get_client_code(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        plugin_run_id: &CordisDynamicPluginRunId,
    ) -> anyhow::Result<DynamicCordisClientSource> {
        let plugin = self.owned(session, plugin_id)?;
        let plugin = plugin.lock();
        let run = plugin.run.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "dynamic plugin \"{plugin_id}\" is not running activation \"{plugin_run_id}\""
            )
        })?;
        anyhow::ensure!(
            run.plugin_run_id == *plugin_run_id,
            "dynamic plugin \"{plugin_id}\" is not running activation \"{plugin_run_id}\""
        );
        let definition = plugin
            .packages
            .get(&run.package_id)
            .ok_or_else(|| anyhow::anyhow!("package \"{}\" is missing", run.package_id))?;
        let code = definition
            .client_code
            .clone()
            .ok_or_else(|| anyhow::anyhow!("package \"{}\" has no Client half", run.package_id))?;
        Ok(DynamicCordisClientSource {
            code,
            name: definition.name.clone(),
            plugin_id: plugin_id.clone(),
            package_id: run.package_id.clone(),
            plugin_run_id: plugin_run_id.clone(),
        })
    }

    /// Applies one browser verdict to the exact active run.
    pub async fn settle_user_run(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        resolution: &DynamicCordisRunResolution,
    ) -> DynamicCordisRunResponse {
        let settled = self
            .settle_activation(session, plugin_id, resolution, None)
            .await;
        self.inject_user_run_outcome(session, plugin_id, &settled);
        settled
    }

    async fn settle_activation(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        resolution: &DynamicCordisRunResolution,
        request_id: Option<&ApprovalRequestId>,
    ) -> DynamicCordisRunResponse {
        let plugin = match self.owned(session, plugin_id) {
            Ok(plugin) => plugin,
            Err(error) => {
                return DynamicCordisRunResponse::Failure {
                    reason: DynamicCordisRunFailureReason::PluginMissing,
                    message: error.to_string(),
                    stack: None,
                };
            }
        };
        match resolution {
            DynamicCordisRunResolution::Success {
                plugin_run_id,
                waiting_for,
            } => self.settle_client_success(
                plugin_id,
                &plugin,
                plugin_run_id,
                waiting_for.as_deref(),
            ),
            DynamicCordisRunResolution::Failure {
                reason,
                plugin_run_id,
                started_here,
                message,
                stack,
            } => {
                self.settle_client_failure(
                    plugin_id,
                    &plugin,
                    ClientFailure {
                        reason: *reason,
                        plugin_run_id: plugin_run_id.as_ref(),
                        started_here: *started_here,
                        message: message.as_deref(),
                        stack: stack.as_deref(),
                        request_id,
                    },
                )
                .await
            }
        }
    }

    fn settle_client_success(
        &self,
        plugin_id: &CordisDynamicPluginId,
        plugin: &Arc<parking_lot::Mutex<DynamicCordisPluginState>>,
        plugin_run_id: &CordisDynamicPluginRunId,
        waiting_for: Option<&[String]>,
    ) -> DynamicCordisRunResponse {
        let mut plugin = plugin.lock();
        let Some(run) = &mut plugin.run else {
            return stale_run_failure(plugin_id, plugin_run_id);
        };
        if run.plugin_run_id != *plugin_run_id {
            return stale_run_failure(plugin_id, plugin_run_id);
        }
        let package_id = run.package_id.clone();
        run.started_for_request = None;
        plugin.current_package_id = Some(package_id.clone());
        plugin.next_package_id = None;
        if let Some(attempt) = &mut plugin.latest_run {
            attempt.client = CordisHalfState {
                status: if waiting_for.is_some_and(|waiting| !waiting.is_empty()) {
                    CordisHalfStatus::Waiting
                } else {
                    CordisHalfStatus::Running
                },
                waiting_for: waiting_for.unwrap_or_default().to_vec(),
                error: None,
            };
            attempt.status = if attempt.host.status == CordisHalfStatus::Waiting
                || attempt.client.status == CordisHalfStatus::Waiting
            {
                CordisRunStatus::Waiting
            } else {
                CordisRunStatus::Running
            };
            attempt.approval_request_id = None;
            attempt.requires_approval = None;
            attempt.error = None;
        }
        DynamicCordisRunResponse::Success {
            status: DynamicCordisRunSuccessStatus::Running,
            plugin_id: plugin_id.clone(),
            package_id,
            plugin_run_id: plugin_run_id.clone(),
            waiting_for: plugin
                .run
                .as_ref()
                .and_then(|run| run.fiber.as_ref())
                .and_then(|fiber| {
                    self.context
                        .as_ref()
                        .map(|context| missing_services(context, fiber))
                })
                .unwrap_or_default(),
            client_waiting_for: waiting_for.map(<[String]>::to_vec),
            current_package_id: plugin.current_package_id.clone(),
            next_package_id: None,
            mode: plugin
                .latest_run
                .as_ref()
                .map_or(DynamicCordisRunMode::Run, |attempt| attempt.mode),
        }
    }

    async fn settle_client_failure(
        &self,
        plugin_id: &CordisDynamicPluginId,
        plugin: &Arc<parking_lot::Mutex<DynamicCordisPluginState>>,
        failure: ClientFailure<'_>,
    ) -> DynamicCordisRunResponse {
        let ClientFailure {
            reason,
            plugin_run_id,
            started_here,
            message,
            stack,
            request_id,
        } = failure;
        if reason == DynamicCordisRunFailureReason::Rejected {
            let message = message.unwrap_or("the run request was declined").to_owned();
            let mut plugin = plugin.lock();
            if let Some(attempt) = &mut plugin.latest_run {
                attempt.status = CordisRunStatus::Rejected;
                attempt.error = Some(crate::CordisRunDiagnostic {
                    phase: CordisDiagnosticPhase::Approval,
                    message: message.clone(),
                    stack: None,
                    plugin_id: plugin_id.clone(),
                    package_id: attempt.package_id.clone(),
                    plugin_run_id: attempt.plugin_run_id.clone(),
                });
                attempt.client = stopped_half();
            }
            return DynamicCordisRunResponse::Failure {
                reason,
                message,
                stack: None,
            };
        }
        let owns_run = plugin.lock().run.as_ref().is_some_and(|run| {
            plugin_run_id == Some(&run.plugin_run_id)
                && request_id
                    .is_none_or(|request| run.started_for_request.as_ref() == Some(request))
                && started_here != Some(false)
        });
        if owns_run {
            self.retract(plugin_id, plugin).await;
        }
        let message = message.map_or_else(|| reason_string(reason), str::to_owned);
        let mut plugin = plugin.lock();
        if let Some(attempt) = &mut plugin.latest_run
            && plugin_run_id.is_none_or(|run| attempt.plugin_run_id == *run)
        {
            let host_failure = reason == DynamicCordisRunFailureReason::HostHalfFailed;
            attempt.status = CordisRunStatus::Failed;
            attempt.error = Some(crate::CordisRunDiagnostic {
                phase: if host_failure {
                    CordisDiagnosticPhase::HostApply
                } else {
                    CordisDiagnosticPhase::ClientApply
                },
                message: message.clone(),
                stack: stack.map(str::to_owned),
                plugin_id: plugin_id.clone(),
                package_id: attempt.package_id.clone(),
                plugin_run_id: attempt.plugin_run_id.clone(),
            });
            let failed_half = CordisHalfState {
                status: CordisHalfStatus::Failed,
                waiting_for: Vec::new(),
                error: Some(message.clone()),
            };
            if host_failure {
                attempt.host = failed_half;
            } else {
                attempt.client = failed_half;
            }
        }
        DynamicCordisRunResponse::Failure {
            reason,
            message,
            stack: stack.map(str::to_owned),
        }
    }

    /// Runs a Host-only package and commits it as current.
    pub async fn run_host_only(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
        mode: DynamicCordisRunMode,
    ) -> DynamicCordisRunResponse {
        match self
            .run_host_half_with_id(
                session,
                plugin_id,
                package_id,
                mode,
                HostStartOptions {
                    attach_policy: AttachPolicy::Never,
                    forced_run_id: None,
                    started_for_request: None,
                },
            )
            .await
        {
            DynamicCordisHostHalfResult::Success {
                plugin_run_id,
                waiting_for,
                ..
            } => {
                let plugin = self.registry.get(plugin_id);
                let (current, next) = plugin.map_or((None, None), |plugin| {
                    let plugin = plugin.lock();
                    (
                        plugin.current_package_id.clone(),
                        plugin.next_package_id.clone(),
                    )
                });
                DynamicCordisRunResponse::Success {
                    status: DynamicCordisRunSuccessStatus::Running,
                    plugin_id: plugin_id.clone(),
                    package_id: package_id.clone(),
                    plugin_run_id,
                    waiting_for,
                    client_waiting_for: None,
                    current_package_id: current,
                    next_package_id: next,
                    mode,
                }
            }
            DynamicCordisHostHalfResult::Failure(error) => DynamicCordisRunResponse::Failure {
                reason: DynamicCordisRunFailureReason::HostHalfFailed,
                message: error.message,
                stack: error.stack,
            },
        }
    }

    /// Starts a Host-only package or publishes a Client-bearing activation request.
    pub async fn run(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
        mode: DynamicCordisRunMode,
    ) -> DynamicCordisRunResponse {
        self.run_with_signal(session, plugin_id, package_id, mode, None)
            .await
    }

    /// Starts or requests one package, observing cancellation only before publication.
    pub async fn run_with_signal(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
        mode: DynamicCordisRunMode,
        signal: Option<&AbortSignal>,
    ) -> DynamicCordisRunResponse {
        let plugin = match self.owned(session, plugin_id) {
            Ok(plugin) => plugin,
            Err(error) => {
                return DynamicCordisRunResponse::Failure {
                    reason: DynamicCordisRunFailureReason::PluginMissing,
                    message: error.to_string(),
                    stack: None,
                };
            }
        };
        let definition = {
            let plugin = plugin.lock();
            let Some(definition) = plugin.packages.get(package_id).cloned() else {
                return DynamicCordisRunResponse::Failure {
                    reason: DynamicCordisRunFailureReason::PackageMissing,
                    message: format!("plugin \"{plugin_id}\" has no package \"{package_id}\""),
                    stack: None,
                };
            };
            if let Some(message) = invalid_mode_message(
                plugin_id,
                package_id,
                mode,
                plugin.current_package_id.as_ref(),
            ) {
                return DynamicCordisRunResponse::Failure {
                    reason: DynamicCordisRunFailureReason::InvalidMode,
                    message,
                    stack: None,
                };
            }
            definition
        };
        if self.starting.lock().contains_key(plugin_id) {
            return DynamicCordisRunResponse::Failure {
                reason: DynamicCordisRunFailureReason::TransitionInFlight,
                message: format!("plugin \"{plugin_id}\" is already starting"),
                stack: None,
            };
        }
        if signal.is_some_and(AbortSignal::is_aborted) {
            return DynamicCordisRunResponse::Failure {
                reason: DynamicCordisRunFailureReason::Cancelled,
                message: format!(
                    "the run request for dynamic plugin \"{plugin_id}\" was cancelled before activation"
                ),
                stack: None,
            };
        }
        if definition.client_code.is_none() {
            return self
                .run_host_only(session, plugin_id, package_id, mode)
                .await;
        }
        self.arm_client_request(session, plugin_id, package_id, mode, &plugin, definition)
    }

    fn arm_client_request(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
        mode: DynamicCordisRunMode,
        plugin: &Arc<parking_lot::Mutex<DynamicCordisPluginState>>,
        definition: DynamicCordisDefinition,
    ) -> DynamicCordisRunResponse {
        if self.registry.pending_request_for(plugin_id).is_some() {
            return DynamicCordisRunResponse::Failure {
                reason: DynamicCordisRunFailureReason::TransitionInFlight,
                message: format!(
                    "dynamic plugin \"{plugin_id}\" already has a pending run request"
                ),
                stack: None,
            };
        }
        let Some(context) = &self.context else {
            return DynamicCordisRunResponse::Failure {
                reason: DynamicCordisRunFailureReason::ClientHalfFailed,
                message: "dynamic Cordis runner has no Host context".to_owned(),
                stack: None,
            };
        };
        let plugin_run_id = self.registry.mint_plugin_run_id();
        let request_id = self.registry.mint_approval_request_id();
        let requires_approval = {
            let plugin = plugin.lock();
            !plugin.client_version_updates_approved
                && !plugin.approved_client_packages.contains(package_id)
        };
        let mut attempt = pending_attempt(
            plugin_run_id.clone(),
            package_id.clone(),
            mode,
            definition.host_code.is_some(),
            true,
        );
        attempt.approval_request_id = Some(request_id.clone());
        attempt.requires_approval = Some(requires_approval);
        attempt.status = if requires_approval {
            CordisRunStatus::AwaitingApproval
        } else {
            CordisRunStatus::StartingHost
        };
        {
            let mut plugin = plugin.lock();
            plugin.next_package_id = Some(package_id.clone());
            plugin.latest_run = Some(attempt);
        }
        self.registry.arm_request(
            request_id.clone(),
            crate::DynamicCordisPendingRequest {
                agent_id: session.clone(),
                plugin_id: plugin_id.clone(),
                package_id: package_id.clone(),
                plugin_run_id: plugin_run_id.clone(),
                mode,
                requires_approval,
            },
        );
        let _ = context.events().emit(
            context,
            "cordis/request-run",
            &EventArgs::one(DynamicCordisRunRequest {
                request_id,
                agent_id: session.clone(),
                plugin_id: plugin_id.clone(),
                package_id: package_id.clone(),
                mode,
                name: definition.name,
                purpose: definition.purpose,
                requires_approval,
            }),
        );
        DynamicCordisRunResponse::Success {
            status: if requires_approval {
                DynamicCordisRunSuccessStatus::AwaitingApproval
            } else {
                DynamicCordisRunSuccessStatus::Starting
            },
            plugin_id: plugin_id.clone(),
            package_id: package_id.clone(),
            plugin_run_id,
            waiting_for: Vec::new(),
            client_waiting_for: None,
            current_package_id: plugin.lock().current_package_id.clone(),
            next_package_id: Some(package_id.clone()),
            mode,
        }
    }

    /// Settles a pending request before Host activation, using first-answer-wins claiming.
    pub async fn resolve_request_run(
        &self,
        request_id: &crate::ApprovalRequestId,
        resolution: &DynamicCordisRunResolution,
    ) -> DynamicCordisResolveAck {
        let Some(pending) = self.registry.peek_request(request_id) else {
            return DynamicCordisResolveAck { accepted: false };
        };
        let addressed_run = match resolution {
            DynamicCordisRunResolution::Success { plugin_run_id, .. } => Some(plugin_run_id),
            DynamicCordisRunResolution::Failure { plugin_run_id, .. } => plugin_run_id.as_ref(),
        };
        if let Some(addressed) = addressed_run
            && self
                .registry
                .get(&pending.plugin_id)
                .and_then(|plugin| {
                    plugin
                        .lock()
                        .run
                        .as_ref()
                        .map(|run| run.plugin_run_id.clone())
                })
                .as_ref()
                != Some(addressed)
        {
            return DynamicCordisResolveAck { accepted: false };
        }
        let Some(_) = self.registry.claim_request(request_id) else {
            return DynamicCordisResolveAck { accepted: false };
        };
        let settled = self
            .settle_activation(
                &pending.agent_id,
                &pending.plugin_id,
                resolution,
                Some(request_id),
            )
            .await;
        if let Some(context) = &self.context {
            let _ = context.events().emit(
                context,
                "cordis/request-run-resolved",
                &EventArgs::one(DynamicCordisRequestResolved {
                    request_id: request_id.clone(),
                    outcome: if pending.requires_approval {
                        match resolution {
                            DynamicCordisRunResolution::Success { .. } => {
                                RequestRunOutcome::Approved
                            }
                            DynamicCordisRunResolution::Failure {
                                reason: DynamicCordisRunFailureReason::Rejected,
                                ..
                            } => RequestRunOutcome::Rejected,
                            DynamicCordisRunResolution::Failure { .. } => RequestRunOutcome::Failed,
                        }
                    } else {
                        RequestRunOutcome::Completed
                    },
                }),
            );
        }
        self.steer_run_outcome(&pending, &settled);
        DynamicCordisResolveAck { accepted: true }
    }

    /// Invokes one Host method only for the exact active generation.
    pub async fn invoke(
        &self,
        plugin_id: &CordisDynamicPluginId,
        plugin_run_id: &CordisDynamicPluginRunId,
        method: &str,
        args: serde_json::Value,
    ) -> DynamicCordisInvokeResult {
        let Some(plugin) = self.registry.get(plugin_id) else {
            return invoke_failure(
                DynamicCordisInvokeErrorCode::PluginNotRunning,
                format!("dynamic plugin \"{plugin_id}\" is not running"),
            );
        };
        let (runtime, session) = {
            let plugin = plugin.lock();
            let Some(run) = &plugin.run else {
                return invoke_failure(
                    DynamicCordisInvokeErrorCode::PluginNotRunning,
                    format!("dynamic plugin \"{plugin_id}\" is not running"),
                );
            };
            if run.plugin_run_id != *plugin_run_id {
                return invoke_failure(
                    DynamicCordisInvokeErrorCode::StaleRun,
                    format!("activation \"{plugin_run_id}\" is no longer active"),
                );
            }
            let Some(runtime) = &run.host_runtime else {
                return invoke_failure(
                    DynamicCordisInvokeErrorCode::MethodNotFound,
                    format!(
                        "dynamic plugin \"{plugin_id}\" registered no Host method \"{method}\""
                    ),
                );
            };
            if !runtime
                .handler_names()
                .iter()
                .any(|registered| registered == method)
            {
                return invoke_failure(
                    DynamicCordisInvokeErrorCode::MethodNotFound,
                    format!(
                        "dynamic plugin \"{plugin_id}\" registered no Host method \"{method}\""
                    ),
                );
            }
            (runtime.clone(), plugin.session_id.clone())
        };
        match runtime.invoke(method, args).await {
            Ok(value) => DynamicCordisInvokeResult::Success { value },
            Err(error) => {
                let failure = CordisErrorDetails {
                    message: error.to_string(),
                    stack: None,
                };
                let key = format!("Host\0handler\0{method}\0{}", failure.message);
                if self.claim_runtime_failure(plugin_id, plugin_run_id, &key) {
                    self.steer_handler_failure(
                        &session,
                        plugin_id,
                        plugin_run_id,
                        method,
                        &failure,
                    );
                }
                DynamicCordisInvokeResult::Failure {
                    code: DynamicCordisInvokeErrorCode::HandlerError,
                    error: failure,
                }
            }
        }
    }

    /// Stops one active Host run while retaining package definitions.
    pub async fn stop(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
    ) -> DynamicCordisStopResponse {
        let plugin = match self.owned(session, plugin_id) {
            Ok(plugin) => plugin,
            Err(error) => {
                return DynamicCordisStopResponse::Failure {
                    reason: DynamicCordisStopFailureReason::PluginMissing,
                    message: error.to_string(),
                };
            }
        };
        let pending = self.registry.pending_request_for(plugin_id);
        let has_run = plugin.lock().run.is_some();
        if !has_run && pending.is_none() {
            return DynamicCordisStopResponse::Failure {
                reason: DynamicCordisStopFailureReason::NotRunning,
                message: format!("dynamic plugin \"{plugin_id}\" is not running"),
            };
        }
        if pending.is_some() {
            self.cancel_pending(
                plugin_id,
                format!("dynamic plugin \"{plugin_id}\" was stopped before approval"),
            );
        }
        self.retract(plugin_id, &plugin).await;
        let mut plugin = plugin.lock();
        plugin.active_run = None;
        plugin.next_package_id = None;
        if let Some(attempt) = &mut plugin.latest_run {
            attempt.status = CordisRunStatus::Stopped;
            if attempt.host.status != CordisHalfStatus::Absent {
                attempt.host = stopped_half();
            }
            if attempt.client.status != CordisHalfStatus::Absent {
                attempt.client = stopped_half();
            }
        }
        DynamicCordisStopResponse::Success
    }

    /// Stops a Plugin for a manual panel action and injects the outcome without waking the model.
    pub async fn stop_from_panel(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
    ) -> DynamicCordisStopResponse {
        let result = self.stop(session, plugin_id).await;
        if result == DynamicCordisStopResponse::Success {
            let current = self
                .owned(session, plugin_id)
                .ok()
                .and_then(|plugin| plugin.lock().current_package_id.clone())
                .map_or_else(|| "none".to_owned(), |package| package.to_string());
            self.inject_user_context(
                session,
                format!(
                    "The user stopped Cordis Plugin {plugin_id}. Its Packages remain defined; currentPackageId is {current}."
                ),
            );
        }
        result
    }

    fn cancel_pending(&self, plugin_id: &CordisDynamicPluginId, message: String) {
        let Some(request_id) = self.registry.pending_request_for(plugin_id) else {
            return;
        };
        let Some(pending) = self.registry.claim_request(&request_id) else {
            return;
        };
        if let Some(plugin) = self.registry.get(plugin_id) {
            let mut plugin = plugin.lock();
            if let Some(attempt) = &mut plugin.latest_run
                && attempt.plugin_run_id == pending.plugin_run_id
            {
                attempt.status = CordisRunStatus::Cancelled;
                attempt.error = Some(crate::CordisRunDiagnostic {
                    phase: CordisDiagnosticPhase::Approval,
                    message,
                    stack: None,
                    plugin_id: plugin_id.clone(),
                    package_id: attempt.package_id.clone(),
                    plugin_run_id: attempt.plugin_run_id.clone(),
                });
                attempt.approval_request_id = None;
                attempt.requires_approval = None;
            }
        }
        if let Some(context) = &self.context {
            let _ = context.events().emit(
                context,
                "cordis/request-run-resolved",
                &EventArgs::one(DynamicCordisRequestResolved {
                    request_id,
                    outcome: RequestRunOutcome::Cancelled,
                }),
            );
        }
    }

    async fn retract(
        &self,
        plugin_id: &CordisDynamicPluginId,
        plugin: &Arc<parking_lot::Mutex<DynamicCordisPluginState>>,
    ) {
        let run = {
            let mut plugin = plugin.lock();
            plugin.active_run = None;
            plugin.run.take()
        };
        let Some(run) = run else {
            return;
        };
        let retracted = DynamicCordisRetracted {
            plugin_id: plugin_id.clone(),
            package_id: run.package_id,
            plugin_run_id: run.plugin_run_id,
        };
        if let Some(fiber) = run.fiber {
            let _ = fiber.dispose().await;
        }
        if let Some(context) = &self.context {
            let _ = context.events().emit(
                context,
                "cordis/dynamic-retract",
                &EventArgs::one(retracted),
            );
        }
    }

    /// Removes an owned plugin, stopping its active run and forgetting every version.
    pub async fn undefine(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
    ) -> crate::DynamicCordisUndefineReceipt {
        let plugin = match self.owned(session, plugin_id) {
            Ok(plugin) => plugin,
            Err(error) => {
                return crate::DynamicCordisUndefineReceipt::PluginMissing {
                    message: error.to_string(),
                };
            }
        };
        self.cancel_pending(
            plugin_id,
            format!("dynamic plugin \"{plugin_id}\" was removed before approval"),
        );
        let was_running = plugin.lock().run.is_some();
        if was_running {
            self.retract(plugin_id, &plugin).await;
        }
        self.registry.delete(plugin_id);
        crate::DynamicCordisUndefineReceipt::Success { was_running }
    }

    /// Removes a Plugin for a manual panel action and injects the outcome without waking the model.
    pub async fn undefine_from_panel(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
    ) -> crate::DynamicCordisUndefineReceipt {
        let result = self.undefine(session, plugin_id).await;
        if matches!(result, crate::DynamicCordisUndefineReceipt::Success { .. }) {
            self.inject_user_context(
                session,
                format!(
                    "The user removed Cordis Plugin {plugin_id} and all of its Packages. The Plugin no longer exists."
                ),
            );
        }
        result
    }

    fn claim_runtime_failure(
        &self,
        plugin_id: &CordisDynamicPluginId,
        plugin_run_id: &CordisDynamicPluginRunId,
        key: &str,
    ) -> bool {
        let Some(plugin) = self.registry.get(plugin_id) else {
            return false;
        };
        let mut plugin = plugin.lock();
        if plugin.latest_run.as_ref().is_none_or(|attempt| {
            attempt.plugin_run_id != *plugin_run_id
                || !matches!(
                    attempt.status,
                    CordisRunStatus::Running | CordisRunStatus::Waiting
                )
        }) {
            return false;
        }
        let Some(run) = &mut plugin.run else {
            return false;
        };
        if run.plugin_run_id != *plugin_run_id {
            return false;
        }
        run.reported_runtime_errors.insert(key.to_owned())
    }

    fn steer_run_outcome(
        &self,
        pending: &crate::DynamicCordisPendingRequest,
        settled: &DynamicCordisRunResponse,
    ) {
        let Some(agent) = self.agent(&pending.agent_id) else {
            return;
        };
        let plugin = self.registry.get(&pending.plugin_id);
        let (current, next) = plugin.map_or((None, None), |plugin| {
            let plugin = plugin.lock();
            (
                plugin.current_package_id.clone(),
                plugin.next_package_id.clone(),
            )
        });
        let identity = format!(
            "{}/{} ({})",
            pending.plugin_id, pending.package_id, pending.plugin_run_id
        );
        let text = match settled {
            DynamicCordisRunResponse::Success {
                current_package_id, ..
            } => format!(
                "Cordis {} {identity} completed successfully. currentPackageId is {}. Continue using the running Plugin.",
                run_mode_string(pending.mode),
                current_package_id.as_ref().unwrap_or(&pending.package_id)
            ),
            DynamicCordisRunResponse::Failure {
                reason: DynamicCordisRunFailureReason::Rejected,
                ..
            } => format!(
                "The user rejected Cordis {} {identity}. Do not request the same activation again unless the user asks.",
                run_mode_string(pending.mode)
            ),
            failure @ DynamicCordisRunResponse::Failure { reason, .. } => format!(
                "Cordis {} {identity} failed after cordis_run returned {}: {}\n{}\ncurrentPackageId: {}\nnextPackageId: {}\nInspect the failed Package, correct it on the same Plugin when needed, and retry the activation autonomously.",
                run_mode_string(pending.mode),
                if pending.requires_approval {
                    "awaiting-approval"
                } else {
                    "starting"
                },
                reason_string(*reason),
                format_run_failure(failure),
                current
                    .as_ref()
                    .map_or("none".to_owned(), ToString::to_string),
                next.as_ref()
                    .map_or_else(|| pending.package_id.to_string(), ToString::to_string),
            ),
        };
        let _ = agent.steer(cordis_message(text));
    }

    fn steer_render_failure(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        package_id: &CordisDynamicPackageId,
        plugin_run_id: &CordisDynamicPluginRunId,
        failure: &DynamicCordisRenderFailure,
    ) {
        let Some(agent) = self.agent(session) else {
            return;
        };
        let details = format_error_details(&CordisErrorDetails {
            message: failure.message.clone(),
            stack: failure.stack.clone(),
        });
        let _ = agent.steer(cordis_message(format!(
            "Cordis Client UI {plugin_id}/{package_id} ({plugin_run_id}) failed while rendering Slot {:?} after activation.\n{details}\nentryAbdicated: {}\nInspect the failed Package, fix the Client code by defining a new Package on the same Plugin, and activate that Package autonomously with cordis_run mode:\"update\".",
            failure.slot, failure.abdicated
        )));
    }

    fn steer_handler_failure(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        plugin_run_id: &CordisDynamicPluginRunId,
        method: &str,
        failure: &CordisErrorDetails,
    ) {
        let Some(agent) = self.agent(session) else {
            return;
        };
        let package_id = self
            .registry
            .get(plugin_id)
            .and_then(|plugin| plugin.lock().run.as_ref().map(|run| run.package_id.clone()))
            .map_or_else(|| "unknown".to_owned(), |package| package.to_string());
        let _ = agent.steer(cordis_message(format!(
            "Cordis Host handler {plugin_id}/{package_id} ({plugin_run_id}) failed when the Client called host.call({method:?}).\n{}\nThe Plugin remains running. Inspect this Package, correct the Host code on the same Plugin, and activate the new Package autonomously with cordis_run mode:\"update\". If the handler needs a Service, either declare that Service in the returned Plugin inject list or read it with ctx.get(name) and handle undefined.",
            format_error_details(failure)
        )));
    }

    fn steer_guard_failure(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        plugin_run_id: &CordisDynamicPluginRunId,
        platform: &str,
        failure: &CordisErrorDetails,
    ) {
        let Some(agent) = self.agent(session) else {
            return;
        };
        let package_id = self
            .registry
            .get(plugin_id)
            .and_then(|plugin| plugin.lock().run.as_ref().map(|run| run.package_id.clone()))
            .map_or_else(|| "unknown".to_owned(), |package| package.to_string());
        let _ = agent.steer(cordis_message(format!(
            "Cordis {platform} guard rejected runtime code in {plugin_id}/{package_id} ({plugin_run_id}) after activation.\n{}\nThe Plugin remains running. Inspect this Package, define a corrected Package on the same Plugin, and activate it autonomously with cordis_run mode:\"update\".",
            format_error_details(failure)
        )));
    }

    fn inject_user_run_outcome(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        settled: &DynamicCordisRunResponse,
    ) {
        let plugin = self.owned(session, plugin_id).ok();
        let text = match settled {
            DynamicCordisRunResponse::Success {
                package_id,
                plugin_run_id,
                current_package_id,
                ..
            } => format!(
                "The user manually ran Cordis Plugin {plugin_id}, Package {package_id}, as {plugin_run_id}. The activation succeeded; currentPackageId is {}.",
                current_package_id
                    .as_ref()
                    .map_or("none".to_owned(), ToString::to_string)
            ),
            failure @ DynamicCordisRunResponse::Failure { reason, .. } => {
                let (attempt, current, next) = plugin.map_or((None, None, None), |plugin| {
                    let plugin = plugin.lock();
                    (
                        plugin.latest_run.clone(),
                        plugin.current_package_id.clone(),
                        plugin.next_package_id.clone(),
                    )
                });
                let identity = attempt.as_ref().map_or_else(String::new, |attempt| {
                    format!(
                        ", Package {}, as {}",
                        attempt.package_id, attempt.plugin_run_id
                    )
                });
                format!(
                    "The user manually ran Cordis Plugin {plugin_id}{identity}, but it failed: {}\n{}\ncurrentPackageId: {}\nnextPackageId: {}",
                    reason_string(*reason),
                    format_run_failure(failure),
                    current
                        .as_ref()
                        .map_or("none".to_owned(), ToString::to_string),
                    next.as_ref().map_or("none".to_owned(), ToString::to_string),
                )
            }
        };
        self.inject_user_context(session, text);
    }

    fn inject_user_context(&self, session: &SessionId, text: String) {
        if let Some(agent) = self.agent(session) {
            let _ = agent.inject(cordis_message(text));
        }
    }

    fn agent(&self, session: &SessionId) -> Option<Arc<seekdeep_agent::Agent>> {
        self.context.as_ref()?.get(AGENTS)?.get(session)
    }

    fn owned(
        &self,
        session: &SessionId,
        plugin_id: &CordisDynamicPluginId,
    ) -> anyhow::Result<Arc<parking_lot::Mutex<DynamicCordisPluginState>>> {
        let plugin = self
            .registry
            .get(plugin_id)
            .ok_or_else(|| anyhow::anyhow!(missing_plugin_message(plugin_id)))?;
        anyhow::ensure!(
            plugin.lock().session_id == *session,
            missing_plugin_message(plugin_id)
        );
        Ok(plugin)
    }
}

fn record_started_host(
    plugin: &Arc<parking_lot::Mutex<DynamicCordisPluginState>>,
    definition: &DynamicCordisDefinition,
    commit: StartedHostCommit,
) {
    let StartedHostCommit {
        started,
        plugin_run_id,
        package_id,
        waiting_for,
        started_for_request,
    } = commit;
    let running_status = if waiting_for.is_empty() {
        CordisRunStatus::Running
    } else {
        CordisRunStatus::Waiting
    };
    let mut plugin = plugin.lock();
    plugin.run = Some(DynamicCordisPhysicalRun {
        plugin_run_id: plugin_run_id.clone(),
        package_id: package_id.clone(),
        fiber: started.fiber,
        host_runtime: started.runtime,
        render_failure: None,
        reported_runtime_errors: std::collections::BTreeSet::new(),
        started_for_request,
    });
    plugin.active_run = Some(crate::DynamicCordisActiveRun {
        plugin_run_id,
        package_id: package_id.clone(),
    });
    if definition.client_code.is_none() {
        plugin.current_package_id = Some(package_id);
        plugin.next_package_id = None;
    }
    if let Some(attempt) = &mut plugin.latest_run {
        attempt.status = if definition.client_code.is_some() {
            CordisRunStatus::ClientPending
        } else {
            running_status
        };
        attempt.host = if definition.host_code.is_none() {
            CordisHalfState {
                status: CordisHalfStatus::Absent,
                waiting_for: Vec::new(),
                error: None,
            }
        } else {
            CordisHalfState {
                status: if waiting_for.is_empty() {
                    CordisHalfStatus::Running
                } else {
                    CordisHalfStatus::Waiting
                },
                waiting_for,
                error: None,
            }
        };
    }
}

fn package_summaries(plugin: &DynamicCordisPluginState) -> Vec<DynamicCordisInventoryPackage> {
    plugin
        .packages
        .values()
        .map(|definition| DynamicCordisInventoryPackage {
            package_id: definition.package_id.clone(),
            name: definition.name.clone(),
            purpose: definition.purpose.clone(),
            has_host_half: definition.host_code.is_some(),
            has_client_half: definition.client_code.is_some(),
        })
        .collect()
}

fn missing_plugin_message(id: &CordisDynamicPluginId) -> String {
    format!(
        "no dynamic plugin \"{id}\" in this process — it may have been removed or lost on SeekDeep restart"
    )
}

fn announce_package(
    context: &Context,
    plugin_id: &CordisDynamicPluginId,
    package_id: &CordisDynamicPackageId,
    plugin_run_id: &CordisDynamicPluginRunId,
    name: String,
) {
    let _ = context.events().emit(
        context,
        "cordis/dynamic-package",
        &EventArgs::one(DynamicCordisPackage {
            plugin_id: plugin_id.clone(),
            package_id: package_id.clone(),
            plugin_run_id: plugin_run_id.clone(),
            name,
        }),
    );
}

fn invalid_mode_message(
    plugin_id: &CordisDynamicPluginId,
    package_id: &CordisDynamicPackageId,
    mode: DynamicCordisRunMode,
    current: Option<&CordisDynamicPackageId>,
) -> Option<String> {
    match (mode, current) {
        (DynamicCordisRunMode::Update, None) => Some(format!(
            "plugin \"{plugin_id}\" has no successful version yet; start \"{package_id}\" with mode \"run\""
        )),
        (DynamicCordisRunMode::Update, Some(current)) if current == package_id => Some(format!(
            "package \"{package_id}\" is already current; use mode \"run\""
        )),
        (DynamicCordisRunMode::Run, Some(current)) if current != package_id => Some(format!(
            "package \"{package_id}\" differs from current \"{current}\"; use mode \"update\""
        )),
        (DynamicCordisRunMode::Run | DynamicCordisRunMode::Update, None | Some(_)) => None,
    }
}

fn pending_attempt(
    plugin_run_id: crate::CordisDynamicPluginRunId,
    package_id: CordisDynamicPackageId,
    mode: DynamicCordisRunMode,
    has_host: bool,
    has_client: bool,
) -> DynamicCordisRunAttempt {
    DynamicCordisRunAttempt {
        plugin_run_id,
        package_id,
        mode,
        status: CordisRunStatus::StartingHost,
        approval_request_id: None,
        requires_approval: None,
        host: CordisHalfState {
            status: if has_host {
                CordisHalfStatus::Pending
            } else {
                CordisHalfStatus::Absent
            },
            waiting_for: Vec::new(),
            error: None,
        },
        client: CordisHalfState {
            status: if has_client {
                CordisHalfStatus::Pending
            } else {
                CordisHalfStatus::Absent
            },
            waiting_for: Vec::new(),
            error: None,
        },
        error: None,
    }
}

fn stopped_half() -> CordisHalfState {
    CordisHalfState {
        status: CordisHalfStatus::Stopped,
        waiting_for: Vec::new(),
        error: None,
    }
}

fn missing_services(context: &Context, fiber: &Arc<seekdeep_cordis::PluginFiber>) -> Vec<String> {
    fiber
        .inject()
        .iter()
        .filter(|service| !context.has_named(service))
        .cloned()
        .collect()
}

fn record_host_failure(plugin: &Arc<parking_lot::Mutex<DynamicCordisPluginState>>, message: &str) {
    let mut plugin = plugin.lock();
    plugin.run = None;
    plugin.active_run = None;
    if let Some(attempt) = &mut plugin.latest_run {
        attempt.status = CordisRunStatus::Failed;
        attempt.host = CordisHalfState {
            status: CordisHalfStatus::Failed,
            waiting_for: Vec::new(),
            error: Some(message.to_owned()),
        };
    }
}

fn host_failure(error: &anyhow::Error) -> DynamicCordisHostHalfResult {
    DynamicCordisHostHalfResult::Failure(CordisErrorDetails {
        message: error.to_string(),
        stack: None,
    })
}

fn invoke_failure(
    code: DynamicCordisInvokeErrorCode,
    message: String,
) -> DynamicCordisInvokeResult {
    DynamicCordisInvokeResult::Failure {
        code,
        error: CordisErrorDetails {
            message,
            stack: None,
        },
    }
}

fn stale_run_failure(
    plugin_id: &CordisDynamicPluginId,
    run_id: &CordisDynamicPluginRunId,
) -> DynamicCordisRunResponse {
    DynamicCordisRunResponse::Failure {
        reason: DynamicCordisRunFailureReason::ClientHalfFailed,
        message: format!("dynamic plugin \"{plugin_id}\" is not running activation \"{run_id}\""),
        stack: None,
    }
}

fn reason_string(reason: DynamicCordisRunFailureReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "dynamic Cordis activation failed".to_owned())
}

fn run_mode_string(mode: DynamicCordisRunMode) -> &'static str {
    match mode {
        DynamicCordisRunMode::Run => "run",
        DynamicCordisRunMode::Update => "update",
    }
}

fn format_error_details(failure: &CordisErrorDetails) -> String {
    failure.stack.as_ref().map_or_else(
        || format!("message: {}", failure.message),
        |stack| format!("message: {}\nstack:\n{stack}", failure.message),
    )
}

fn format_run_failure(failure: &DynamicCordisRunResponse) -> String {
    match failure {
        DynamicCordisRunResponse::Failure { message, stack, .. } => {
            format_error_details(&CordisErrorDetails {
                message: message.clone(),
                stack: stack.clone(),
            })
        }
        DynamicCordisRunResponse::Success { .. } => "message: activation succeeded".to_owned(),
    }
}

fn cordis_message(text: String) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text { text }],
        MessageSource::plugin("cordis-host-runner"),
    )
}
