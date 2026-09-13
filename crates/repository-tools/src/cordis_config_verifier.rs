//! Loader metadata, package-resolution, and composition-plane verification.

use std::{
    collections::HashSet,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use indexmap::{IndexMap, IndexSet};
use path_clean::PathClean as _;
use regex::Regex;
use serde_json::{Map, Value};

use crate::{
    cordis_config_files::cordis_config_files, cordis_config_metadata::metadata_expression_errors,
};

const PACKAGE_SCOPE: &str = "@seekdeep-ai/seekdeep-";
const CHOOSER_PACKAGE: &str = "@seekdeep-ai/seekdeep-host-directory-picker-auto";
const CHOOSER_BACKEND_PACKAGES: &[&str] = &[
    "@seekdeep-ai/seekdeep-host-directory-picker-native",
    "@seekdeep-ai/seekdeep-host-directory-picker-browse",
    "@seekdeep-ai/seekdeep-client-ui-directory-picker-browse",
    "@seekdeep-ai/seekdeep-client-ui-directory-picker-native",
];
const GROUP_PACKAGE: &str = "@seekdeep-ai/cordis-plugin-group";
const INCLUDE_PACKAGE: &str = "@seekdeep-ai/cordis-plugin-include";

/// Complete result of one repository-wide Loader configuration inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CordisConfigReport {
    /// Number of discovered Loader YAML files.
    pub files: usize,
    /// Source-compatible diagnostics in deterministic discovery order.
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PluginReference {
    file: String,
    name: String,
}

/// Validates every Loader config plus its owning package and execution planes.
///
/// The pinned source's TypeScript-path check is represented by the equivalent
/// Rust production invariant: every configured local package has Cargo or
/// explicit Rust source ownership, so clean source launch cannot fall through
/// to built JavaScript. Model-authored external JavaScript specifiers remain
/// outside the local-package check.
///
/// # Errors
///
/// Returns traversal, YAML, JSON, manifest, path, or source-read failures.
pub fn inspect_cordis_config(root: &Path) -> anyhow::Result<CordisConfigReport> {
    let files = cordis_config_files(root)?;
    let mut errors = Vec::new();
    let mut references = Vec::new();
    for file in &files {
        let document = load_document(root, file)?;
        let Some(entries) = document.as_array() else {
            errors.push(format!("{file}: root must be a Loader entry array"));
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            validate_entry(
                entry,
                file,
                &format!("[{index}]"),
                &mut errors,
                &mut references,
            );
        }
    }
    errors.extend(validate_example_resolution(root, &references)?);
    errors.extend(validate_app_resolution(root, &references)?);
    errors.extend(validate_rust_source_resolution(root, &references)?);
    errors.extend(validate_preset_plane_separation(root)?);
    errors.extend(validate_client_halves_declared(root)?);
    Ok(CordisConfigReport {
        files: files.len(),
        errors,
    })
}

/// Renders the command's success or aggregate failure output.
#[must_use]
pub fn render_cordis_config_report(report: &CordisConfigReport) -> String {
    if report.errors.is_empty() {
        return format!(
            "verify-cordis-config: {} config files passed.\n",
            report.files
        );
    }
    let mut output =
        "verify-cordis-config: invalid Loader metadata or plugin package resolution:\n".to_owned();
    for error in &report.errors {
        let _ = writeln!(output, "- {error}");
    }
    output
}

fn validate_entry(
    value: &Value,
    file: &str,
    path: &str,
    errors: &mut Vec<String>,
    references: &mut Vec<PluginReference>,
) {
    let Some(entry) = value.as_object() else {
        errors.push(format!("{file}{path}: entry must be an object"));
        return;
    };
    record_plugin(entry, file, references);
    validate_metadata(entry, file, path, errors);
    let group = entry.get("group").and_then(Value::as_bool) == Some(true)
        || entry.get("name").and_then(Value::as_str) == Some(GROUP_PACKAGE);
    if group && let Some(config) = entry.get("config").and_then(Value::as_array) {
        for (index, child) in config.iter().enumerate() {
            validate_entry(
                child,
                file,
                &format!("{path}.config[{index}]"),
                errors,
                references,
            );
        }
    }
    if let Some(insert) = entry.get("insert").and_then(Value::as_array) {
        for (index, child) in insert.iter().enumerate() {
            validate_entry(
                child,
                file,
                &format!("{path}.insert[{index}]"),
                errors,
                references,
            );
        }
    }
    if entry.get("name").and_then(Value::as_str) != Some(INCLUDE_PACKAGE) {
        return;
    }
    let Some(patches) = entry
        .get("config")
        .and_then(Value::as_object)
        .and_then(|config| config.get("patches"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for (patch_index, patch_value) in patches.iter().enumerate() {
        let patch_path = format!("{path}.config.patches[{patch_index}]");
        let Some(patch_entry) = patch_value.as_object() else {
            continue;
        };
        record_plugin(patch_entry, file, references);
        validate_metadata(patch_entry, file, &patch_path, errors);
        let Some(insert) = patch_entry.get("insert").and_then(Value::as_array) else {
            continue;
        };
        for (insert_index, child) in insert.iter().enumerate() {
            validate_entry(
                child,
                file,
                &format!("{patch_path}.insert[{insert_index}]"),
                errors,
                references,
            );
        }
    }
}

fn validate_metadata(entry: &Map<String, Value>, file: &str, path: &str, errors: &mut Vec<String>) {
    errors.extend(
        metadata_expression_errors(entry, path)
            .into_iter()
            .map(|problem| format!("{file}{problem}")),
    );
}

fn record_plugin(entry: &Map<String, Value>, file: &str, references: &mut Vec<PluginReference>) {
    if let Some(name) = entry.get("name").and_then(Value::as_str) {
        references.push(PluginReference {
            file: file.to_owned(),
            name: name.to_owned(),
        });
    }
}

fn validate_example_resolution(
    root: &Path,
    references: &[PluginReference],
) -> anyhow::Result<Vec<String>> {
    let manifest_path = "examples/package.json";
    let manifest = read_manifest(root, manifest_path)?;
    let dependencies = dependencies(&manifest);
    let overlays = app_overlay_files(root)?;
    let example_references = references
        .iter()
        .filter(|reference| {
            reference.file.starts_with("examples/") && !overlays.contains(&reference.file)
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut violations =
        missing_plugin_dependencies(&example_references, &dependencies, manifest_path);
    let local_packages = local_package_directories(root)?;
    let rust_owned = rust_owned_package_names(root)?;
    let mut required = dependencies.keys().cloned().collect::<IndexSet<_>>();
    required.extend(
        example_references
            .iter()
            .filter_map(|reference| package_name_from_specifier(&reference.name)),
    );
    for package in required {
        let Some(directory) = local_packages.get(&package) else {
            continue;
        };
        if rust_owned.contains(&package) {
            continue;
        }
        violations.push(format!(
            "Cargo.toml: missing Rust source ownership for {package} ({})",
            repo_path(root, directory)
        ));
    }
    Ok(violations)
}

fn validate_app_resolution(
    root: &Path,
    references: &[PluginReference],
) -> anyhow::Result<Vec<String>> {
    let mut app_dependencies = dependencies(&read_manifest(root, "apps/cli/package.json")?);
    let bundle_manifests = bundle_manifest_paths(root)?;
    for path in &bundle_manifests {
        for (name, version) in dependencies(&read_manifest(root, path)?) {
            app_dependencies.insert(name, version);
        }
    }
    let shipped = direct_files(root, "apps/cli/config", |name| {
        name.ends_with(".cordis.yml")
    })?
    .into_iter()
    .collect::<HashSet<_>>();
    let overlays = app_overlay_files(root)?;
    let app_references = references
        .iter()
        .filter(|reference| shipped.contains(&reference.file) || overlays.contains(&reference.file))
        .cloned()
        .collect::<Vec<_>>();
    let mut violations = missing_plugin_dependencies(
        &app_references,
        &app_dependencies,
        "apps/cli/package.json or a bundle manifest",
    );
    for manifest_path in bundle_manifests {
        let bundle_directory = manifest_path
            .strip_suffix("/package.json")
            .unwrap_or(&manifest_path);
        let manifest = read_manifest(root, &manifest_path)?;
        let package_name = manifest.get("name").and_then(Value::as_str);
        let bundle_references = references
            .iter()
            .filter(|reference| reference.file.starts_with(&format!("{bundle_directory}/")))
            .filter(|reference| {
                package_name_from_specifier(&reference.name).as_deref() != package_name
            })
            .cloned()
            .collect::<Vec<_>>();
        violations.extend(missing_plugin_dependencies(
            &bundle_references,
            &dependencies(&manifest),
            &manifest_path,
        ));
    }
    Ok(violations)
}

fn validate_rust_source_resolution(
    root: &Path,
    references: &[PluginReference],
) -> anyhow::Result<Vec<String>> {
    let local_packages = local_package_directories(root)?;
    let rust_owned = rust_owned_package_names(root)?;
    let mut locations = IndexMap::<String, IndexSet<String>>::new();
    for reference in references {
        let Some(package) = package_name_from_specifier(&reference.name) else {
            continue;
        };
        let local = package.starts_with(PACKAGE_SCOPE)
            || local_packages.contains_key(&package)
            || package.starts_with("@seekdeep-ai/cordis");
        if !local {
            continue;
        }
        locations
            .entry(package)
            .or_default()
            .insert(reference.file.clone());
    }
    let mut violations = Vec::new();
    for (package, files) in locations {
        if rust_owned.contains(&package) {
            continue;
        }
        violations.push(format!(
            "{}: {package} does not resolve to Rust source ownership through Cargo packages or explicit Loader built-ins (add a compiled owner so source launch does not depend on built JavaScript)",
            files.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(violations)
}

fn validate_preset_plane_separation(root: &Path) -> anyhow::Result<Vec<String>> {
    let host_file = "packages/bundle/base/cordis.patch.yml";
    let overlay_file = "packages/bundle/web-app/cordis.patch.yml";
    let host_rows = row_ids(root, host_file)?;
    let overlay = load_entries(root, overlay_file)?;
    let disabled = overlay
        .iter()
        .filter_map(|entry| {
            let entry = entry.as_object()?;
            (entry.get("disabled").and_then(Value::as_bool) == Some(true))
                .then(|| entry.get("id").and_then(Value::as_str).map(str::to_owned))?
        })
        .collect::<HashSet<_>>();
    let active = host_rows
        .into_iter()
        .chain(row_ids(root, overlay_file)?)
        .filter(|id| !disabled.contains(id))
        .collect::<HashSet<_>>();
    let mut problems = Vec::new();
    for file in files_in_directory(root, "apps/cli/config/agent-presets", |path| {
        path.file_name().and_then(std::ffi::OsStr::to_str) == Some("agent.cordis.yml")
    })? {
        for id in row_ids(root, &file)? {
            if active.contains(&id) {
                problems.push(format!(
                    "{file}: row {id:?} is also active in the host composition; a row belongs to exactly one plane"
                ));
            }
        }
    }
    Ok(problems)
}

fn validate_client_halves_declared(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut problems = Vec::new();
    for manifest_path in child_manifest_paths(root, "packages/client")? {
        let manifest = read_manifest(root, &manifest_path)?;
        let ships_client = manifest
            .get("exports")
            .and_then(Value::as_object)
            .is_some_and(|exports| exports.contains_key("./client"));
        let declares_client = manifest
            .get("seekdeep")
            .and_then(Value::as_object)
            .and_then(|seekdeep| seekdeep.get("client"))
            .is_some();
        if ships_client == declares_client {
            continue;
        }
        problems.push(if ships_client {
            format!(
                "{manifest_path}: exports \"./client\" but declares no seekdeep.client, so its browser half is never served"
            )
        } else {
            format!(
                "{manifest_path}: declares seekdeep.client but exports no \"./client\" entry to serve"
            )
        });
    }
    Ok(problems)
}

fn missing_plugin_dependencies(
    references: &[PluginReference],
    dependencies: &IndexMap<String, String>,
    manifest_path: &str,
) -> Vec<String> {
    let mut required = IndexMap::<String, IndexSet<String>>::new();
    for reference in references {
        let Some(package) = package_name_from_specifier(&reference.name) else {
            continue;
        };
        required
            .entry(package.clone())
            .or_default()
            .insert(reference.file.clone());
        if package == CHOOSER_PACKAGE {
            for backend in CHOOSER_BACKEND_PACKAGES {
                required
                    .entry((*backend).to_owned())
                    .or_default()
                    .insert(reference.file.clone());
            }
        }
    }
    required
        .into_iter()
        .filter(|(package, _)| !dependencies.contains_key(package))
        .map(|(package, files)| {
            format!(
                "{}: {package} must be declared in {manifest_path} dependencies",
                files.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect()
}

fn package_name_from_specifier(specifier: &str) -> Option<String> {
    static URL_SCHEME: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)^[a-z][a-z+.-]*:").expect("static URL-scheme regex")
    });
    if specifier.starts_with('.') || specifier.starts_with('/') || URL_SCHEME.is_match(specifier) {
        return None;
    }
    let segments = specifier.split('/').collect::<Vec<_>>();
    if specifier.starts_with('@') {
        return (segments.len() >= 2).then(|| format!("{}/{}", segments[0], segments[1]));
    }
    segments
        .first()
        .filter(|segment| !segment.is_empty())
        .map(|value| (*value).to_owned())
}

fn row_ids(root: &Path, file: &str) -> anyhow::Result<IndexSet<String>> {
    fn walk(value: &Value, output: &mut IndexSet<String>) {
        match value {
            Value::Array(values) => {
                for value in values {
                    walk(value, output);
                }
            }
            Value::Object(values) => {
                if let (Some(id), Some(_)) = (
                    values.get("id").and_then(Value::as_str),
                    values.get("name").and_then(Value::as_str),
                ) {
                    output.insert(id.to_owned());
                }
                for value in values.values() {
                    walk(value, output);
                }
            }
            _ => {}
        }
    }
    let mut ids = IndexSet::new();
    walk(&Value::Array(load_entries(root, file)?), &mut ids);
    Ok(ids)
}

fn load_entries(root: &Path, file: &str) -> anyhow::Result<Vec<Value>> {
    Ok(load_document(root, file)?
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn load_document(root: &Path, file: &str) -> anyhow::Result<Value> {
    let source = std::fs::read_to_string(root.join(file))?;
    let normalized = normalize_js_tags(&source);
    let value = serde_yml::from_str::<serde_yml::Value>(&normalized)
        .map_err(|error| anyhow::anyhow!("{file}: {error}"))?;
    yaml_to_json(value).map_err(|error| anyhow::anyhow!("{file}: {error}"))
}

fn normalize_js_tags(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut block_parent_indent = None::<usize>;
    for segment in source.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        let content = line.trim();
        if let Some(parent_indent) = block_parent_indent {
            if content.is_empty() || indent > parent_indent {
                output.push_str(segment);
                continue;
            }
            block_parent_indent = None;
        }
        let normalized = normalize_js_tags_on_line(line);
        if starts_block_scalar(&normalized) {
            block_parent_indent = Some(indent);
        }
        output.push_str(&normalized);
        if segment.ends_with('\n') {
            output.push('\n');
        }
    }
    output
}

fn normalize_js_tags_on_line(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut index = 0;
    let mut single_quoted = false;
    let mut double_quoted = false;
    let bytes = line.as_bytes();
    while index < bytes.len() {
        let byte = bytes[index];
        if single_quoted {
            if !byte.is_ascii() {
                let Some(character) = line[index..].chars().next() else {
                    break;
                };
                output.push(character);
                index += character.len_utf8();
                continue;
            }
            output.push(char::from(byte));
            if byte == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    output.push('\'');
                    index += 2;
                    continue;
                }
                single_quoted = false;
            }
            index += 1;
            continue;
        }
        if double_quoted {
            if !byte.is_ascii() {
                let Some(character) = line[index..].chars().next() else {
                    break;
                };
                output.push(character);
                index += character.len_utf8();
                continue;
            }
            output.push(char::from(byte));
            if byte == b'\\' {
                if let Some(next) = bytes.get(index + 1) {
                    output.push(char::from(*next));
                    index += 2;
                    continue;
                }
            } else if byte == b'"' {
                double_quoted = false;
            }
            index += 1;
            continue;
        }
        if byte == b'#' && (index == 0 || bytes[index - 1].is_ascii_whitespace()) {
            output.push_str(&line[index..]);
            break;
        }
        if byte == b'\'' {
            single_quoted = true;
            output.push('\'');
            index += 1;
            continue;
        }
        if byte == b'"' {
            double_quoted = true;
            output.push('"');
            index += 1;
            continue;
        }
        if line[index..].starts_with("!!js")
            && bytes.get(index + 4).is_none_or(u8::is_ascii_whitespace)
            && tag_can_start_after(&line[..index])
        {
            output.push_str("!js");
            index += 4;
            continue;
        }
        let Some(character) = line[index..].chars().next() else {
            break;
        };
        output.push(character);
        index += character.len_utf8();
    }
    output
}

fn tag_can_start_after(prefix: &str) -> bool {
    prefix
        .trim_end()
        .chars()
        .next_back()
        .is_none_or(|character| matches!(character, ':' | '-' | '[' | '{' | ','))
}

fn starts_block_scalar(line: &str) -> bool {
    let syntax = line
        .split_once('#')
        .map_or(line, |(syntax, _)| syntax)
        .trim_end();
    let Some(last) = syntax.split_whitespace().next_back() else {
        return false;
    };
    last.starts_with('|') || last.starts_with('>')
}

fn yaml_to_json(value: serde_yml::Value) -> anyhow::Result<Value> {
    use serde_yml::Value as Yaml;

    match value {
        Yaml::Null => Ok(Value::Null),
        Yaml::Bool(value) => Ok(Value::Bool(value)),
        Yaml::Number(value) => Ok(serde_json::to_value(value)?),
        Yaml::String(value) => Ok(Value::String(value)),
        Yaml::Sequence(values) => values
            .into_iter()
            .map(yaml_to_json)
            .collect::<anyhow::Result<Vec<_>>>()
            .map(Value::Array),
        Yaml::Mapping(values) => {
            let mut output = Map::new();
            for (key, value) in values {
                output.insert(yaml_key(key)?, yaml_to_json(value)?);
            }
            Ok(Value::Object(output))
        }
        Yaml::Tagged(tagged) => {
            if tagged.tag.string != "tag:yaml.org,2002:js"
                && tagged.tag.string.trim_start_matches('!') != "js"
            {
                anyhow::bail!("unsupported YAML tag {}", tagged.tag);
            }
            let Yaml::String(expression) = tagged.value else {
                anyhow::bail!("!!js requires a scalar string");
            };
            Ok(serde_json::json!({ "__jsExpr": expression }))
        }
    }
}

fn yaml_key(value: serde_yml::Value) -> anyhow::Result<String> {
    match value {
        serde_yml::Value::String(value) => Ok(value),
        serde_yml::Value::Bool(value) => Ok(value.to_string()),
        serde_yml::Value::Number(value) => Ok(value.to_string()),
        serde_yml::Value::Null => Ok("null".to_owned()),
        _ => anyhow::bail!("Loader YAML mapping keys must be scalars"),
    }
}

fn dependencies(manifest: &Value) -> IndexMap<String, String> {
    manifest
        .get("dependencies")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| value.as_str().map(|value| (name.clone(), value.to_owned())))
        .collect()
}

fn read_manifest(root: &Path, path: &str) -> anyhow::Result<Value> {
    serde_json::from_str(&std::fs::read_to_string(root.join(path))?).map_err(anyhow::Error::from)
}

fn local_package_directories(root: &Path) -> anyhow::Result<IndexMap<String, PathBuf>> {
    let mut paths = package_manifest_paths(root)?;
    paths.extend(vendor_manifest_paths(root)?);
    utf16_sort(&mut paths);
    let mut packages = IndexMap::new();
    for path in paths {
        let manifest = read_manifest(root, &path)?;
        if let Some(name) = manifest.get("name").and_then(Value::as_str) {
            let directory = root.join(path.strip_suffix("/package.json").unwrap_or(path.as_str()));
            packages.insert(name.to_owned(), directory.clean());
        }
    }
    Ok(packages)
}

fn rust_owned_package_names(root: &Path) -> anyhow::Result<HashSet<String>> {
    static PACKAGE_IDENTITY: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r"@seekdeep-ai/(?:seekdeep-[A-Za-z0-9._-]+|cordis(?:-plugin)?-[A-Za-z0-9._-]+|cordis)",
        )
        .expect("static package-identity regex")
    });
    static CARGO_NAME: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?m)^name\s*=\s*"(seekdeep-[A-Za-z0-9._-]+)"\s*$"#)
            .expect("static Cargo package-name regex")
    });
    let mut owned = HashSet::new();
    let mut cargo_names = HashSet::new();
    for manifest in cargo_manifest_paths(root)? {
        let source = std::fs::read_to_string(root.join(&manifest))?;
        if let Some(name) = CARGO_NAME
            .captures(&source)
            .and_then(|captures| captures.get(1))
            .map(|capture| capture.as_str())
        {
            cargo_names.insert(name.to_owned());
            owned.insert(format!("@seekdeep-ai/{name}"));
        }
    }
    for (package, cargo) in [
        ("@seekdeep-ai/cordis", "seekdeep-cordis"),
        (GROUP_PACKAGE, "seekdeep-loader"),
        (INCLUDE_PACKAGE, "seekdeep-loader"),
        ("@seekdeep-ai/cordis-plugin-timer", "seekdeep-cordis-timer"),
        ("@seekdeep-ai/cordis-plugin-hmr", "seekdeep-hmr"),
        (
            "@seekdeep-ai/cordis-plugin-logger-console",
            "seekdeep-logger-console",
        ),
        (
            "@seekdeep-ai/seekdeep-tool-call-timeout-policy",
            "seekdeep-tool-timeout-policy",
        ),
    ] {
        if cargo_names.contains(cargo) {
            owned.insert(package.to_owned());
        }
    }
    for path in rust_source_files(root)? {
        let source = std::fs::read_to_string(root.join(path))?;
        owned.extend(
            PACKAGE_IDENTITY
                .find_iter(&source)
                .map(|capture| capture.as_str().to_owned()),
        );
    }
    Ok(owned)
}

fn app_overlay_files(root: &Path) -> anyhow::Result<HashSet<String>> {
    let mut files = HashSet::from([
        "examples/web-cordis/cordis.yml".to_owned(),
        "examples/web-schedule/cordis.yml".to_owned(),
    ]);
    files.extend(direct_files(root, "examples/mcp-memory", |name| {
        name.ends_with(".cordis.yml")
    })?);
    Ok(files)
}

fn package_manifest_paths(root: &Path) -> anyhow::Result<Vec<String>> {
    two_level_manifest_paths(root, "packages")
}

fn bundle_manifest_paths(root: &Path) -> anyhow::Result<Vec<String>> {
    child_manifest_paths(root, "packages/bundle")
}

fn vendor_manifest_paths(root: &Path) -> anyhow::Result<Vec<String>> {
    child_manifest_paths(root, "vendor")
}

fn cargo_manifest_paths(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut paths = child_manifest_paths_named(root, "crates", "Cargo.toml")?;
    paths.extend(child_manifest_paths_named(root, "apps", "Cargo.toml")?);
    utf16_sort(&mut paths);
    Ok(paths)
}

fn rust_source_files(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    for parent in ["crates", "apps"] {
        let absolute = root.join(parent);
        if !absolute.is_dir() {
            continue;
        }
        for package in std::fs::read_dir(absolute)? {
            let package = package?;
            if !package.file_type()?.is_dir() {
                continue;
            }
            let source = package.path().join("src");
            if !source.is_dir() {
                continue;
            }
            for entry in walkdir::WalkDir::new(source) {
                let entry = entry?;
                if entry.file_type().is_file()
                    && entry.path().extension().and_then(std::ffi::OsStr::to_str) == Some("rs")
                {
                    files.push(repo_path(root, entry.path()));
                }
            }
        }
    }
    utf16_sort(&mut files);
    Ok(files)
}

fn two_level_manifest_paths(root: &Path, directory: &str) -> anyhow::Result<Vec<String>> {
    let mut paths = Vec::new();
    let absolute = root.join(directory);
    if !absolute.is_dir() {
        return Ok(paths);
    }
    for group in child_directories(&absolute)? {
        for package in child_directories(&group)? {
            if package.join("package.json").is_file() {
                paths.push(repo_path(root, &package.join("package.json")));
            }
        }
    }
    utf16_sort(&mut paths);
    Ok(paths)
}

fn child_manifest_paths(root: &Path, directory: &str) -> anyhow::Result<Vec<String>> {
    child_manifest_paths_named(root, directory, "package.json")
}

fn child_manifest_paths_named(
    root: &Path,
    directory: &str,
    manifest: &str,
) -> anyhow::Result<Vec<String>> {
    let absolute = root.join(directory);
    if !absolute.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = child_directories(&absolute)?
        .into_iter()
        .map(|child| child.join(manifest))
        .filter(|path| path.is_file())
        .map(|path| repo_path(root, &path))
        .collect::<Vec<_>>();
    utf16_sort(&mut paths);
    Ok(paths)
}

fn child_directories(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            directories.push(entry.path());
        }
    }
    directories.sort_by(|left, right| {
        left.to_string_lossy()
            .encode_utf16()
            .cmp(right.to_string_lossy().encode_utf16())
    });
    Ok(directories)
}

fn direct_files(
    root: &Path,
    directory: &str,
    keep: impl Fn(&str) -> bool,
) -> anyhow::Result<Vec<String>> {
    let absolute = root.join(directory);
    if !absolute.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(absolute)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_file() && keep(&name) {
            files.push(format!("{directory}/{name}"));
        }
    }
    utf16_sort(&mut files);
    Ok(files)
}

fn files_in_directory(
    root: &Path,
    directory: &str,
    keep: impl Fn(&Path) -> bool,
) -> anyhow::Result<Vec<String>> {
    let absolute = root.join(directory);
    if !absolute.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(absolute) {
        let entry = entry?;
        if entry.file_type().is_file() && keep(entry.path()) {
            files.push(repo_path(root, entry.path()));
        }
    }
    utf16_sort(&mut files);
    Ok(files)
}

fn repo_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn utf16_sort(values: &mut [String]) {
    values.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
}
