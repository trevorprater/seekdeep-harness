//! Tool-independent trusted environment registry for model-facing shell calls.

use std::{
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    sync::Arc,
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, CordisError, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_shell::{SEEKDEEP_ENV_PREFIX, SeekDeepEnvironment, SeekDeepEnvironmentKey};
use seekdeep_tools::ToolExecution;
use seekdeep_util::home_paths::{
    SEEKDEEP_HOME_ENV, resolve_process_seekdeep_home, resolve_seekdeep_home,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Explained-empty invariant companion.
pub mod invariant;

/// Cordis plugin name.
pub const NAME: &str = "shell-env";
/// The registry has no mandatory service dependencies.
pub const INJECT: &[&str] = &[];
/// Typed Cordis seat corresponding to `ctx.shellEnv`.
pub const SHELL_ENV: ServiceKey<ShellEnvRegistry> = ServiceKey::new("shellEnv");

/// Registry configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ShellEnvConfig {
    /// Explicit `SeekDeep` home, before `SEEKDEEP_HOME` and `~/.seekdeep`.
    pub seekdeep_home: Option<String>,
}

/// Model-visible metadata for one managed `SEEKDEEP_*` variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellEnvVariable {
    /// Concise description of the environment fact.
    pub description: String,
}

/// Raw runtime values returned by a contributor.
///
/// Raw string keys and JSON values deliberately retain the source runtime's
/// undeclared-key and non-string-value failure modes at compatibility edges.
pub type ShellEnvResolvedValues = IndexMap<String, Value>;

/// Per-execution resolver owned by one contribution.
pub type ShellEnvResolver =
    Arc<dyn Fn(&ToolExecution) -> anyhow::Result<ShellEnvResolvedValues> + Send + Sync>;

/// One plugin contribution to every managed shell environment snapshot.
#[derive(Clone)]
pub struct ShellEnvContributor {
    /// Stable contributor identity.
    pub name: String,
    /// Complete insertion-ordered declaration set.
    pub variables: IndexMap<String, ShellEnvVariable>,
    /// Synchronous per-execution resolver.
    pub resolve: ShellEnvResolver,
}

impl std::fmt::Debug for ShellEnvContributor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellEnvContributor")
            .field("name", &self.name)
            .field("variables", &self.variables)
            .finish_non_exhaustive()
    }
}

/// Enumerable declaration returned by [`ShellEnvRegistry::list`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellEnvVariableInfo {
    /// Contributor that owns this key.
    pub contributor: String,
    /// Declared managed variable name.
    pub key: String,
    /// Concise description of the environment fact.
    pub description: String,
}

#[derive(Clone)]
struct RegisteredContributor {
    id: Uuid,
    contributor: ShellEnvContributor,
}

#[derive(Default)]
struct RegistryState {
    contributors: HashMap<String, RegisteredContributor>,
    key_owners: HashMap<String, String>,
}

/// Trusted per-execution `SEEKDEEP_*` environment registry.
pub struct ShellEnvRegistry {
    seekdeep_home: String,
    state: Mutex<RegistryState>,
}

impl std::fmt::Debug for ShellEnvRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellEnvRegistry")
            .field("seekdeep_home", &self.seekdeep_home)
            .field("contributors", &self.state.lock().contributors.keys())
            .finish()
    }
}

impl ShellEnvRegistry {
    /// Resolves process configuration and constructs a registry.
    ///
    /// # Errors
    ///
    /// Returns when the configured, ambient, or operating-system home cannot
    /// be resolved to an absolute path.
    pub fn new(config: &ShellEnvConfig) -> anyhow::Result<Arc<Self>> {
        let home = resolve_process_seekdeep_home(config.seekdeep_home.as_deref().map(OsStr::new))?;
        Ok(Arc::new(Self {
            seekdeep_home: home.to_string_lossy().into_owned(),
            state: Mutex::new(RegistryState::default()),
        }))
    }

    /// Deterministic constructor for environment-precedence tests and hosts.
    ///
    /// # Errors
    ///
    /// Returns when the selected path cannot be resolved.
    #[doc(hidden)]
    pub fn new_with_environment(
        config: &ShellEnvConfig,
        environment: &HashMap<OsString, OsString>,
    ) -> anyhow::Result<Arc<Self>> {
        let home =
            resolve_seekdeep_home(config.seekdeep_home.as_deref().map(OsStr::new), environment)?;
        Ok(Arc::new(Self {
            seekdeep_home: home.to_string_lossy().into_owned(),
            state: Mutex::new(RegistryState::default()),
        }))
    }

    /// Publishes the registry on the exact `shellEnv` service seat.
    ///
    /// # Errors
    ///
    /// Returns ordinary duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        match context.provide(SHELL_ENV, self.clone()) {
            Ok(effect) => Ok(effect),
            Err(CordisError::DuplicateService(_)) => {
                anyhow::bail!("service \"shellEnv\" has been registered")
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Registers one contributor with atomic ownership validation.
    ///
    /// The returned effect is both an explicit disposer and an owner-scoped
    /// registration. Disposal removes only this exact registration epoch.
    ///
    /// # Errors
    ///
    /// Returns for blank/duplicate names, malformed/reserved/duplicate keys,
    /// blank descriptions, or an inactive owner.
    pub fn register(
        self: &Arc<Self>,
        owner: &Context,
        contributor: ShellEnvContributor,
    ) -> anyhow::Result<EffectHandle> {
        let name = contributor.name.clone();
        let variables = contributor
            .variables
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        let id = Uuid::now_v7();
        {
            let mut state = self.state.lock();
            anyhow::ensure!(
                !contributor.name.trim().is_empty(),
                "bash env contributor name must be non-empty"
            );
            anyhow::ensure!(
                !state.contributors.contains_key(&contributor.name),
                "bash env contributor \"{}\" is already registered",
                contributor.name
            );
            for (key, variable) in &variables {
                anyhow::ensure!(
                    valid_key(key),
                    "bash env contributor \"{}\" declared invalid key \"{key}\"",
                    contributor.name
                );
                anyhow::ensure!(
                    !reserved_key(key),
                    "bash env contributor \"{}\" cannot own reserved key \"{key}\"",
                    contributor.name
                );
                anyhow::ensure!(
                    !variable.description.trim().is_empty(),
                    "bash env contributor \"{}\" must describe \"{key}\"",
                    contributor.name
                );
                if let Some(existing) = state.key_owners.get(key) {
                    anyhow::bail!(
                        "bash env key \"{key}\" is already owned by contributor \"{existing}\"; contributor \"{}\" cannot also own it",
                        contributor.name
                    );
                }
            }
            for (key, _) in &variables {
                state
                    .key_owners
                    .insert(key.clone(), contributor.name.clone());
            }
            state.contributors.insert(
                contributor.name.clone(),
                RegisteredContributor { id, contributor },
            );
        }

        let registry = Arc::downgrade(self);
        let disposal_name = name.clone();
        let effect = EffectHandle::synchronous("bashEnv.register()", move || {
            if let Some(registry) = registry.upgrade() {
                registry.remove_registration(&disposal_name, id);
            }
            Ok(())
        });
        if let Err(error) = owner.own(effect.clone()) {
            self.remove_registration(&name, id);
            return Err(error.into());
        }
        Ok(effect)
    }

    fn remove_registration(&self, name: &str, id: Uuid) {
        let mut state = self.state.lock();
        let Some(entry) = state
            .contributors
            .get(name)
            .filter(|entry| entry.id == id)
            .cloned()
        else {
            return;
        };
        state.contributors.remove(name);
        for key in entry.contributor.variables.keys() {
            if state.key_owners.get(key).is_some_and(|owner| owner == name) {
                state.key_owners.remove(key);
            }
        }
    }

    /// Builds one immutable, lexically sorted trusted environment snapshot.
    ///
    /// # Errors
    ///
    /// Propagates resolver failures and rejects undeclared keys or non-string
    /// runtime values before returning any snapshot.
    pub fn collect(&self, execution: &ToolExecution) -> anyhow::Result<SeekDeepEnvironment> {
        let contributors = {
            let state = self.state.lock();
            let mut contributors = state
                .contributors
                .values()
                .map(|entry| entry.contributor.clone())
                .collect::<Vec<_>>();
            contributors.sort_by(|left, right| left.name.cmp(&right.name));
            contributors
        };
        let mut values = BTreeMap::new();
        insert_value(&mut values, SEEKDEEP_HOME_ENV, &self.seekdeep_home)?;
        insert_value(&mut values, "SEEKDEEP_SHELL", "1")?;
        if let Some(agent) = &execution.agent {
            insert_value(
                &mut values,
                "SEEKDEEP_SESSION_ID",
                agent.session().id().as_str(),
            )?;
        }
        for contributor in contributors {
            for (key, value) in (contributor.resolve)(execution)? {
                anyhow::ensure!(
                    contributor.variables.contains_key(&key),
                    "bash env contributor \"{}\" returned undeclared key \"{key}\"",
                    contributor.name
                );
                let value = value.as_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "bash env contributor \"{}\" returned a non-string value for \"{key}\"",
                        contributor.name
                    )
                })?;
                insert_value(&mut values, &key, value)?;
            }
        }
        Ok(SeekDeepEnvironment::new(values))
    }

    /// Lists contributed declarations without invoking any resolver.
    #[must_use]
    pub fn list(&self) -> Vec<ShellEnvVariableInfo> {
        let state = self.state.lock();
        let mut declarations = state
            .contributors
            .values()
            .flat_map(|entry| {
                entry
                    .contributor
                    .variables
                    .iter()
                    .map(|(key, variable)| ShellEnvVariableInfo {
                        contributor: entry.contributor.name.clone(),
                        key: key.clone(),
                        description: variable.description.clone(),
                    })
            })
            .collect::<Vec<_>>();
        declarations.sort_by(|left, right| left.key.cmp(&right.key));
        declarations
    }
}

fn insert_value(
    values: &mut BTreeMap<SeekDeepEnvironmentKey, String>,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    values.insert(SeekDeepEnvironmentKey::new(key)?, value.to_owned());
    Ok(())
}

fn valid_key(key: &str) -> bool {
    let Some(suffix) = key.strip_prefix(SEEKDEEP_ENV_PREFIX) else {
        return false;
    };
    let mut bytes = suffix.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_uppercase())
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn reserved_key(key: &str) -> bool {
    matches!(
        key,
        SEEKDEEP_HOME_ENV | "SEEKDEEP_SHELL" | "SEEKDEEP_SESSION_ID"
    )
}

fn javascript_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(javascript_value)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn validate_plugin_config(value: &Value) -> anyhow::Result<Value> {
    if value.is_null() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("expected object but got {}", javascript_value(value)))?;
    if let Some(home) = object.get("seekdeepHome") {
        anyhow::ensure!(
            home.is_string(),
            "$.seekdeepHome expected string but got {}",
            javascript_value(home)
        );
    }
    Ok(value.clone())
}

/// Installs the service and backend-neutral JSONL-location contributor.
///
/// # Errors
///
/// Returns home-resolution, service-registration, or contribution failures.
pub fn apply(context: &Context, config: &ShellEnvConfig) -> anyhow::Result<Arc<ShellEnvRegistry>> {
    let registry = ShellEnvRegistry::new(config)?;
    registry.provide(context)?;
    let persistence_context = context.clone();
    registry.register(
        context,
        ShellEnvContributor {
            name: "session-persistence".to_owned(),
            variables: IndexMap::from([(
                "SEEKDEEP_SESSION_JSONL".to_owned(),
                ShellEnvVariable {
                    description: "Absolute target path of the current session JSONL when the active persistence backend provides one.".to_owned(),
                },
            )]),
            resolve: Arc::new(move |execution| {
                let Some(agent) = &execution.agent else {
                    return Ok(IndexMap::new());
                };
                let Some(persistence) = persistence_context.get(SESSION_PERSISTENCE) else {
                    return Ok(IndexMap::new());
                };
                let Some(location) = persistence.persistence().locate(agent.session().header())
                else {
                    return Ok(IndexMap::new());
                };
                if location.kind != "jsonl" {
                    return Ok(IndexMap::new());
                }
                Ok(IndexMap::from([(
                    "SEEKDEEP_SESSION_JSONL".to_owned(),
                    Value::String(location.path.to_string_lossy().into_owned()),
                )]))
            }),
        },
    )?;
    Ok(registry)
}

/// Builds the Loader-compatible plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: ShellEnvConfig = serde_json::from_value(config)?;
            apply(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(validate_plugin_config)
}
