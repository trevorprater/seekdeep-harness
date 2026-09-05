//! Runtime-wheel build-hook policy, shared with the generated foreign-language binding.

use std::path::Path;

use serde_json::{Value, json};

use crate::{
    RUNTIME_DISTRIBUTION, RuntimePlatforms, load_platforms, python_repr, runtime_suffixes,
};

/// Resolves the source's platform and machine aliases to a manifest tag.
///
/// # Errors
/// Rejects a host absent from the closed runtime platform manifest.
pub fn host_platform_tag(
    platforms: &RuntimePlatforms,
    system: &str,
    machine: &str,
) -> anyhow::Result<String> {
    let machine = machine.to_lowercase();
    let arch = match machine.as_str() {
        "arm64" | "aarch64" => "arm64",
        "x86_64" | "amd64" => "x64",
        other => other,
    };
    let system = system.to_lowercase();
    let key = match system.as_str() {
        "darwin" => format!("macos-{arch}"),
        "linux" => format!("linux-{arch}"),
        other => other.to_owned(),
    };
    platforms
        .get(&key)
        .map(|platform| platform.tag.clone())
        .ok_or_else(|| anyhow::anyhow!("unsupported {RUNTIME_DISTRIBUTION} build platform: {key}"))
}

/// Computes Hatch's native wheel fields after verifying the exact platform payload.
///
/// Editable builds are unchanged. Non-editable sdists are rejected before
/// platform selection, and mixed or non-executable payloads never receive a tag.
///
/// # Errors
/// Rejects malformed manifests, unsupported targets/tags, and incomplete payloads.
pub fn initialize(
    root: &Path,
    version: &str,
    target: &str,
    requested_tag: Option<&str>,
    system: &str,
    machine: &str,
) -> anyhow::Result<Value> {
    let platforms = load_platforms(&root.join("platforms.json"))?;
    initialize_with_platforms(
        root,
        &platforms,
        version,
        target,
        requested_tag,
        system,
        machine,
    )
}

/// Computes Hatch's native wheel fields from an import-time platform snapshot.
///
/// # Errors
/// Rejects unsupported targets/tags and incomplete or invalid native payloads.
pub fn initialize_with_platforms(
    root: &Path,
    platforms: &RuntimePlatforms,
    version: &str,
    target: &str,
    requested_tag: Option<&str>,
    system: &str,
    machine: &str,
) -> anyhow::Result<Value> {
    if version == "editable" {
        return Ok(json!({}));
    }
    anyhow::ensure!(
        target != "sdist",
        "{RUNTIME_DISTRIBUTION} is wheel-only; build and publish platform wheels only."
    );
    let tag = match requested_tag.filter(|tag| !tag.is_empty()) {
        Some(tag) => tag.to_owned(),
        None => host_platform_tag(platforms, system, machine)?,
    };
    let matches = platforms
        .values()
        .filter(|platform| platform.tag == tag)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        matches.len() == 1,
        "unsupported SEEKDEEP_RUNTIME_PLATFORM_TAG {}; expected one of {}",
        python_repr(&json!(tag)),
        platforms
            .values()
            .map(|platform| platform.tag.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let platform = matches[0];
    let expected = runtime_suffixes(&platform.executable)
        .iter()
        .map(|suffix| format!("{}{suffix}", platform.executable))
        .collect::<Vec<_>>();
    let runtime = root.join("src/deepseek_harness_runtime/runtime");
    let mut found = Vec::new();
    if runtime.is_dir() {
        for entry in std::fs::read_dir(&runtime)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("seekdeep-jsonrpc-agent-pkg-")
            {
                found.push(entry.path());
            }
        }
    }
    found.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    let names = found
        .iter()
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        names == expected,
        "runtime wheel {tag} payload must be {}; found {}",
        python_repr(&json!(expected)),
        python_repr(&json!(names))
    );
    for executable in found {
        anyhow::ensure!(
            is_executable(&executable)?,
            "runtime executable is not executable: {}",
            executable.display()
        );
    }
    let binding_name = crate::runtime_binding_name(&platform.executable)?;
    let mut bindings = std::fs::read_dir(&runtime)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    bindings.retain(|name| name.starts_with("seekdeep-python-sdk-ffi-"));
    bindings.sort();
    anyhow::ensure!(
        bindings == [binding_name.clone()],
        "runtime wheel {tag} binding payload must be {}; found {}",
        python_repr(&json!([binding_name])),
        python_repr(&json!(bindings))
    );
    crate::executable::validate_native_library(
        &runtime.join(binding_name),
        &crate::runtime_binding_target(&platform.executable)?,
    )?;
    Ok(json!({"pure_python":false,"infer_tag":false,"tag":format!("py3-none-{tag}")}))
}

fn is_executable(path: &Path) -> std::io::Result<bool> {
    let metadata = std::fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(metadata.permissions().mode() & 0o100 != 0)
    }
    #[cfg(not(unix))]
    {
        Ok(metadata.is_file())
    }
}
