use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail};
use indexmap::IndexMap;
use serde_json::Value;

use super::{
    BuildTimeTool, CLAUDE_AGENT_SDK_PACKAGE, CLAUDE_PLATFORM_DECLARED_LICENSE, ClaudeDistribution,
    ExternalDep, FIRST_PARTY, Manifest, Metadata, NoticeCollection, PatchedDependency, VendoredRow,
    claude_distribution_from_manifest, collect_python_dependencies, js_entries, locale_compare,
    manifest_patterns, normalize_repo, parse_vendored_rows, policy, printed, read_manifest,
    sorted_entries, tier_external_deps, virtual_manifest,
};

fn glob_paths(root: &Path, pattern: &str) -> anyhow::Result<Vec<PathBuf>> {
    glob::glob(&format!(
        "{}/{}",
        glob::Pattern::escape(&root.to_string_lossy()),
        pattern
    ))?
    .map(|entry| entry.map_err(anyhow::Error::from))
    .collect()
}

fn load_workspace_manifests(
    root: &Path,
) -> anyhow::Result<(IndexMap<String, Manifest>, HashSet<String>)> {
    let workspace: serde_yml::Value =
        serde_yml::from_slice(&fs::read(root.join("pnpm-workspace.yaml"))?)?;
    let members = workspace.get("packages").and_then(serde_yml::Value::as_sequence).filter(|members| !members.is_empty()).context("gen-third-party-notices: pnpm-workspace.yaml declares no workspace members; the manifest set cannot be derived.")?;
    let members = members.iter().map(yaml_string).collect::<Vec<_>>();
    let mut manifests = IndexMap::new();
    let mut names = HashSet::new();
    for pattern in manifest_patterns(&members) {
        for file in glob_paths(root, &pattern)? {
            let manifest = read_manifest(&file)?;
            if let Some(name) = manifest.get("name").and_then(Value::as_str) {
                names.insert(name.to_owned());
            }
            manifests.insert(
                file.strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                manifest,
            );
        }
    }
    if manifests.len() < 100 {
        bail!(
            "gen-third-party-notices: only {} workspace manifests found; the glob set is stale.",
            manifests.len()
        );
    }
    Ok((manifests, names))
}

fn installed_manifest(root: &Path, name: &str) -> anyhow::Result<Option<Manifest>> {
    for store in ["node_modules", "native/landlock-run/node_modules"] {
        let direct = root.join(store).join(name).join("package.json");
        if direct.exists() {
            return read_manifest(&direct).map(Some);
        }
        let virtual_store = root.join(store).join(".pnpm");
        if virtual_store.exists()
            && let Some(manifest) = virtual_manifest(&virtual_store, name)?
        {
            return Ok(Some(manifest));
        }
    }
    Ok(None)
}

fn installed_metadata(root: &Path, name: &str) -> anyhow::Result<Metadata> {
    let override_metadata = policy().overrides.get(name);
    let manifest = installed_manifest(root, name)?;
    let license = override_metadata
        .and_then(|metadata| metadata.license.clone())
        .or_else(|| {
            manifest
                .as_ref()
                .and_then(|manifest| manifest.get("license"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let repository = manifest.as_ref().and_then(|manifest| {
        let repository = manifest.get("repository");
        repository.and_then(Value::as_str).or_else(|| {
            repository
                .and_then(|repository| repository.get("url"))
                .filter(|url| !url.is_null())
                .or_else(|| manifest.get("homepage"))
                .and_then(Value::as_str)
        })
    });
    let repo = override_metadata
        .and_then(|metadata| metadata.repo.clone())
        .or_else(|| normalize_repo(repository));
    let Some(license) = license else {
        bail!(
            "gen-third-party-notices: cannot resolve license for {name}; run `pnpm install`, or add an OVERRIDES entry."
        );
    };
    let Some(repo) = repo else {
        bail!(
            "gen-third-party-notices: cannot resolve repository for {name}; run `pnpm install`, or add an OVERRIDES entry."
        );
    };
    Ok(Metadata { license, repo })
}

fn collect_npm(root: &Path) -> anyhow::Result<Vec<ExternalDep>> {
    let (manifests, names) = load_workspace_manifests(root)?;
    let mut dependencies = tier_external_deps(&manifests, &names)?
        .into_iter()
        .filter(|(name, _)| !FIRST_PARTY.contains(&name.as_str()))
        .collect::<Vec<_>>();
    dependencies.sort_by(|a, b| locale_compare(&a.0, &b.0));
    dependencies
        .into_iter()
        .map(|(name, runtime)| {
            let metadata = installed_metadata(root, &name)?;
            Ok(ExternalDep {
                name,
                license: metadata.license,
                repo: metadata.repo,
                runtime,
            })
        })
        .collect()
}

fn collect_vendored(root: &Path) -> anyhow::Result<Vec<VendoredRow>> {
    let rows = parse_vendored_rows(&fs::read_to_string(root.join("vendor/README.md"))?);
    let mut on_disk = IndexMap::new();
    for entry in sorted_entries(&root.join("vendor"))? {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest = read_manifest(&entry.path().join("package.json"))?;
        if let Some(name) = manifest.get("name").and_then(Value::as_str) {
            on_disk.insert(name.to_owned(), entry.path());
        }
    }
    let parsed = rows
        .iter()
        .map(|row| row.npm_name.as_str())
        .collect::<HashSet<_>>();
    let missing = on_disk
        .keys()
        .filter(|name| !parsed.contains(name.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "gen-third-party-notices: vendor/README.md has no manifest-table row for {}; its table format changed or the sync is incomplete.",
            missing.join(", ")
        );
    }
    for row in &rows {
        let directory = on_disk.get(&row.npm_name).with_context(|| format!("gen-third-party-notices: vendored package {} from vendor/README.md has no vendor/ directory.", row.npm_name))?;
        let manifest = read_manifest(&directory.join("package.json"))?;
        if manifest.get("license").and_then(Value::as_str) != Some("MIT") {
            bail!(
                "gen-third-party-notices: vendored {} declares license {}; the vendored section assumes MIT throughout.",
                row.npm_name,
                printed(manifest.get("license"))
            );
        }
    }
    Ok(rows)
}

fn collect_claude(root: &Path) -> anyhow::Result<ClaudeDistribution> {
    let manifest = installed_manifest(root, CLAUDE_AGENT_SDK_PACKAGE)?.with_context(|| format!("gen-third-party-notices: cannot resolve {CLAUDE_AGENT_SDK_PACKAGE}; run `pnpm install`."))?;
    let distribution = claude_distribution_from_manifest(&manifest)?;
    let mut installed_payloads = 0;
    for payload in &distribution.payloads {
        let Some(installed) = installed_manifest(root, &payload.name)? else {
            continue;
        };
        installed_payloads += 1;
        if installed.get("name").and_then(Value::as_str) != Some(payload.name.as_str())
            || installed.get("version").and_then(Value::as_str) != Some(payload.version.as_str())
            || installed.get("license").and_then(Value::as_str)
                != Some(CLAUDE_PLATFORM_DECLARED_LICENSE)
        {
            bail!(
                "gen-third-party-notices: installed {} does not match its SDK-declared version and {CLAUDE_PLATFORM_DECLARED_LICENSE} license field.",
                payload.name
            );
        }
    }
    if installed_payloads == 0 {
        bail!(
            "gen-third-party-notices: no SDK-declared Claude platform payload is installed; install optional dependencies before regenerating."
        );
    }
    Ok(distribution)
}

fn collect_build_tools(root: &Path) -> anyhow::Result<Vec<BuildTimeTool>> {
    let mut tools = Vec::new();
    for tool in &policy().build_time_tools {
        let source = root.join(&tool.pin_source);
        if source.exists() {
            if !fs::read_to_string(source)?.contains(&tool.name) {
                bail!(
                    "gen-third-party-notices: {} no longer references {}; update BUILD_TIME_TOOLS.",
                    tool.pin_source,
                    tool.name
                );
            }
            tools.push(tool.clone());
        } else {
            // The native executable pipeline compiles the shipped Rust runtime;
            // it does not fetch the Node executable packager from npm.
            let native_source = root.join("crates/python-release/src/executable/pipeline.rs");
            let native = fs::read_to_string(&native_source)
                .with_context(|| format!("read {}", source.display()))?;
            if tool.name != "@yao-pkg/pkg"
                || !native.contains("Command::new(\"cargo\")")
                || native.contains(&tool.name)
            {
                bail!(
                    "gen-third-party-notices: {} no longer references {}; update BUILD_TIME_TOOLS.",
                    tool.pin_source,
                    tool.name
                );
            }
        }
    }
    Ok(tools)
}

/// Discovers all direct declarations, installed metadata and provenance inputs.
///
/// # Errors
/// Returns missing manifests or installation metadata, stale discovery policy,
/// unsupported requirement forms, and invalid vendor or platform payload data.
pub fn collect(root: &Path) -> anyhow::Result<NoticeCollection> {
    let build_time_tools = collect_build_tools(root)?;
    let npm = collect_npm(root)?;
    let vendored = collect_vendored(root)?;
    let python_paths = glob_paths(root, "python/*/pyproject.toml")?;
    if python_paths.is_empty() {
        bail!("gen-third-party-notices: no python/*/pyproject.toml found; the Python tree moved.");
    }
    let pyprojects = python_paths
        .into_iter()
        .map(fs::read_to_string)
        .collect::<Result<Vec<_>, _>>()?;
    let python = collect_python_dependencies(&pyprojects, None)?;
    let workspace: serde_yml::Value =
        serde_yml::from_slice(&fs::read(root.join("pnpm-workspace.yaml"))?)?;
    let mut patched = Vec::new();
    if let Some(value) = workspace
        .get("patchedDependencies")
        .filter(|value| !value.is_null())
    {
        let entries = value
            .as_mapping()
            .context("gen-third-party-notices: patchedDependencies must be an object.")?;
        let entries = entries
            .iter()
            .map(|(spec, patch)| (yaml_string(spec), yaml_string(patch)))
            .collect::<IndexMap<_, _>>();
        for (spec, patch) in js_entries(entries.iter().map(|(spec, patch)| (spec.as_str(), patch)))
        {
            patched.push(PatchedDependency {
                spec: spec.to_owned(),
                patch: patch.clone(),
            });
        }
    }
    let claude_distribution = if npm
        .iter()
        .any(|dependency| dependency.runtime && dependency.name == CLAUDE_AGENT_SDK_PACKAGE)
    {
        Some(collect_claude(root)?)
    } else {
        None
    };
    Ok(NoticeCollection {
        npm,
        vendored,
        python,
        patched,
        build_time_tools,
        claude_distribution,
    })
}

fn yaml_string(value: &serde_yml::Value) -> String {
    match value {
        serde_yml::Value::Null => "null".to_owned(),
        serde_yml::Value::Bool(value) => value.to_string(),
        serde_yml::Value::Number(value) => value.as_f64().map_or_else(
            || value.to_string(),
            |value| ryu_js::Buffer::new().format(value).to_owned(),
        ),
        serde_yml::Value::String(value) => value.clone(),
        serde_yml::Value::Sequence(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    yaml_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        serde_yml::Value::Mapping(_) => "[object Object]".to_owned(),
        serde_yml::Value::Tagged(value) => yaml_string(&value.value),
    }
}
