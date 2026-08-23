//! Live roster, standing Loader generations, Agent joins, and private-service addressing.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Weak},
    time::SystemTime,
};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentLifecycleEvent};
use seekdeep_cordis::{
    Context, EventArgs, EventOptions, EventReply, Service, ServiceKey, fiber::EffectHandle,
};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_loader::{LOADER, LoadedComposition, PluginCatalog};
use seekdeep_schemastery::Schema;
use seekdeep_scope::{
    Scope, ScopeKey, ScopeParentBinding, bind_scope_parent, create_scope, scope_of, scope_parent_of,
};
use seekdeep_settings::{
    SETTINGS, SettingsPathOp, SettingsScope, SettingsService, settings_namespace,
};
use serde_json::{Value, json};

use crate::{
    authoring::{copy_composition, delete_composition, read_composition},
    discovery::{USER_PRESET_DIR, discover_presets},
    preset::{AgentPreset, AgentPresetConfig, PresetMountError, PresetRoot, UnknownPresetError},
};

/// Typed Cordis seat corresponding to the Agent-preset roster.
pub const AGENT_PRESETS: ServiceKey<AgentPresetRegistry> = ServiceKey::new("agentPresets");
/// Settings namespace carrying the user's chosen default preset.
pub const SETTINGS_NAMESPACE: &str = "agent-presets";

/// Runtime construction inputs beyond the portable roster config.
#[derive(Clone, Debug)]
pub struct AgentPresetRegistryConfig {
    /// Portable roster configuration.
    pub roster: AgentPresetConfig,
    /// Optional harness-home user root override for deterministic hosts and tests.
    pub user_root: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompositionStamp {
    modified: SystemTime,
    size: u64,
}

struct StandingMount {
    preset_id: String,
    key: ScopeKey,
    scope: Scope,
    _composition: LoadedComposition,
    stamp: CompositionStamp,
}

struct PresetSettings {
    service: Arc<SettingsService>,
    scope: SettingsScope,
}

impl std::fmt::Debug for StandingMount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StandingMount")
            .field("preset_id", &self.preset_id)
            .field("key", &self.key)
            .field("stamp", &self.stamp)
            .finish_non_exhaustive()
    }
}

/// Live first-root-wins roster with shared standing composition generations.
pub struct AgentPresetRegistry {
    context: Context,
    catalog: PluginCatalog,
    config: AgentPresetConfig,
    roots: Vec<PresetRoot>,
    standing: tokio::sync::Mutex<HashMap<String, Arc<StandingMount>>>,
    generations: Mutex<Vec<Arc<StandingMount>>>,
    bindings: Arc<Mutex<HashMap<ScopeKey, Arc<ScopeParentBinding>>>>,
    settings: Mutex<Option<PresetSettings>>,
}

impl std::fmt::Debug for AgentPresetRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentPresetRegistry")
            .field("default", &self.config.default)
            .field("roots", &self.roots)
            .field("generations", &self.generations.lock().len())
            .finish_non_exhaustive()
    }
}

impl AgentPresetRegistry {
    /// Constructs a roster and resolves its immutable root precedence.
    ///
    /// # Errors
    ///
    /// Returns when the harness-home user root cannot be resolved.
    pub fn new(
        context: &Context,
        catalog: PluginCatalog,
        config: AgentPresetRegistryConfig,
    ) -> anyhow::Result<Arc<Self>> {
        let mut roots = config.roster.roots.clone();
        if config.roster.include_user_root {
            let user_root = match config.user_root {
                Some(path) => path,
                None => seekdeep_util::home_paths::seekdeep_home_path([USER_PRESET_DIR])?,
            };
            roots.push(PresetRoot {
                path: user_root.to_string_lossy().into_owned(),
                trust: crate::PresetTrust::User,
            });
        }
        let registry = Arc::new(Self {
            context: context.clone(),
            catalog,
            config: config.roster,
            roots,
            standing: tokio::sync::Mutex::new(HashMap::new()),
            generations: Mutex::new(Vec::new()),
            bindings: Arc::new(Mutex::new(HashMap::new())),
            settings: Mutex::new(None),
        });
        if let Some(settings) = context.get(SETTINGS) {
            *registry.settings.lock() = Some(registry.attach_settings(settings)?);
        }
        registry.install_observers(context)?;
        Ok(registry)
    }

    /// Publishes the exact roster generation.
    ///
    /// # Errors
    ///
    /// Returns ordinary duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(AGENT_PRESETS, self.clone())
    }

    /// Composition default before the optional settings layer is mounted.
    #[must_use]
    pub fn default_id(&self) -> String {
        self.sync_settings();
        self.settings
            .lock()
            .as_ref()
            .and_then(|settings| settings.scope.get().get("default").cloned())
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| self.config.default.clone())
    }

    /// Roots in discovery and authoring precedence order.
    #[must_use]
    pub fn roots(&self) -> &[PresetRoot] {
        &self.roots
    }

    /// Whether any root accepts locally authored presets.
    #[must_use]
    pub fn authorable(&self) -> bool {
        self.roots
            .iter()
            .any(|root| root.trust == crate::PresetTrust::User)
    }

    /// Re-reads every root and returns the current first-wins roster.
    ///
    /// # Errors
    ///
    /// Returns a root discovery failure.
    pub async fn list(&self) -> anyhow::Result<Vec<AgentPreset>> {
        discover_presets(&self.roots).await
    }

    /// Resolves one preset or the current default.
    ///
    /// # Errors
    ///
    /// Returns root discovery or unknown-preset failures.
    pub async fn resolve(&self, id: Option<&str>) -> anyhow::Result<AgentPreset> {
        let default;
        let wanted = if let Some(id) = id {
            id
        } else {
            default = self.default_id();
            &default
        };
        let presets = self.list().await?;
        presets
            .iter()
            .find(|preset| preset.id == wanted)
            .cloned()
            .ok_or_else(|| {
                UnknownPresetError::new(
                    wanted,
                    presets.into_iter().map(|preset| preset.id).collect(),
                )
                .into()
            })
    }

    /// Resolves one composition and refuses a discovery-reported broken slot.
    ///
    /// # Errors
    ///
    /// Returns discovery, unknown-preset, or broken-composition failures.
    pub async fn resolve_mountable(&self, id: Option<&str>) -> anyhow::Result<AgentPreset> {
        let preset = self.resolve(id).await?;
        if let Some(reason) = &preset.broken {
            return Err(PresetMountError::new(&preset.id, reason).into());
        }
        Ok(preset)
    }

    /// Joins an Agent scope to the requested preset's standing generation.
    ///
    /// # Errors
    ///
    /// Returns unscoped, discovery, mount, audit, or parent-binding failures.
    pub async fn mount(
        self: &Arc<Self>,
        agent_context: &Context,
        id: Option<&str>,
    ) -> anyhow::Result<AgentPreset> {
        if scope_of(agent_context).is_none() {
            return Err(anyhow::anyhow!(
                "agent-presets: refusing to mount into an unscoped context; its registrations would apply to every agent in the process"
            ));
        }
        let preset = self.resolve_mountable(id).await?;
        self.mount_resolved(agent_context, preset).await
    }

    /// Joins an Agent scope to one already resolved mountable preset.
    ///
    /// # Errors
    ///
    /// Returns unscoped, mount, audit, or parent-binding failures.
    pub async fn mount_resolved(
        self: &Arc<Self>,
        agent_context: &Context,
        preset: AgentPreset,
    ) -> anyhow::Result<AgentPreset> {
        let key = scope_of(agent_context).ok_or_else(|| {
            anyhow::anyhow!(
                "agent-presets: refusing to mount into an unscoped context; its registrations would apply to every agent in the process"
            )
        })?;
        let standing = self.ensure_standing(&preset).await?;
        let binding = Arc::new(bind_scope_parent(key, standing.key)?);
        self.install_binding(agent_context, key, binding)?;
        Ok(preset)
    }

    /// Joins an Agent to the exact standing generation its parent uses.
    ///
    /// # Errors
    ///
    /// Returns for an unscoped or already-bound child.
    pub fn compose_from(
        self: &Arc<Self>,
        agent_context: &Context,
        parent_context: &Context,
    ) -> anyhow::Result<Option<String>> {
        let key = scope_of(agent_context).ok_or_else(|| {
            anyhow::anyhow!(
                "agent-presets: refusing to compose an unscoped context; the scope key is what joins an agent to its preset"
            )
        })?;
        let Some(parent_key) = scope_of(parent_context).and_then(scope_parent_of) else {
            return Ok(None);
        };
        let Some(standing) = self.standing_by_key(parent_key) else {
            return Ok(None);
        };
        let binding = Arc::new(bind_scope_parent(key, standing.key)?);
        self.install_binding(agent_context, key, binding)?;
        Ok(Some(standing.preset_id.clone()))
    }

    /// Preset id one live Agent scope currently joins.
    #[must_use]
    pub fn composed_preset(&self, agent_context: &Context) -> Option<String> {
        self.composed_preset_for_scope(scope_of(agent_context)?)
    }

    /// Preset id joined by one exact scope key.
    #[must_use]
    pub fn composed_preset_for_scope(&self, scope: ScopeKey) -> Option<String> {
        let parent = scope_parent_of(scope)?;
        self.standing_by_key(parent)
            .map(|standing| standing.preset_id.clone())
    }

    pub(crate) fn leaked_standing_services(&self) -> Vec<(String, Vec<String>)> {
        let providers = self.context.service_providers();
        self.generations
            .lock()
            .iter()
            .filter_map(|standing| {
                let leaked = providers
                    .iter()
                    .filter(|provider| provider.owner.is_within(&standing.scope.fiber()))
                    .filter(|provider| !provider.isolated && provider.name != "loader")
                    .map(|provider| provider.name.clone())
                    .collect::<Vec<_>>();
                (!leaked.is_empty()).then(|| (standing.preset_id.clone(), leaked))
            })
            .collect()
    }

    /// Re-links one already-scoped Agent to a validated standing composition.
    ///
    /// # Errors
    ///
    /// Returns unscoped, discovery, mount, audit, or binding failures. A failed
    /// target leaves the previous parent unchanged.
    pub async fn recompose(
        self: &Arc<Self>,
        agent_context: &Context,
        id: &str,
    ) -> anyhow::Result<AgentPreset> {
        let key = scope_of(agent_context).ok_or_else(|| {
            anyhow::anyhow!("agent-presets: refusing to recompose an unscoped context")
        })?;
        let preset = self.resolve_mountable(Some(id)).await?;
        let standing = self.ensure_standing(&preset).await?;
        if let Some(binding) = self.bindings.lock().get(&key).cloned() {
            binding.rebind(standing.key)?;
        } else {
            let binding = Arc::new(bind_scope_parent(key, standing.key)?);
            self.install_binding(agent_context, key, binding)?;
        }
        Ok(preset)
    }

    /// Resolves a preset's standing scope for an Agent-free Host read.
    ///
    /// # Errors
    ///
    /// Returns discovery, unknown-preset, mount, or audit failures.
    pub async fn standing_key_for(&self, id: Option<&str>) -> anyhow::Result<ScopeKey> {
        let preset = self.resolve_mountable(id).await?;
        Ok(self.ensure_standing(&preset).await?.key)
    }

    /// Reads one service implementation owned by an Agent's preset subtree.
    #[must_use]
    pub fn service_for<T: Service>(&self, agent: &Agent, key: ServiceKey<T>) -> Option<Arc<T>> {
        let parent = scope_parent_of(agent.scope_key())?;
        let standing = self.standing_by_key(parent)?;
        self.context
            .service_from_fiber(key, &standing.scope.fiber())
    }

    /// Reads one composition exactly as stored.
    ///
    /// # Errors
    ///
    /// Returns discovery, unknown-preset, or filesystem failures.
    pub async fn read(&self, id: &str) -> anyhow::Result<String> {
        read_composition(&self.resolve(Some(id)).await?).await
    }

    /// Copies one resolved preset after refusing every roster-visible target id.
    ///
    /// # Errors
    ///
    /// Returns discovery, unknown-source, occupied-roster, or authoring failures.
    pub async fn copy(&self, from: &str, id: &str, name: Option<&str>) -> anyhow::Result<()> {
        let source = self.resolve(Some(from)).await?;
        if self.list().await?.iter().any(|preset| preset.id == id) {
            return Err(crate::PresetExistsError {
                preset_id: id.to_owned(),
            }
            .into());
        }
        copy_composition(&self.roots, &source, id, name).await?;
        self.standing.lock().await.remove(id);
        Ok(())
    }

    /// Deletes one locally authored preset while joined generations keep running.
    ///
    /// # Errors
    ///
    /// Returns discovery, unknown-preset, ownership, or filesystem failures.
    pub async fn remove(&self, id: &str) -> anyhow::Result<()> {
        let preset = self.resolve(Some(id)).await?;
        delete_composition(&self.roots, &preset).await?;
        self.standing.lock().await.remove(id);
        self.sync_settings();
        let current = self
            .settings
            .lock()
            .as_ref()
            .map(|settings| (settings.service.clone(), settings.scope.get()));
        if let Some((settings, value)) = current
            && value.get("default").and_then(Value::as_str) == Some(id)
        {
            settings
                .mutate(
                    &settings_namespace(SETTINGS_NAMESPACE)?,
                    vec![SettingsPathOp::Unset {
                        path: vec!["default".to_owned()],
                    }],
                    None,
                )
                .await?;
        }
        Ok(())
    }

    fn attach_settings(&self, settings: Arc<SettingsService>) -> anyhow::Result<PresetSettings> {
        let namespace = settings_namespace(SETTINGS_NAMESPACE)?;
        let scope = settings.register(
            &self.context,
            &namespace,
            Schema::object([("default", Schema::string())]),
            seekdeep_settings::SettingsRegisterOptions {
                base: Some(json!({ "default": self.config.default })),
                ..seekdeep_settings::SettingsRegisterOptions::default()
            },
        )?;
        Ok(PresetSettings {
            service: settings,
            scope,
        })
    }

    fn sync_settings(&self) {
        let current = self.context.get(SETTINGS);
        let mut attached = self.settings.lock();
        if current
            .as_ref()
            .zip(attached.as_ref())
            .is_some_and(|(current, attached)| Arc::ptr_eq(current, &attached.service))
        {
            return;
        }
        if let Some(settings) = current {
            match self.attach_settings(settings) {
                Ok(settings) => *attached = Some(settings),
                Err(error) => {
                    tracing::warn!(%error, "Agent preset settings attachment failed");
                    *attached = None;
                }
            }
        } else {
            *attached = None;
        }
    }

    fn install_observers(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let weak = Arc::downgrade(self);
        context.events().on_sync(
            context,
            "agent/created",
            move |_, args| {
                let Some(registry) = weak.upgrade() else {
                    return Ok(EventReply::Undefined);
                };
                if registry.roots.is_empty() {
                    return Ok(EventReply::Undefined);
                }
                let event = args
                    .get::<AgentLifecycleEvent>(0)
                    .ok_or_else(|| anyhow::anyhow!("agent/created lacks its Agent event"))?;
                if registry.composed_preset(event.agent.context()).is_none() {
                    tracing::warn!(
                        agent = %event.agent.id(),
                        "Agent was published without joining an Agent preset; its model-facing registries resolve against the global layer"
                    );
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        let emit_context = context.clone();
        context.events().on_sync(
            context,
            "session/event",
            move |_, args| {
                let session = args
                    .get::<Session>(0)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks a session"))?;
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks an event"))?;
                if event.event_type != "agent-preset/selected" {
                    return Ok(EventReply::Undefined);
                }
                let Some(preset) = event.data.get("agentPreset").and_then(Value::as_str) else {
                    return Ok(EventReply::Undefined);
                };
                let emission = emit_context.events().prepare_emit(
                    &emit_context,
                    "agent-preset/selected",
                    &EventArgs::from_values(vec![
                        Arc::new(session.id().clone()),
                        Arc::new(preset.to_owned()),
                    ]),
                )?;
                emission.emit_contained(|error| {
                    tracing::warn!(%error, "agent-preset/selected observer failed");
                });
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        Ok(())
    }

    async fn ensure_standing(&self, preset: &AgentPreset) -> anyhow::Result<Arc<StandingMount>> {
        let mut standing = self.standing.lock().await;
        if let Some(current) = standing.get(&preset.id).cloned() {
            let stamp = composition_stamp(&preset.path).await;
            if stamp.as_ref().is_none_or(|stamp| stamp == &current.stamp) {
                return Ok(current);
            }
        }
        let created = self.mount_standing(preset).await?;
        standing.insert(preset.id.clone(), created.clone());
        self.generations.lock().push(created.clone());
        Ok(created)
    }

    async fn mount_standing(&self, preset: &AgentPreset) -> anyhow::Result<Arc<StandingMount>> {
        let stamp = composition_stamp(&preset.path).await.ok_or_else(|| {
            PresetMountError::new(
                &preset.id,
                format!("composition file is unreadable: {}", preset.path.display()),
            )
        })?;
        let mut scope = create_scope(&self.context, ScopeKey::new(), None)?;
        scope.context = scope.context.isolate(LOADER);
        let composition = match self.catalog.load_file(&scope.context, &preset.path).await {
            Ok(composition) => composition,
            Err(error) => {
                let _ = scope.dispose().await;
                return Err(PresetMountError::new(&preset.id, format!("{error:#}")).into());
            }
        };
        let inactive = composition
            .entries()
            .into_iter()
            .filter(|entry| !entry.disabled && !entry.group)
            .filter(|entry| entry.state != Some(seekdeep_cordis::FiberState::Active))
            .map(|entry| {
                format!(
                    "{} ({}): never became active",
                    entry.id,
                    entry.plugin.as_str()
                )
            })
            .collect::<Vec<_>>();
        let leaked = self
            .context
            .service_providers()
            .into_iter()
            .filter(|provider| provider.owner.is_within(&scope.fiber()))
            .filter(|provider| !provider.isolated && provider.name != "loader")
            .map(|provider| provider.name)
            .collect::<Vec<_>>();
        if !inactive.is_empty() || !leaked.is_empty() {
            let reason = if inactive.is_empty() {
                format!(
                    "services escaped the preset scope into the process root: {}",
                    leaked.join(", ")
                )
            } else {
                format!("inactive rows:\n{}", inactive.join("\n"))
            };
            let _ = scope.dispose().await;
            return Err(PresetMountError::new(&preset.id, reason).into());
        }
        let fiber = scope.fiber();
        let cleanup_fiber = fiber.clone();
        self.context.own(EffectHandle::new(
            format!("agent preset {}", preset.id),
            move || Box::pin(async move { cleanup_fiber.dispose().await }),
        ))?;
        Ok(Arc::new(StandingMount {
            preset_id: preset.id.clone(),
            key: scope_of(&scope.context).expect("created scope carries a key"),
            scope,
            _composition: composition,
            stamp,
        }))
    }

    fn install_binding(
        self: &Arc<Self>,
        agent_context: &Context,
        key: ScopeKey,
        binding: Arc<ScopeParentBinding>,
    ) -> anyhow::Result<()> {
        self.bindings.lock().insert(key, binding.clone());
        let rollback = binding.clone();
        let bindings: Weak<Mutex<HashMap<ScopeKey, Arc<ScopeParentBinding>>>> =
            Arc::downgrade(&self.bindings);
        let cleanup = EffectHandle::synchronous("agent preset scope binding", move || {
            binding.unbind();
            if let Some(bindings) = bindings.upgrade() {
                bindings.lock().remove(&key);
            }
            Ok(())
        });
        if let Err(error) = agent_context.own(cleanup) {
            rollback.unbind();
            self.bindings.lock().remove(&key);
            return Err(error.into());
        }
        Ok(())
    }

    fn standing_by_key(&self, key: ScopeKey) -> Option<Arc<StandingMount>> {
        self.generations
            .lock()
            .iter()
            .rev()
            .find(|standing| standing.key == key)
            .cloned()
    }
}

async fn composition_stamp(path: &std::path::Path) -> Option<CompositionStamp> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    Some(CompositionStamp {
        modified: metadata.modified().ok()?,
        size: metadata.len(),
    })
}
