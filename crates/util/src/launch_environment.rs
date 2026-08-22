//! Immutable launch-time environment snapshots with layer provenance.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use seekdeep_cordis::{Context, ServiceKey};

/// Environment layer, ordered from most to least trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LaunchEnvironmentSource {
    /// Environment inherited by this process.
    Process,
    /// Invoking project's `.env` file.
    ProjectEnv,
    /// `SeekDeep` home `.env` file.
    UserEnv,
}

const SOURCE_ORDER: [LaunchEnvironmentSource; 3] = [
    LaunchEnvironmentSource::Process,
    LaunchEnvironmentSource::ProjectEnv,
    LaunchEnvironmentSource::UserEnv,
];

/// One resolved value and the layer that supplied it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchEnvironmentEntry {
    /// Exact value, including an intentionally empty value.
    pub value: String,
    /// Winning layer.
    pub source: LaunchEnvironmentSource,
    /// Absolute source file path, absent for the process layer.
    pub path: Option<PathBuf>,
}

/// Raw input for one environment layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchEnvironmentLayerInput {
    /// Layer identity.
    pub source: LaunchEnvironmentSource,
    /// Absolute source file path, absent for the process layer.
    pub path: Option<PathBuf>,
    /// Exact variable contents.
    pub values: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct FrozenLayer {
    path: Option<PathBuf>,
    values: BTreeMap<String, String>,
}

/// Frozen environment of one launch.
#[derive(Clone, Debug, Default)]
pub struct LaunchEnvironmentSnapshot {
    layers: BTreeMap<LaunchEnvironmentSource, FrozenLayer>,
}

impl LaunchEnvironmentSnapshot {
    /// Resolves a name across every layer in canonical trust order.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<LaunchEnvironmentEntry> {
        self.get_from(name, &SOURCE_ORDER)
    }

    /// Resolves a name only from the allowed layers, while retaining canonical
    /// trust order regardless of the slice's order.
    #[must_use]
    pub fn get_from(
        &self,
        name: &str,
        sources: &[LaunchEnvironmentSource],
    ) -> Option<LaunchEnvironmentEntry> {
        let key = lookup_key(name);
        SOURCE_ORDER.iter().find_map(|source| {
            if !sources.contains(source) {
                return None;
            }
            let layer = self.layers.get(source)?;
            let value = layer.values.get(&key)?;
            Some(LaunchEnvironmentEntry {
                value: value.clone(),
                source: *source,
                path: layer.path.clone(),
            })
        })
    }

    /// Materializes the winning value for every name in deterministic order.
    #[must_use]
    pub fn materialized(&self) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        for source in SOURCE_ORDER {
            let Some(layer) = self.layers.get(&source) else {
                continue;
            };
            for (name, value) in &layer.values {
                values.entry(name.clone()).or_insert_with(|| value.clone());
            }
        }
        values
    }
}

/// Builds a frozen snapshot. Input order does not influence lookup trust;
/// repeated layers use the last supplied layer, matching JavaScript `Map#set`.
#[must_use]
pub fn create_launch_environment_snapshot(
    layers: &[LaunchEnvironmentLayerInput],
) -> LaunchEnvironmentSnapshot {
    let mut frozen = BTreeMap::new();
    for layer in layers {
        frozen.insert(
            layer.source,
            FrozenLayer {
                path: layer.path.clone(),
                values: layer
                    .values
                    .iter()
                    .map(|(name, value)| (lookup_key(name), value.clone()))
                    .collect(),
            },
        );
    }
    LaunchEnvironmentSnapshot { layers: frozen }
}

#[cfg(windows)]
fn lookup_key(name: &str) -> String {
    lookup_key_for(name, true)
}

#[cfg(not(windows))]
fn lookup_key(name: &str) -> String {
    lookup_key_for(name, false)
}

fn lookup_key_for(name: &str, case_insensitive: bool) -> String {
    if case_insensitive {
        name.to_uppercase()
    } else {
        name.to_owned()
    }
}

/// Launcher-owned snapshot service slot.
pub const SEEKDEEP_LAUNCH_ENVIRONMENT: ServiceKey<LaunchEnvironmentSnapshot> =
    ServiceKey::new("launchEnvironment");

/// Returns the launcher snapshot or a frozen process-only fallback.
#[must_use]
pub fn launch_environment_of(context: &Context) -> Arc<LaunchEnvironmentSnapshot> {
    context.get(SEEKDEEP_LAUNCH_ENVIRONMENT).unwrap_or_else(|| {
        let values = std::env::vars_os()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect();
        Arc::new(create_launch_environment_snapshot(&[
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::Process,
                path: None,
                values,
            },
        ]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn layered() -> LaunchEnvironmentSnapshot {
        create_launch_environment_snapshot(&[
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::Process,
                path: None,
                values: values(&[("SHARED", "from-process"), ("ONLY_PROCESS", "p")]),
            },
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::ProjectEnv,
                path: Some("/work/.env".into()),
                values: values(&[("SHARED", "from-project"), ("ONLY_PROJECT", "j")]),
            },
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::UserEnv,
                path: Some("/home/.seekdeep/.env".into()),
                values: values(&[("SHARED", "from-user"), ("ONLY_USER", "u")]),
            },
        ])
    }

    #[test]
    fn resolves_canonical_precedence_and_provenance() {
        let snapshot = layered();
        assert_eq!(
            snapshot.get("SHARED"),
            Some(LaunchEnvironmentEntry {
                value: "from-process".into(),
                source: LaunchEnvironmentSource::Process,
                path: None
            })
        );
        assert_eq!(
            snapshot.get("ONLY_PROJECT"),
            Some(LaunchEnvironmentEntry {
                value: "j".into(),
                source: LaunchEnvironmentSource::ProjectEnv,
                path: Some("/work/.env".into())
            })
        );
        assert_eq!(
            snapshot.get("ONLY_USER"),
            Some(LaunchEnvironmentEntry {
                value: "u".into(),
                source: LaunchEnvironmentSource::UserEnv,
                path: Some("/home/.seekdeep/.env".into())
            })
        );
        assert_eq!(snapshot.get("ABSENT"), None);
    }

    #[test]
    fn source_filter_never_reorders_trust() {
        let snapshot = layered();
        assert_eq!(
            snapshot.get_from(
                "ONLY_PROJECT",
                &[
                    LaunchEnvironmentSource::Process,
                    LaunchEnvironmentSource::UserEnv
                ]
            ),
            None
        );
        assert_eq!(
            snapshot
                .get_from(
                    "SHARED",
                    &[
                        LaunchEnvironmentSource::UserEnv,
                        LaunchEnvironmentSource::Process
                    ]
                )
                .unwrap()
                .source,
            LaunchEnvironmentSource::Process
        );
        assert_eq!(snapshot.get_from("SHARED", &[]), None);
    }

    #[test]
    fn materialization_uses_canonical_precedence_and_sorted_names() {
        assert_eq!(
            layered().materialized(),
            BTreeMap::from([
                ("ONLY_PROCESS".to_owned(), "p".to_owned()),
                ("ONLY_PROJECT".to_owned(), "j".to_owned()),
                ("ONLY_USER".to_owned(), "u".to_owned()),
                ("SHARED".to_owned(), "from-process".to_owned()),
            ])
        );
    }

    #[test]
    fn snapshot_copies_values_and_keeps_empty_values_present() {
        let mut mutable = values(&[("KEY", "first"), ("EMPTY", "")]);
        let snapshot = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: mutable.clone(),
        }]);
        mutable.insert("KEY".into(), "second".into());
        mutable.insert("LATE".into(), "added".into());
        assert_eq!(snapshot.get("KEY").unwrap().value, "first");
        assert_eq!(snapshot.get("EMPTY").unwrap().value, "");
        assert_eq!(snapshot.get("LATE"), None);
    }

    #[test]
    fn construction_order_does_not_change_trust_order() {
        let snapshot = create_launch_environment_snapshot(&[
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::UserEnv,
                path: Some("/u".into()),
                values: values(&[("K", "u")]),
            },
            LaunchEnvironmentLayerInput {
                source: LaunchEnvironmentSource::Process,
                path: None,
                values: values(&[("K", "p")]),
            },
        ]);
        assert_eq!(snapshot.get("K").unwrap().value, "p");
    }

    #[test]
    fn platform_lookup_key_preserves_posix_and_folds_windows_names() {
        assert_eq!(
            lookup_key_for("DeepSeek_Api_Key", false),
            "DeepSeek_Api_Key"
        );
        assert_eq!(lookup_key_for("DeepSeek_Api_Key", true), "DEEPSEEK_API_KEY");
        #[cfg(windows)]
        assert_eq!(lookup_key("DeepSeek_Api_Key"), "DEEPSEEK_API_KEY");
        #[cfg(not(windows))]
        assert_eq!(lookup_key("DeepSeek_Api_Key"), "DeepSeek_Api_Key");
    }

    #[test]
    fn context_returns_exact_provided_snapshot() {
        let context = Context::new();
        let snapshot = Arc::new(layered());
        context
            .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, snapshot.clone())
            .unwrap();
        assert!(Arc::ptr_eq(&launch_environment_of(&context), &snapshot));
    }

    #[test]
    fn context_fallback_contains_process_environment() {
        let snapshot = launch_environment_of(&Context::new());
        let (name, value) = std::env::vars_os()
            .next()
            .expect("test process has an environment");
        let name = name.to_string_lossy();
        assert_eq!(snapshot.get(&name).unwrap().value, value.to_string_lossy());
        assert_eq!(
            snapshot.get(&name).unwrap().source,
            LaunchEnvironmentSource::Process
        );
    }
}
