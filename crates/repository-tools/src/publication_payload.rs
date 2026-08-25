//! Publication payload policy shared by static manifests and packed tarballs.

use serde_json::Value;

/// Whether a package manifest exports the canonical generated Host-for-Client pair.
#[must_use]
pub fn has_typert_remote_navigation(manifest: &Value) -> bool {
    manifest
        .as_object()
        .and_then(|manifest| manifest.get("exports"))
        .and_then(Value::as_object)
        .and_then(|exports| exports.get("./remote"))
        .and_then(Value::as_object)
        .is_some_and(|remote| {
            remote.get("types").and_then(Value::as_str) == Some("./lib/typert.remote-client.d.ts")
                && remote.get("default").and_then(Value::as_str)
                    == Some("./lib/typert.remote-client.js")
        })
}

fn payload_path(file: &str) -> String {
    let mut normalized = file.replace('\\', "/");
    if let Some(after_dot) = normalized.strip_prefix('.') {
        let slash_count = after_dot.bytes().take_while(|byte| *byte == b'/').count();
        if slash_count > 0 {
            normalized = after_dot[slash_count..].to_owned();
        }
    }
    normalized.truncate(normalized.trim_end_matches('/').len());
    normalized
        .strip_prefix("package/")
        .unwrap_or(&normalized)
        .to_owned()
}

/// Whether a package payload path exposes source or map intermediates.
#[must_use]
pub fn is_forbidden_publication_file(file: &str) -> bool {
    let normalized = payload_path(file);
    normalized == "src"
        || normalized.starts_with("src/")
        || normalized.ends_with(".d.ts.map")
        || normalized.ends_with(".js.map")
}

/// Rejects source and map members in a packed npm tarball.
///
/// # Errors
///
/// Returns the first forbidden source or source-map member with `context`.
pub fn validate_tarball_payload(files: &[String], context: &str) -> anyhow::Result<()> {
    for file in files {
        if !is_forbidden_publication_file(file) {
            continue;
        }
        let normalized = payload_path(file);
        if normalized == "src" || normalized.starts_with("src/") {
            anyhow::bail!("{context} publishes source file {file}");
        }
        anyhow::bail!("{context} publishes source map {file}");
    }
    Ok(())
}
