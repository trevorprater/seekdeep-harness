//! Configurable registry for package-owned runtime invariant companions.

/// Catalog and lifecycle adapter for packages with intentionally empty
/// invariant companions.
pub mod noop;

use std::{collections::HashSet, future::Future, sync::Arc};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use regress::Regex;
use seekdeep_cordis::{
    Context, CordisError, Fiber, Plugin, PluginFiber, ServiceKey,
    fiber::{DisposeFuture, EffectHandle},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Typed service slot for the invariant registry.
pub const INVARIANTS: ServiceKey<InvariantRegistry> = ServiceKey::new("invariants");

/// Runtime invariant selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InvariantConfig {
    /// Global switch, enabled by default.
    pub enabled: bool,
    /// Case-sensitive ECMAScript regex sources admitting package names.
    pub package_allowlist: Vec<String>,
    /// Case-sensitive ECMAScript regex sources excluding package names after
    /// allowlist matching.
    pub package_blocklist: Vec<String>,
}

impl Default for InvariantConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            package_allowlist: Vec::new(),
            package_blocklist: Vec::new(),
        }
    }
}

/// Package-attributed invariant failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("invariant violated by \"{package_name}\": {message}")]
pub struct InvariantError {
    /// Stable machine-readable failure code.
    pub code: &'static str,
    /// Full package name owning the violated contract.
    pub package_name: String,
    /// Contract failure without the standard prefix.
    pub message: String,
}

impl InvariantError {
    /// Constructs a package-attributed invariant error.
    ///
    /// This is public so compatibility integrations can preserve the source
    /// runtime's structural `code === "INVARIANT"` failure channel when an
    /// invariant is detected outside an [`InvariantFailure`] callback.
    #[must_use]
    pub fn new(package_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: "INVARIANT",
            package_name: package_name.into(),
            message: message.into(),
        }
    }
}

/// Reporter bound to the package that owns one installer.
#[derive(Clone, Debug)]
pub struct InvariantFailure {
    package_name: Arc<str>,
}

impl InvariantFailure {
    /// Creates a package-attributed error for immediate return from a check.
    #[must_use]
    pub fn fail(&self, message: impl Into<String>) -> InvariantError {
        InvariantError::new(self.package_name.as_ref(), message)
    }
}

type InstallerCallback =
    Arc<dyn Fn(Context, InvariantFailure) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// One package's invariant installer plus required child services.
#[derive(Clone)]
pub struct InvariantInstaller {
    inject: Vec<String>,
    callback: InstallerCallback,
}

impl std::fmt::Debug for InvariantInstaller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InvariantInstaller")
            .field("inject", &self.inject)
            .finish_non_exhaustive()
    }
}

impl InvariantInstaller {
    /// Defines an asynchronous installer and its required services.
    #[must_use]
    pub fn new<I, S, F, Fut>(inject: I, callback: F) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        F: Fn(Context, InvariantFailure) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        Self {
            inject: inject.into_iter().map(Into::into).collect(),
            callback: Arc::new(move |context, failure| Box::pin(callback(context, failure))),
        }
    }

    /// Defines an explained empty runtime invariant.
    #[must_use]
    pub fn noop() -> Self {
        Self::new(std::iter::empty::<String>(), |_, _| async { Ok(()) })
    }
}

#[derive(Default)]
struct RegistryState {
    registrations: HashSet<String>,
}

#[derive(Clone, Debug)]
enum StartupOutcome {
    Ready,
    Failed(String),
    Disposed,
}

#[derive(Debug, Default)]
struct StartupState {
    outcome: Mutex<Option<StartupOutcome>>,
    notify: tokio::sync::Notify,
}

impl StartupState {
    fn complete(&self, outcome: StartupOutcome) {
        let mut stored = self.outcome.lock();
        if stored.is_none() {
            *stored = Some(outcome);
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) -> StartupOutcome {
        loop {
            let notified = self.notify.notified();
            if let Some(outcome) = self.outcome.lock().clone() {
                return outcome;
            }
            notified.await;
        }
    }
}

/// Package-owned invariant registry with global and regex-based selection.
pub struct InvariantRegistry {
    owner_context: Context,
    enabled: bool,
    package_allowlist: Vec<Regex>,
    package_blocklist: Vec<Regex>,
    state: Arc<Mutex<RegistryState>>,
}

impl std::fmt::Debug for InvariantRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InvariantRegistry")
            .field("enabled", &self.enabled)
            .field("registrations", &self.state.lock().registrations)
            .finish_non_exhaustive()
    }
}

impl InvariantRegistry {
    /// Creates and provides the registry in one context.
    ///
    /// # Errors
    ///
    /// Rejects malformed ECMAScript patterns, duplicate patterns, or an
    /// occupied/inactive service slot.
    pub fn install(context: &Context, config: &InvariantConfig) -> anyhow::Result<Arc<Self>> {
        let registry = Arc::new(Self::new(context, config)?);
        context.provide(INVARIANTS, registry.clone())?;
        Ok(registry)
    }

    /// Constructs a registry without publishing it as a service.
    ///
    /// # Errors
    ///
    /// Rejects malformed ECMAScript patterns and duplicates.
    pub fn new(context: &Context, config: &InvariantConfig) -> anyhow::Result<Self> {
        Ok(Self {
            owner_context: context.clone(),
            enabled: config.enabled,
            package_allowlist: compile_patterns("package_allowlist", &config.package_allowlist)?,
            package_blocklist: compile_patterns("package_blocklist", &config.package_blocklist)?,
            state: Arc::new(Mutex::new(RegistryState::default())),
        })
    }

    /// Registers one package's installer and synchronously reserves its name.
    ///
    /// Filtering disables execution but not reservation. Selected installers
    /// run inside dependency-aware child plugin fibers.
    ///
    /// # Errors
    ///
    /// Rejects malformed or duplicate package names and inactive ownership.
    pub fn register(
        self: &Arc<Self>,
        package_name: &str,
        installer: InvariantInstaller,
    ) -> anyhow::Result<InvariantRegistration> {
        validate_package_name(package_name)?;
        {
            let mut state = self.state.lock();
            if !state.registrations.insert(package_name.to_owned()) {
                anyhow::bail!("invariants: package \"{package_name}\" is already registered");
            }
        }

        let selected = self.selected(package_name);
        let startup = Arc::new(StartupState::default());
        let plugin = if selected {
            match self.mount_installer(package_name, installer, startup.clone()) {
                Ok(plugin) => Some(plugin),
                Err(error) => {
                    self.state.lock().registrations.remove(package_name);
                    return Err(error.into());
                }
            }
        } else {
            startup.complete(StartupOutcome::Ready);
            None
        };

        let weak_registry = Arc::downgrade(self);
        let cleanup_name = package_name.to_owned();
        let cleanup_plugin = plugin.clone();
        let cleanup_startup = startup.clone();
        let effect = EffectHandle::new(
            format!("invariants.register({package_name:?})"),
            move || -> DisposeFuture {
                Box::pin(async move {
                    let result = if let Some(plugin) = cleanup_plugin {
                        plugin.dispose().await
                    } else {
                        Ok(())
                    };
                    if let Some(registry) = weak_registry.upgrade() {
                        registry.state.lock().registrations.remove(&cleanup_name);
                    }
                    cleanup_startup.complete(StartupOutcome::Disposed);
                    result
                })
            },
        );
        if let Err(error) = self.owner_context.own(effect.clone()) {
            self.state.lock().registrations.remove(package_name);
            return Err(error.into());
        }

        monitor_startup(startup.clone(), effect.clone());
        Ok(InvariantRegistration { effect, startup })
    }

    fn selected(&self, package_name: &str) -> bool {
        self.enabled
            && (self.package_allowlist.is_empty()
                || self
                    .package_allowlist
                    .iter()
                    .any(|pattern| pattern.find(package_name).is_some()))
            && !self
                .package_blocklist
                .iter()
                .any(|pattern| pattern.find(package_name).is_some())
    }

    fn mount_installer(
        &self,
        package_name: &str,
        installer: InvariantInstaller,
        startup: Arc<StartupState>,
    ) -> Result<Arc<PluginFiber>, CordisError> {
        let failure = InvariantFailure {
            package_name: Arc::from(package_name),
        };
        let callback = installer.callback;
        let plugin = Plugin::new(
            format!("{package_name}-invariant"),
            installer.inject,
            move |context, _| {
                let callback = callback.clone();
                let failure = failure.clone();
                let startup = startup.clone();
                Box::pin(async move {
                    let fiber = Fiber::active_child("invariant-installer");
                    let child = context.with_fiber(fiber.clone());
                    match callback(child, failure).await {
                        Ok(()) => {
                            let cleanup_fiber = fiber.clone();
                            let effect = EffectHandle::new(
                                "invariant installer child",
                                move || -> DisposeFuture {
                                    Box::pin(async move { cleanup_fiber.dispose().await })
                                },
                            );
                            if let Err(error) = context.own(effect) {
                                let cleanup = fiber.dispose().await;
                                let message = cleanup.map_or_else(
                                    |cleanup| format!("{error}: cleanup failed: {cleanup:#}"),
                                    |()| error.to_string(),
                                );
                                startup.complete(StartupOutcome::Failed(message));
                            } else {
                                startup.complete(StartupOutcome::Ready);
                            }
                        }
                        Err(error) => {
                            let cleanup = fiber.dispose().await;
                            let message = cleanup.map_or_else(
                                |cleanup| format!("{error:#}: cleanup failed: {cleanup:#}"),
                                |()| format!("{error:#}"),
                            );
                            startup.complete(StartupOutcome::Failed(message));
                        }
                    }
                    // The dependency wrapper stays active after publishing a
                    // fully rolled-back failure, preventing lifecycle retries.
                    Ok(())
                })
            },
        );
        self.owner_context.plugin(plugin, Value::Null)
    }

    /// Whether a package name is currently reserved.
    #[must_use]
    pub fn is_registered(&self, package_name: &str) -> bool {
        self.state.lock().registrations.contains(package_name)
    }
}

/// Effect-scoped invariant registration.
#[derive(Clone, Debug)]
pub struct InvariantRegistration {
    effect: EffectHandle,
    startup: Arc<StartupState>,
}

impl InvariantRegistration {
    /// Joins child startup, including a dependency-pending phase.
    ///
    /// # Errors
    ///
    /// Returns installer/publication failure or disposal before activation.
    pub async fn await_ready(&self) -> anyhow::Result<()> {
        match self.startup.wait().await {
            StartupOutcome::Ready => Ok(()),
            StartupOutcome::Failed(rendered) => {
                let _ = self.effect.dispose().await;
                Err(anyhow::anyhow!(rendered))
            }
            StartupOutcome::Disposed => anyhow::bail!("invariant registration was disposed"),
        }
    }

    /// Disposes the child completely, then releases package ownership.
    /// Concurrent callers join the same disposal.
    ///
    /// # Errors
    ///
    /// Returns aggregated child cleanup failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.effect.dispose().await
    }
}

fn monitor_startup(startup: Arc<StartupState>, effect: EffectHandle) {
    let monitor = async move {
        if matches!(startup.wait().await, StartupOutcome::Failed(_)) {
            let _ = effect.dispose().await;
        }
    };
    spawn_monitor(monitor);
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_monitor(future: impl Future<Output = ()> + Send + 'static) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(future);
    } else {
        std::thread::spawn(move || futures::executor::block_on(future));
    }
}

#[cfg(target_arch = "wasm32")]
fn spawn_monitor(future: impl Future<Output = ()> + Send + 'static) {
    wasm_bindgen_futures::spawn_local(future);
}

fn compile_patterns(field: &str, values: &[String]) -> anyhow::Result<Vec<Regex>> {
    let mut seen = HashSet::new();
    values
        .iter()
        .map(|value| {
            if value.is_empty() || has_surrounding_js_whitespace(value) {
                anyhow::bail!(
                    "invariants: {field} entries must be non-blank and have no surrounding whitespace"
                );
            }
            if !seen.insert(value.as_str()) {
                anyhow::bail!(
                    "invariants: {field} contains duplicate regex {}",
                    serde_json::to_string(value)?
                );
            }
            Regex::new(value).map_err(|error| {
                anyhow::anyhow!(
                    "invariants: {field} contains invalid regex {}: {error}",
                    serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
                )
            })
        })
        .collect()
}

fn validate_package_name(package_name: &str) -> anyhow::Result<()> {
    if package_name.is_empty()
        || has_surrounding_js_whitespace(package_name)
        || package_name.chars().any(is_js_whitespace)
    {
        anyhow::bail!("invariants: packageName must be non-blank and contain no whitespace");
    }
    Ok(())
}

fn has_surrounding_js_whitespace(value: &str) -> bool {
    value.chars().next().is_some_and(is_js_whitespace)
        || value.chars().next_back().is_some_and(is_js_whitespace)
}

fn is_js_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{feff}'
}

/// Registers this registry package's explained empty companion.
///
/// # Errors
///
/// Returns ordinary registration failures.
pub fn register_self_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-invariants", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use seekdeep_cordis::{EventOptions, EventReply};

    use super::*;

    fn config(allow: &[&str], block: &[&str]) -> InvariantConfig {
        InvariantConfig {
            package_allowlist: allow.iter().map(|value| (*value).to_owned()).collect(),
            package_blocklist: block.iter().map(|value| (*value).to_owned()).collect(),
            ..InvariantConfig::default()
        }
    }

    fn probe_installer(calls: Arc<AtomicUsize>) -> InvariantInstaller {
        InvariantInstaller::new(std::iter::empty::<String>(), move |context, _| {
            let calls = calls.clone();
            async move {
                context.events().on_sync(
                    &context,
                    "invariants-test/ping",
                    move |_, _| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(EventReply::Undefined)
                    },
                    EventOptions {
                        global: true,
                        ..EventOptions::default()
                    },
                )?;
                Ok(())
            }
        })
    }

    fn emit(context: &Context) -> anyhow::Result<()> {
        context.events().emit(
            context,
            "invariants-test/ping",
            &seekdeep_cordis::EventArgs::new(),
        )?;
        Ok(())
    }

    #[tokio::test]
    async fn selection_defaults_filters_and_reservation_match_source() {
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let registration = registry
            .register("seekdeep-session", probe_installer(calls.clone()))
            .unwrap();
        registration.await_ready().await.unwrap();
        emit(&context).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let filtered_context = Context::new();
        let filtered =
            InvariantRegistry::install(&filtered_context, &config(&["session"], &["extra"]))
                .unwrap();
        let allowed = Arc::new(AtomicUsize::new(0));
        let blocked = Arc::new(AtomicUsize::new(0));
        filtered
            .register("seekdeep-session", probe_installer(allowed.clone()))
            .unwrap()
            .await_ready()
            .await
            .unwrap();
        filtered
            .register("seekdeep-session-extra", probe_installer(blocked.clone()))
            .unwrap()
            .await_ready()
            .await
            .unwrap();
        emit(&filtered_context).unwrap();
        assert_eq!(allowed.load(Ordering::SeqCst), 1);
        assert_eq!(blocked.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn disabled_registry_still_reserves_ownership() {
        let context = Context::new();
        let registry = InvariantRegistry::install(
            &context,
            &InvariantConfig {
                enabled: false,
                ..InvariantConfig::default()
            },
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let registration = registry
            .register("seekdeep-session", probe_installer(calls.clone()))
            .unwrap();
        registration.await_ready().await.unwrap();
        assert!(
            registry
                .register("seekdeep-session", InvariantInstaller::noop())
                .is_err()
        );
        emit(&context).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        registration.dispose().await.unwrap();
        assert!(!registry.is_registered("seekdeep-session"));
    }

    #[test]
    fn validates_ecmascript_patterns_duplicates_and_names() {
        let context = Context::new();
        for bad in ["", " ", " session", "session "] {
            assert!(InvariantRegistry::new(&context, &config(&[bad], &[])).is_err());
        }
        assert!(InvariantRegistry::new(&context, &config(&["session", "session"], &[])).is_err());
        assert!(InvariantRegistry::new(&context, &config(&["["], &[])).is_err());
        // Backreferences and lookaround are valid JavaScript patterns, unlike
        // the workspace's linear-time Rust regex dialect.
        assert!(InvariantRegistry::new(&context, &config(&[r"(s)\1(?=ion)"], &[])).is_ok());
        let registry =
            Arc::new(InvariantRegistry::new(&context, &InvariantConfig::default()).unwrap());
        for bad in [
            "",
            " ",
            " package",
            "pack age",
            "package\n",
            "\u{feff}package",
        ] {
            assert!(registry.register(bad, InvariantInstaller::noop()).is_err());
        }
    }

    #[tokio::test]
    async fn failure_is_attributed_and_rolls_back_listener_and_name() {
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let leaked = Arc::new(AtomicBool::new(false));
        let installer = InvariantInstaller::new(std::iter::empty::<String>(), {
            let leaked = leaked.clone();
            move |child, failure| {
                let leaked = leaked.clone();
                async move {
                    child.events().on_sync(
                        &child,
                        "invariants-test/ping",
                        move |_, _| {
                            leaked.store(true, Ordering::Release);
                            Ok(EventReply::Undefined)
                        },
                        EventOptions {
                            global: true,
                            ..EventOptions::default()
                        },
                    )?;
                    Err(failure.fail("seq must strictly increase").into())
                }
            }
        });
        let failed = registry.register("seekdeep-session", installer).unwrap();
        let error = failed.await_ready().await.unwrap_err().to_string();
        assert_eq!(
            error,
            "invariant violated by \"seekdeep-session\": seq must strictly increase"
        );
        emit(&context).unwrap();
        assert!(!leaked.load(Ordering::Acquire));
        assert!(!registry.is_registered("seekdeep-session"));
        let retry = registry
            .register("seekdeep-session", InvariantInstaller::noop())
            .unwrap();
        retry.await_ready().await.unwrap();
    }

    #[tokio::test]
    async fn disposal_is_joined_and_name_releases_after_child_cleanup() {
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let release = Arc::new(tokio::sync::Notify::new());
        let installer = InvariantInstaller::new(std::iter::empty::<String>(), {
            let release = release.clone();
            move |child, _| {
                let release = release.clone();
                async move {
                    child.own(EffectHandle::new("barrier", move || {
                        Box::pin(async move {
                            release.notified().await;
                            Ok(())
                        })
                    }))?;
                    Ok(())
                }
            }
        });
        let registration = registry.register("seekdeep-session", installer).unwrap();
        registration.await_ready().await.unwrap();
        let disposing = tokio::spawn({
            let registration = registration.clone();
            async move { registration.dispose().await }
        });
        tokio::task::yield_now().await;
        assert!(
            registry
                .register("seekdeep-session", InvariantInstaller::noop())
                .is_err()
        );
        release.notify_waiters();
        disposing.await.unwrap().unwrap();
        let replacement = registry
            .register("seekdeep-session", InvariantInstaller::noop())
            .unwrap();
        replacement.await_ready().await.unwrap();
    }

    #[tokio::test]
    async fn inactive_owner_rolls_back_synchronous_reservation() {
        let root = Context::new();
        let fiber = seekdeep_cordis::Fiber::active_child("owner");
        let child = root.with_fiber(fiber.clone());
        let registry =
            Arc::new(InvariantRegistry::new(&child, &InvariantConfig::default()).unwrap());
        fiber.dispose().await.unwrap();
        assert!(
            registry
                .register("seekdeep-session", InvariantInstaller::noop())
                .is_err()
        );
        assert!(!registry.is_registered("seekdeep-session"));
    }
}
