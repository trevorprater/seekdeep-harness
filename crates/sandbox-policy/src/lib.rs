//! Deployment defaults and durable per-session sandbox-policy resolution.

use std::{
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use path_clean::PathClean as _;
use seekdeep_cordis::{Context, Fiber, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::Session;
use seekdeep_sandbox::{SandboxExecutionPolicy, SandboxMode, canonical_path};
use seekdeep_system_prompt::{PromptContext, PromptText, SYSTEM_PROMPT};
use serde::{Deserialize, Serialize};

pub mod invariant;

/// Typed Cordis seat corresponding to `ctx.sandboxPolicy`.
pub const SANDBOX_POLICY: ServiceKey<SandboxPolicyService> = ServiceKey::new("sandboxPolicy");
/// Cordis plugin name.
pub const NAME: &str = "sandbox-policy";
/// The policy service has no mandatory services; prompt context is opportunistic.
pub const INJECT: &[&str] = &[];

/// Every mode for option advertisement and untrusted-input validation.
pub use seekdeep_sandbox::SANDBOX_MODES;

/// Deployment sandbox fallback configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxPolicyConfig {
    /// Mode a session starts from.
    pub mode: SandboxMode,
    /// Fallback root for agentless calls and sessions without a cwd.
    pub workspace_root: Option<PathBuf>,
}

impl Default for SandboxPolicyConfig {
    fn default() -> Self {
        Self {
            mode: SandboxMode::ReadOnly,
            workspace_root: None,
        }
    }
}

/// Inputs selecting one complete capability-call policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct SandboxPolicyRequest<'a> {
    /// Calling session; its immutable cwd becomes the workspace boundary.
    pub session: Option<&'a Session>,
    /// Explicit approved mode override, outranking durable session state.
    pub mode: Option<SandboxMode>,
}

/// Sandbox-policy service and deployment fallback owner.
#[derive(Debug)]
pub struct SandboxPolicyService {
    /// Deployment fallback beneath a session override.
    pub default_mode: SandboxMode,
    /// Absolute canonical-or-lexically-resolved fallback root.
    pub workspace_root: PathBuf,
}

impl SandboxPolicyService {
    /// Resolves deployment fallbacks at construction time.
    ///
    /// # Errors
    ///
    /// Returns when the process cwd cannot be read for a missing root.
    pub fn new(config: SandboxPolicyConfig) -> anyhow::Result<Arc<Self>> {
        let root = match config.workspace_root {
            Some(root) => root,
            None => std::env::current_dir()?,
        };
        Ok(Arc::new(Self {
            default_mode: config.mode,
            workspace_root: resolve_workspace_root(&root)?,
        }))
    }

    /// Publishes this exact service in a Cordis context.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(SANDBOX_POLICY, self.clone())?)
    }

    /// Resolves explicit mode, durable session override, and deployment fallback in precedence order.
    ///
    /// # Errors
    ///
    /// Returns only if an unexpected relative session fallback cannot be absolutized.
    pub fn resolve(
        &self,
        request: SandboxPolicyRequest<'_>,
    ) -> anyhow::Result<SandboxExecutionPolicy> {
        let session_mode = request
            .session
            .map(|session| validated_effective_sandbox_mode(&session.events()))
            .transpose()?
            .flatten();
        let root = request
            .session
            .and_then(|session| session.header().cwd.as_deref())
            .map_or(self.workspace_root.as_path(), Path::new);
        Ok(SandboxExecutionPolicy {
            mode: request.mode.or(session_mode).unwrap_or(self.default_mode),
            workspace_root: resolve_workspace_root(root)?,
            session_id: request.session.map(|session| session.id().clone()),
        })
    }

    /// Last durable session override without the deployment fallback.
    #[must_use]
    pub fn override_of(&self, session: &Session) -> Option<SandboxMode> {
        effective_sandbox_mode(&session.events())
    }

    fn contribute_prompt_context(
        self: &Arc<Self>,
        owner: &Context,
        lookup: &Context,
    ) -> anyhow::Result<Option<EffectHandle>> {
        let Some(prompt) = lookup.get(SYSTEM_PROMPT) else {
            return Ok(None);
        };
        let weak = Arc::downgrade(self);
        Ok(Some(prompt.prompt_context(
            owner,
            PromptContext::new(
                "sandbox:policy",
                110.0,
                PromptText::Dynamic(Arc::new(move |context| {
                    let Some(session) = context.agent_session.as_deref() else {
                        return Ok(String::new());
                    };
                    let service = weak.upgrade().ok_or_else(|| {
                        anyhow::anyhow!("sandbox policy disposed before prompt assembly")
                    })?;
                    render_policy_context(&service.resolve(SandboxPolicyRequest {
                        session: Some(session),
                        mode: None,
                    })?)
                })),
            ),
        )?))
    }
}

fn reconcile_prompt_context(
    service: &Arc<SandboxPolicyService>,
    owner: &Context,
    lookup: &Context,
    mounted_prompt: &Mutex<Option<usize>>,
) -> anyhow::Result<()> {
    let Some(prompt) = lookup.get(SYSTEM_PROMPT) else {
        *mounted_prompt.lock() = None;
        return Ok(());
    };
    let identity = Arc::as_ptr(&prompt) as usize;
    if *mounted_prompt.lock() == Some(identity) {
        return Ok(());
    }
    service.contribute_prompt_context(owner, lookup)?;
    *mounted_prompt.lock() = Some(identity);
    Ok(())
}

/// Installed service plus its reversible prompt contribution boundary.
pub struct SandboxPolicyInstallation {
    service: Arc<SandboxPolicyService>,
    effect: EffectHandle,
}

impl SandboxPolicyInstallation {
    /// Exact installed service.
    #[must_use]
    pub fn service(&self) -> Arc<SandboxPolicyService> {
        self.service.clone()
    }

    /// Disposes service and prompt contribution together.
    ///
    /// # Errors
    ///
    /// Returns aggregate cleanup failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.effect.dispose().await
    }
}

impl Deref for SandboxPolicyInstallation {
    type Target = SandboxPolicyService;

    fn deref(&self) -> &Self::Target {
        &self.service
    }
}

/// Installs policy service and opportunistic prompt contribution atomically.
///
/// # Errors
///
/// Returns configuration, duplicate-service, prompt, ownership, or cleanup failures.
pub fn install(
    context: &Context,
    config: SandboxPolicyConfig,
) -> anyhow::Result<SandboxPolicyInstallation> {
    let fiber = Fiber::active_child("sandbox-policy");
    let child = context.with_fiber(fiber.clone());
    let service = SandboxPolicyService::new(config)?;
    let install_result = (|| {
        service.provide(&child)?;
        let mounted_prompt = Arc::new(Mutex::new(None));
        reconcile_prompt_context(&service, &child, context, &mounted_prompt)?;
        let watched_service = Arc::downgrade(&service);
        let watched_owner = child.clone();
        let watched_lookup = context.clone();
        child.on_service_change({
            let mounted_prompt = mounted_prompt.clone();
            move || {
                let Some(service) = watched_service.upgrade() else {
                    return;
                };
                // The notification seam has no error return. Failed late
                // registration remains unmarked and is retried on the next
                // service change.
                let _ = reconcile_prompt_context(
                    &service,
                    &watched_owner,
                    &watched_lookup,
                    &mounted_prompt,
                );
            }
        })?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(error) = install_result {
        return match futures::executor::block_on(fiber.dispose()) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow::anyhow!("{error:#}: cleanup failed: {cleanup:#}")),
        };
    }
    let cleanup_fiber = fiber.clone();
    let effect = EffectHandle::new("sandbox-policy", move || {
        Box::pin(async move { cleanup_fiber.dispose().await })
    });
    if let Err(error) = context.own(effect.clone()) {
        return match futures::executor::block_on(fiber.dispose()) {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!("{error}: cleanup failed: {cleanup:#}")),
        };
    }
    Ok(SandboxPolicyInstallation { service, effect })
}

fn validate_plugin_config(value: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let config: SandboxPolicyConfig = serde_json::from_value(value.clone())?;
    // Construction resolves cwd and path semantics at the same load boundary.
    SandboxPolicyService::new(config)?;
    Ok(value.clone())
}

/// Builds the Loader-compatible policy plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: SandboxPolicyConfig = serde_json::from_value(config)?;
            install(&context, config)?;
            Ok(())
        })
    })
    .with_config_validator(validate_plugin_config)
}

/// Resolves filesystem identity before lexical normalization can erase symlink components.
fn resolve_workspace_root(path: &Path) -> anyhow::Result<PathBuf> {
    let canonical = canonical_path(path);
    let absolute = if canonical.is_absolute() {
        canonical
    } else {
        std::env::current_dir()?.join(canonical)
    };
    Ok(absolute.clean())
}

/// Renders current policy without claiming which capabilities are mounted.
///
/// # Errors
///
/// Returns when an OS path cannot be represented in the source string model.
pub fn render_policy_context(policy: &SandboxExecutionPolicy) -> anyhow::Result<String> {
    let root = policy
        .workspace_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("sandbox workspace root is not valid Unicode"))?;
    Ok(match policy.mode {
        SandboxMode::ReadOnly => "Current SeekDeep file policy: read-only. Any available operation enforced by the SeekDeep file sandbox cannot modify files in the standing mode. Do not refuse a required modification from this policy alone: try an available tool normally and follow any denial and escalation guidance it returns.".to_owned(),
        SandboxMode::WorkspaceWrite => format!(
            "Current SeekDeep file policy: workspace-write. Any available operation enforced by the SeekDeep file sandbox may modify files under the session workspace: {}. Some platform temporary areas may also be writable.",
            serde_json::to_string(root)?
        ),
        SandboxMode::DangerFullAccess => "Current SeekDeep file policy: danger-full-access. The SeekDeep file sandbox does not restrict file modifications by available operations.".to_owned(),
    })
}

/// Folds to the last valid durable `sandbox/mode` event.
#[must_use]
pub fn effective_sandbox_mode(
    events: &[seekdeep_core::session::SessionEvent],
) -> Option<SandboxMode> {
    events.iter().rev().find_map(|event| {
        (event.event_type == "sandbox/mode")
            .then(|| event.data.get("mode").and_then(serde_json::Value::as_str))
            .flatten()
            .and_then(SandboxMode::parse)
    })
}

fn validated_effective_sandbox_mode(
    events: &[seekdeep_core::session::SessionEvent],
) -> anyhow::Result<Option<SandboxMode>> {
    let Some(event) = events
        .iter()
        .rev()
        .find(|event| event.event_type == "sandbox/mode")
    else {
        return Ok(None);
    };
    let Some(mode) = event
        .data
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .and_then(SandboxMode::parse)
    else {
        let rendered = event.data.get("mode").map_or_else(
            || "undefined".to_owned(),
            |value| serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_owned()),
        );
        anyhow::bail!("sandbox/mode carries unknown mode {rendered}");
    };
    Ok(Some(mode))
}

/// Appends exactly one durable, log-only session mode switch.
///
/// # Errors
///
/// Returns session append validation or persistence failures.
pub fn set_sandbox_mode(
    session: &Session,
    mode: SandboxMode,
) -> anyhow::Result<seekdeep_core::session::SessionEvent> {
    Ok(session.append(
        "sandbox/mode",
        serde_json::json!({"mode": mode.as_str()}),
        seekdeep_core::session::AppendOptions::default(),
    )?)
}

/// Validates an untrusted mode spelling before appending.
///
/// # Errors
///
/// Returns a closed-vocabulary diagnostic or session append failure.
pub fn set_sandbox_mode_str(
    session: &Session,
    mode: &str,
) -> anyhow::Result<seekdeep_core::session::SessionEvent> {
    let mode = SandboxMode::parse(mode).ok_or_else(|| {
        anyhow::anyhow!(
            "sandbox mode must be one of \"read-only\", \"workspace-write\", or \"danger-full-access\""
        )
    })?;
    set_sandbox_mode(session, mode)
}
