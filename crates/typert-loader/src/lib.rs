//! Lifecycle loader for compiled native Typert package artifacts.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_loader::{LOADER, LoaderEntrySnapshot};
use seekdeep_typert_registry::{TYPERT, TypertContribution, TypertFace};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Package artifact export name in the source runtime.
pub const TYPERT_HOST_EXPORT: &str = "./typert";
/// Cordis plugin name.
pub const NAME: &str = "typert-loader";
/// Required native services.
pub const INJECT: &[&str] = &["typert", "loader", "typertArtifacts"];

/// Additional compiled packages whose plugins are nested behind another Loader entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TypertLoaderConfig {
    /// Exact package names that must expose a compiled Host artifact.
    pub packages: Vec<String>,
}

/// Asynchronous factory for one compiled Host contribution.
pub type TypertArtifactFactory =
    Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<TypertContribution>> + Send + Sync + 'static>;

/// One installed native package and its optional Host Typert artifact.
#[derive(Clone)]
pub struct TypertPackageArtifact {
    /// Exact package name.
    pub package: String,
    /// Compiled Host contribution factory, absent when the package has no `./typert` export.
    pub host: Option<TypertArtifactFactory>,
}

impl std::fmt::Debug for TypertPackageArtifact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertPackageArtifact")
            .field("package", &self.package)
            .field("host", &self.host.as_ref().map(|_| "<factory>"))
            .finish()
    }
}

struct StoredArtifact {
    generation: Uuid,
    artifact: TypertPackageArtifact,
}

/// Native directory replacing Node package-export discovery.
#[derive(Default)]
pub struct TypertArtifactRegistry {
    artifacts: Arc<Mutex<HashMap<String, StoredArtifact>>>,
}

impl std::fmt::Debug for TypertArtifactRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypertArtifactRegistry")
            .field("len", &self.artifacts.lock().len())
            .finish_non_exhaustive()
    }
}

/// Cordis slot for native compiled Typert artifacts.
pub const TYPERT_ARTIFACTS: ServiceKey<TypertArtifactRegistry> = ServiceKey::new("typertArtifacts");

impl TypertArtifactRegistry {
    /// Installs an empty native artifact directory.
    ///
    /// # Errors
    ///
    /// Returns duplicate-Service or inactive-owner failures.
    pub fn install(context: &Context) -> Result<Arc<Self>, seekdeep_cordis::CordisError> {
        let registry = Arc::new(Self::default());
        context.provide(TYPERT_ARTIFACTS, registry.clone())?;
        Ok(registry)
    }

    /// Registers one exact package artifact for the owner's lifetime.
    ///
    /// # Errors
    ///
    /// Rejects empty names, duplicate packages, or inactive ownership.
    pub fn register(
        &self,
        owner: &Context,
        artifact: TypertPackageArtifact,
    ) -> anyhow::Result<EffectHandle> {
        anyhow::ensure!(
            !artifact.package.is_empty(),
            "typert-loader: artifact package name must not be empty"
        );
        let generation = Uuid::now_v7();
        let package = artifact.package.clone();
        {
            let mut artifacts = self.artifacts.lock();
            anyhow::ensure!(
                !artifacts.contains_key(&package),
                "typert-loader: package artifact {package:?} is already registered"
            );
            artifacts.insert(
                package.clone(),
                StoredArtifact {
                    generation,
                    artifact,
                },
            );
        }
        let artifacts = self.artifacts.clone();
        let cleanup_package = package.clone();
        let effect = EffectHandle::synchronous(
            format!("typertArtifacts.register({package:?})"),
            move || {
                let mut artifacts = artifacts.lock();
                if artifacts
                    .get(&cleanup_package)
                    .is_some_and(|stored| stored.generation == generation)
                {
                    artifacts.remove(&cleanup_package);
                }
                Ok(())
            },
        );
        if let Err(error) = owner.own(effect.clone()) {
            self.artifacts.lock().remove(&package);
            return Err(error.into());
        }
        Ok(effect)
    }

    fn get(&self, package: &str) -> Option<TypertPackageArtifact> {
        self.artifacts
            .lock()
            .get(package)
            .map(|stored| stored.artifact.clone())
    }
}

#[derive(Clone)]
enum CachedArtifact {
    Missing,
    WithoutHost,
    Loaded(Arc<TypertContribution>),
    Failed(Arc<str>),
}

#[derive(Default)]
struct LoaderState {
    cache: HashMap<String, CachedArtifact>,
    registered: HashMap<String, EffectHandle>,
}

struct TypertLoader {
    context: Context,
    config: TypertLoaderConfig,
    artifacts: Arc<TypertArtifactRegistry>,
    state: tokio::sync::Mutex<LoaderState>,
    active: AtomicBool,
    dirty: AtomicBool,
    scheduled: AtomicBool,
}

impl TypertLoader {
    async fn install(context: &Context, config: TypertLoaderConfig) -> anyhow::Result<Arc<Self>> {
        let artifacts = context
            .get(TYPERT_ARTIFACTS)
            .ok_or_else(|| anyhow::anyhow!("typert-loader requires typertArtifacts"))?;
        let loader = Arc::new(Self {
            context: context.clone(),
            config,
            artifacts,
            state: tokio::sync::Mutex::new(LoaderState::default()),
            active: AtomicBool::new(true),
            dirty: AtomicBool::new(false),
            scheduled: AtomicBool::new(false),
        });
        loader.register_lifetime(context)?;
        loader.register_plugin_listener(context)?;
        loader.reconcile(true).await?;
        Ok(loader)
    }

    fn register_lifetime(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let loader = Arc::downgrade(self);
        context.own(EffectHandle::synchronous(
            "typert loader lifetime",
            move || {
                if let Some(loader) = loader.upgrade() {
                    loader.active.store(false, Ordering::Release);
                    loader.dirty.store(false, Ordering::Release);
                }
                Ok(())
            },
        ))?;
        Ok(())
    }

    fn register_plugin_listener(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let loader = self.clone();
        context.events().on(
            context,
            "internal/plugin",
            move |_, args| {
                let loader = loader.clone();
                let entry_name = args
                    .get::<seekdeep_cordis::PluginFiber>(0)
                    .and_then(|fiber| fiber.entry_name());
                Box::pin(async move {
                    if entry_name.is_some() {
                        loader.mark_dirty();
                    }
                    Ok(EventReply::Undefined)
                })
            },
            EventOptions::default(),
        )?;
        Ok(())
    }

    fn mark_dirty(self: &Arc<Self>) {
        self.dirty.store(true, Ordering::Release);
        if self.scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let loader = self.clone();
        tokio::spawn(async move {
            loop {
                loader.dirty.store(false, Ordering::Release);
                if loader.active.load(Ordering::Acquire)
                    && let Err(error) = loader.reconcile(false).await
                {
                    tracing::error!(%error, "typert-loader steady-state reconciliation failed");
                }
                loader.scheduled.store(false, Ordering::Release);
                if !loader.dirty.load(Ordering::Acquire)
                    || loader.scheduled.swap(true, Ordering::AcqRel)
                {
                    break;
                }
            }
        });
    }

    fn live_packages(&self) -> anyhow::Result<BTreeSet<String>> {
        let loader = self
            .context
            .get(LOADER)
            .ok_or_else(|| anyhow::anyhow!("typert-loader requires loader"))?;
        Ok(loader
            .entries()?
            .into_iter()
            .filter(entry_qualifies)
            .map(|entry| entry.plugin.as_str().to_owned())
            .chain(self.config.packages.iter().cloned())
            .collect())
    }

    async fn reconcile(&self, activation: bool) -> anyhow::Result<()> {
        if !activation {
            self.context
                .get(LOADER)
                .ok_or_else(|| anyhow::anyhow!("typert-loader requires loader"))?
                .wait()
                .await?;
        }
        let live = self.live_packages()?;
        let configured = self
            .config
            .packages
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let registry = self
            .context
            .get(TYPERT)
            .ok_or_else(|| anyhow::anyhow!("typert-loader requires typert"))?;
        let mut state = self.state.lock().await;
        let withdrawn_packages = state
            .registered
            .keys()
            .filter(|package| !live.contains(*package))
            .cloned()
            .collect::<Vec<_>>();
        for package in withdrawn_packages {
            if let Some(effect) = state.registered.remove(&package) {
                effect.dispose().await?;
            }
        }
        let mut failures = Vec::new();
        for package in live {
            if state.registered.contains_key(&package) {
                continue;
            }
            let explicit = configured.contains(package.as_str());
            let contribution = match self.load_contribution(&package, explicit, &mut state).await {
                Ok(Some(contribution)) => contribution,
                Ok(None) => continue,
                Err(error) => {
                    failures.push(error);
                    continue;
                }
            };
            if !self.active.load(Ordering::Acquire) || !self.live_packages()?.contains(&package) {
                continue;
            }
            match registry.register(&self.context, &contribution) {
                Ok(effect) => {
                    state.registered.insert(package, effect);
                }
                Err(error) => failures.push(anyhow::anyhow!(
                    "typert-loader: {package} failed registry admission: {error:#}"
                )),
            }
        }
        if failures.is_empty() {
            return Ok(());
        }
        if activation {
            anyhow::bail!(
                "typert-loader: {} typert contributor(s) failed to register:\n{}",
                failures.len(),
                failures
                    .iter()
                    .map(|error| format!("  - {error:#}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        for error in failures {
            tracing::error!(%error, "typert-loader package registration failed");
        }
        Ok(())
    }

    async fn load_contribution(
        &self,
        package: &str,
        explicit: bool,
        state: &mut LoaderState,
    ) -> anyhow::Result<Option<Arc<TypertContribution>>> {
        if let Some(cached) = state.cache.get(package) {
            return cached_result(package, explicit, cached);
        }
        let Some(artifact) = self.artifacts.get(package) else {
            state
                .cache
                .insert(package.to_owned(), CachedArtifact::Missing);
            return cached_result(package, explicit, &CachedArtifact::Missing);
        };
        let Some(factory) = artifact.host else {
            state
                .cache
                .insert(package.to_owned(), CachedArtifact::WithoutHost);
            return cached_result(package, explicit, &CachedArtifact::WithoutHost);
        };
        let verdict = match factory().await {
            Ok(contribution) => match validate_typert_manifest(package, &contribution) {
                Ok(()) => CachedArtifact::Loaded(Arc::new(contribution)),
                Err(error) => CachedArtifact::Failed(Arc::from(format!("{error:#}"))),
            },
            Err(error) => CachedArtifact::Failed(Arc::from(format!(
                "typert-loader: {package} exports {TYPERT_HOST_EXPORT:?} but loading its compiled artifact failed: {error:#}"
            ))),
        };
        state.cache.insert(package.to_owned(), verdict.clone());
        cached_result(package, explicit, &verdict)
    }
}

fn entry_qualifies(entry: &LoaderEntrySnapshot) -> bool {
    !entry.disabled && entry.state.is_some()
}

fn cached_result(
    package: &str,
    explicit: bool,
    cached: &CachedArtifact,
) -> anyhow::Result<Option<Arc<TypertContribution>>> {
    match cached {
        CachedArtifact::Missing if explicit => anyhow::bail!(
            "typert-loader: configured package {package:?} cannot be resolved from the compiled artifact registry — add its native artifact or remove it from packages"
        ),
        CachedArtifact::WithoutHost if explicit => anyhow::bail!(
            "typert-loader: configured package {package:?} does not export {TYPERT_HOST_EXPORT:?}"
        ),
        CachedArtifact::Failed(message) => Err(anyhow::anyhow!(message.to_string())),
        CachedArtifact::Loaded(contribution) => Ok(Some(contribution.clone())),
        CachedArtifact::Missing | CachedArtifact::WithoutHost => Ok(None),
    }
}

/// Validates one compiled Host contribution at its package boundary.
///
/// # Errors
///
/// Returns exact ownership, face, schema, model, or invocation diagnostics.
pub fn validate_typert_manifest(
    package: &str,
    contribution: &TypertContribution,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        contribution.package == package,
        "typert-loader: {package} TYPERT manifest names package {:?} — the manifest must be owned by the package that exports it",
        contribution.package
    );
    anyhow::ensure!(
        contribution.face == TypertFace::Host,
        "typert-loader: {package} exports {TYPERT_HOST_EXPORT:?} but TYPERT.face is not \"host\""
    );
    for schema in &contribution.schemas {
        require_nonempty(package, &schema.name, "schema", "name")?;
    }
    for service in &contribution.model.services {
        require_nonempty(package, &service.key, "service", "key")?;
        require_nonempty(package, &service.export_name, "service", "exportName")?;
        validate_members(
            package,
            &service.members,
            &format!("service {:?}", service.key),
        )?;
        validate_types(
            package,
            &service.types,
            &format!("service {:?}", service.key),
        )?;
    }
    for event in &contribution.model.events {
        require_nonempty(package, &event.name, "event", "name")?;
        require_nonempty(
            package,
            &event.signature,
            &format!("event {:?}", event.name),
            "signature",
        )?;
    }
    for object in &contribution.model.objects {
        require_nonempty(package, &object.name, "object", "name")?;
        require_nonempty(package, &object.export_name, "object", "exportName")?;
        validate_members(
            package,
            &object.members,
            &format!("object {:?}", object.name),
        )?;
        validate_types(package, &object.types, &format!("object {:?}", object.name))?;
    }
    seekdeep_typert_registry::validate_contribution(contribution)
        .map_err(|error| anyhow::anyhow!("typert-loader: {package} {error:#}"))
}

fn require_nonempty(package: &str, value: &str, subject: &str, key: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty(),
        "typert-loader: {package} {subject} has a missing or empty {key}"
    );
    Ok(())
}

fn validate_members(
    package: &str,
    members: &[seekdeep_typert_registry::TypertMemberModel],
    subject: &str,
) -> anyhow::Result<()> {
    for member in members {
        require_nonempty(package, &member.name, &format!("{subject} member"), "name")?;
        require_nonempty(
            package,
            &member.signature,
            &format!("{subject} member"),
            "signature",
        )?;
    }
    Ok(())
}

fn validate_types(
    package: &str,
    types: &[seekdeep_typert_registry::TypertTypeModel],
    subject: &str,
) -> anyhow::Result<()> {
    for type_ in types {
        require_nonempty(package, &type_.name, &format!("{subject} type"), "name")?;
        require_nonempty(
            package,
            &type_.declaration,
            &format!("{subject} type"),
            "declaration",
        )?;
    }
    Ok(())
}

/// Builds the native Typert Loader plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: TypertLoaderConfig = serde_json::from_value(config)?;
            anyhow::ensure!(
                config.packages.iter().all(|package| !package.is_empty()),
                "typert-loader packages must contain non-empty strings"
            );
            TypertLoader::install(&context, config).await?;
            Ok(())
        })
    })
}
