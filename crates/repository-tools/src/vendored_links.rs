//! Vendored package lockfile link-integrity policy.

use std::{collections::HashSet, path::Path};

use serde_json::Value as JsonValue;
use serde_yml::{Mapping, Value};

/// Vendored-link inspection result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VendoredLinkReport {
    /// Vendored package names discovered under `vendor/`.
    pub vendored_packages: usize,
    /// Ordered importer or registry-copy violations.
    pub violations: Vec<String>,
}

/// Verifies vendored dependency resolutions in `pnpm-lock.yaml`.
///
/// # Errors
///
/// Returns vendor traversal, manifest/lockfile read/parse/shape, or empty-vendor failures.
pub fn inspect_vendored_links(root: &Path) -> anyhow::Result<VendoredLinkReport> {
    let names = vendored_names(root)?;
    anyhow::ensure!(
        !names.is_empty(),
        "verify-vendored-links: no vendored package manifests found under vendor/"
    );
    let lockfile: Value = serde_yml::from_slice(&std::fs::read(root.join("pnpm-lock.yaml"))?)?;
    let lockfile = lockfile
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("pnpm-lock.yaml must contain a YAML object"))?;
    let mut violations = Vec::new();

    if let Some(importers) = mapping_value(lockfile, "importers").and_then(Value::as_mapping) {
        for (importer, sections) in importers {
            let Some(importer) = importer.as_str() else {
                continue;
            };
            let Some(sections) = sections.as_mapping() else {
                continue;
            };
            for (section, dependencies) in sections {
                let Some(section) = section.as_str() else {
                    continue;
                };
                let Some(dependencies) = dependencies.as_mapping() else {
                    continue;
                };
                for (dependency, entry) in dependencies {
                    let Some(dependency) = dependency.as_str() else {
                        continue;
                    };
                    if !names.contains(dependency) {
                        continue;
                    }
                    let version = entry
                        .as_mapping()
                        .and_then(|entry| mapping_value(entry, "version"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !version.starts_with("link:") {
                        violations.push(format!(
                            "{importer} {section}.{dependency} resolves to {} (expected link:)",
                            serde_json::to_string(version)?
                        ));
                    }
                }
            }
        }
    }

    for section in ["packages", "snapshots"] {
        let Some(entries) = mapping_value(lockfile, section).and_then(Value::as_mapping) else {
            continue;
        };
        for key in entries.keys().filter_map(Value::as_str) {
            let Some(index) = key.rfind('@').filter(|index| *index > 0) else {
                continue;
            };
            let package_name = &key[..index];
            if names.contains(package_name) {
                violations.push(format!(
                    "{section} entry {key} is a registry copy of a vendored package"
                ));
            }
        }
    }
    Ok(VendoredLinkReport {
        vendored_packages: names.len(),
        violations,
    })
}

fn vendored_names(root: &Path) -> anyhow::Result<HashSet<String>> {
    let mut names = HashSet::new();
    let vendor = root.join("vendor");
    for entry in std::fs::read_dir(vendor)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path().join("package.json")) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<JsonValue>(&bytes) else {
            continue;
        };
        if let Some(name) = manifest.get("name").and_then(JsonValue::as_str) {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

fn mapping_value<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(Value::String(key.to_owned()))
}
