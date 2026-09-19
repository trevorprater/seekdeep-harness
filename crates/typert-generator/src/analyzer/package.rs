//! Source package exports, face ownership, and module provenance.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, TypertGeneratorError, text::locale_compare};

/// Package identity and exact public import subpath.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleIdentity {
    /// Scoped or unscoped package name.
    pub package: String,
    /// Dot for the root export, otherwise a dot-slash subpath.
    pub subpath: String,
}

/// Selects one target per public subpath using the source's condition priority.
///
/// Arrays retain first-resolvable order. Conditions prefer `types`, `import`,
/// then `default`, before inspecting remaining values in authored order.
pub fn package_export_targets(manifest: &Value) -> Vec<(String, String)> {
    let exports = &manifest["exports"];
    if let Some(target) = exports.as_str() {
        return vec![(".".to_owned(), target.to_owned())];
    }
    if !exports.is_object() && !exports.is_array() {
        return manifest["types"]
            .as_str()
            .map_or_else(Vec::new, |target| vec![(".".to_owned(), target.to_owned())]);
    }
    let subpaths = exports
        .as_object()
        .filter(|values| values.keys().any(|key| key.starts_with('.')));
    let Some(subpaths) = subpaths else {
        return export_target(exports)
            .map_or_else(Vec::new, |target| vec![(".".to_owned(), target.to_owned())]);
    };
    let mut targets = subpaths
        .iter()
        .filter(|(subpath, _)| subpath.starts_with('.'))
        .filter_map(|(subpath, value)| {
            export_target(value).map(|target| (subpath.clone(), target.to_owned()))
        })
        .collect::<Vec<_>>();
    targets.sort_by(|(left, _), (right, _)| locale_compare(left, right));
    targets
}

fn export_target(value: &Value) -> Option<&str> {
    match value {
        Value::String(value) => Some(value),
        Value::Array(values) => values.iter().find_map(export_target),
        Value::Object(values) => ["types", "import", "default"]
            .into_iter()
            .filter_map(|key| values.get(key))
            .find_map(export_target)
            .or_else(|| object_values(values).into_iter().find_map(export_target)),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn object_values(values: &serde_json::Map<String, Value>) -> Vec<&Value> {
    let mut indexed = Vec::new();
    let mut ordinary = Vec::new();
    for (key, value) in values {
        if let Some(index) = key
            .parse::<u32>()
            .ok()
            .filter(|index| *index != u32::MAX && index.to_string() == *key)
        {
            indexed.push((index, value));
        } else {
            ordinary.push(value);
        }
    }
    indexed.sort_by_key(|(index, _)| *index);
    indexed
        .into_iter()
        .map(|(_, value)| value)
        .chain(ordinary)
        .collect()
}

/// Public Host export subpaths, excluding Client and Remote artifacts.
pub fn host_export_subpaths(manifest: &Value) -> Vec<String> {
    package_export_targets(manifest)
        .into_iter()
        .map(|(subpath, _)| subpath)
        .filter(|subpath| {
            subpath != "./client" && !subpath.starts_with("./client/") && subpath != "./remote"
        })
        .collect()
}

/// Public Client export subpaths in the source's locale-sorted order.
pub fn client_export_subpaths(manifest: &Value) -> Vec<String> {
    package_export_targets(manifest)
        .into_iter()
        .map(|(subpath, _)| subpath)
        .filter(|subpath| subpath == "./client" || subpath.starts_with("./client/"))
        .collect()
}

/// Whether product metadata and exports jointly declare a dual-face package.
///
/// The renamed `seekdeep` metadata key is authoritative; the pinned source's
/// `dsh` spelling is honored so the oracle workspace analyzes unchanged.
pub fn is_dual_face_package(manifest: &Value) -> bool {
    let product = if manifest.get("seekdeep").is_some() {
        &manifest["seekdeep"]
    } else {
        &manifest["dsh"]
    };
    let client = &product["client"];
    (client.is_object() || client.is_array()) && !client_export_subpaths(manifest).is_empty()
}

/// Maps a published library artifact back to its authored source path.
///
/// The mapping is lexical and does not require the target to exist. Only the
/// source's explicit library suffixes are rewritten; path case is retained.
///
/// # Errors
/// Returns a current-directory error when resolving a relative package root.
pub fn source_path_for_export(package_root: &Path, target: &str) -> Result<PathBuf> {
    let normalized = target.strip_prefix("./").unwrap_or(target);
    let relative = if let Some(tail) = normalized.strip_prefix("lib/types/") {
        Path::new("src").join(replace_suffix(tail, &[".d.mts", ".d.cts", ".d.ts"]))
    } else if let Some(tail) = normalized.strip_prefix("lib/") {
        Path::new("src").join(replace_suffix(tail, &[".mjs", ".cjs", ".js", ".d.ts"]))
    } else {
        PathBuf::from(normalized)
    };
    let root = if package_root.is_absolute() {
        package_root.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| TypertGeneratorError::Analysis(error.to_string()))?
            .join(package_root)
    };
    Ok(lexical_normalize(&root.join(relative)))
}

fn replace_suffix(value: &str, suffixes: &[&str]) -> String {
    for suffix in suffixes {
        if let Some(base) = value.strip_suffix(suffix) {
            return format!("{base}.ts");
        }
    }
    value.to_owned()
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component);
            }
        }
    }
    normalized
}

/// Resolves a non-relative package specifier without discarding its subpath.
pub fn module_identity(specifier: &str) -> Option<ModuleIdentity> {
    if specifier.starts_with('.') || specifier.starts_with('/') {
        return None;
    }
    let parts = specifier.split('/').collect::<Vec<_>>();
    let package_length = if specifier.starts_with('@') { 2 } else { 1 };
    let package = parts
        .iter()
        .take(package_length)
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    let rest = parts
        .iter()
        .skip(package_length)
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    Some(ModuleIdentity {
        package,
        subpath: if rest.is_empty() {
            ".".to_owned()
        } else {
            format!("./{rest}")
        },
    })
}

/// Identifies the innermost external package, including nested pnpm layouts.
pub fn external_module_identity_for_file(file: &str) -> Option<ModuleIdentity> {
    let normalized = file.replace('\\', "/");
    let (_, tail) = normalized.rsplit_once("/node_modules/")?;
    let parts = tail.split('/').collect::<Vec<_>>();
    let package_length = if parts.first().is_some_and(|part| part.starts_with('@')) {
        2
    } else {
        1
    };
    Some(ModuleIdentity {
        package: parts
            .iter()
            .take(package_length)
            .copied()
            .collect::<Vec<_>>()
            .join("/"),
        subpath: ".".to_owned(),
    })
}

/// Whether a declaration belongs to the compiler's bundled standard library.
pub fn is_standard_library_file(file: &str) -> bool {
    let normalized = file.replace('\\', "/");
    let Some((_, tail)) = normalized.rsplit_once("/typescript/lib/lib.") else {
        return false;
    };
    tail.strip_suffix(".d.ts")
        .is_some_and(|stem| !stem.is_empty() && !stem.contains('/'))
}

/// The protocol-owned Remote endpoint segment grammar, independent of runtime bindings.
pub fn is_remote_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.' | b'-'))
}
