//! Post-build verification of metadata, native payload names, and executable bits.

use std::{fs::File, io::Read as _, path::Path};

use indexmap::IndexMap;
use serde_json::{Value, json};

use crate::{Package, RUNTIME_DISTRIBUTION, RuntimePlatform, python_repr, runtime_suffixes};

/// Verifies the complete source release contract on a built wheel archive.
///
/// # Errors
/// Rejects unreadable archives, missing metadata, incorrect tags/names/versions/licenses,
/// wrong runtime payloads, lost executable bits, and a missing exact SDK runtime pin.
pub fn verify_wheel(
    path: &Path,
    package: Package,
    version: &str,
    platform: Option<&RuntimePlatform>,
) -> anyhow::Result<()> {
    let tag = platform.map_or_else(
        || "py3-none-any".to_owned(),
        |platform| format!("py3-none-{}", platform.tag),
    );
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    let names = archive.file_names().map(str::to_owned).collect::<Vec<_>>();
    let wheel_path = names
        .iter()
        .find(|name| name.ends_with(".dist-info/WHEEL"))
        .ok_or_else(|| anyhow::anyhow!("{} has no WHEEL metadata", path.display()))?;
    let metadata_path = names
        .iter()
        .find(|name| name.ends_with(".dist-info/METADATA"))
        .ok_or_else(|| anyhow::anyhow!("{} has no package METADATA", path.display()))?;
    let wheel = read_headers(&mut archive, wheel_path)?;
    let metadata = read_headers(&mut archive, metadata_path)?;
    anyhow::ensure!(
        wheel.get("tag") == Some(&vec![tag]),
        "{} has wrong WHEEL tags: {}",
        path.display(),
        optional_list(wheel.get("tag"))
    );
    verify_identity_headers(path, package, version, &metadata)?;
    let runtime_files = names
        .iter()
        .filter(|name| name.contains("/runtime/seekdeep-jsonrpc-agent-pkg-"))
        .collect::<Vec<_>>();
    let binding_files = names
        .iter()
        .filter(|name| name.contains("/runtime/seekdeep-python-sdk-ffi-"))
        .collect::<Vec<_>>();
    match package {
        Package::Runtime => {
            let platform = platform
                .ok_or_else(|| anyhow::anyhow!("runtime wheel verification requires a platform"))?;
            let expected_binding = format!(
                "deepseek_harness_runtime/runtime/{}",
                crate::runtime_binding_name(&platform.executable)?
            );
            anyhow::ensure!(
                binding_files == [&expected_binding],
                "{} native binding payload must be {}; found {}",
                path.display(),
                expected_binding,
                python_repr(&json!(binding_files))
            );
            let expected = runtime_suffixes(&platform.executable)
                .iter()
                .map(|suffix| format!("{}{suffix}", platform.executable))
                .collect::<Vec<_>>();
            let mut found = runtime_files
                .iter()
                .map(|name| {
                    Path::new(name)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>();
            found.sort();
            anyhow::ensure!(
                found == expected,
                "{} runtime payload must be {}, found {}",
                path.display(),
                python_repr(&json!(expected)),
                python_repr(&json!(found))
            );
            for name in runtime_files {
                let entry = archive.by_name(name)?;
                anyhow::ensure!(
                    entry.unix_mode().unwrap_or_default() & 0o100 != 0,
                    "{} runtime executable lost its executable bit: {name}",
                    path.display()
                );
            }
        }
        Package::Sdk => {
            anyhow::ensure!(
                binding_files.is_empty(),
                "SDK wheel unexpectedly contains native binding libraries"
            );
            anyhow::ensure!(
                runtime_files.is_empty(),
                "SDK wheel unexpectedly contains runtime executables: {}",
                python_repr(&json!(runtime_files))
            );
            let requirement = format!("{RUNTIME_DISTRIBUTION}=={version}");
            let requirements = metadata.get("requires-dist").cloned().unwrap_or_default();
            anyhow::ensure!(
                requirements.contains(&requirement),
                "{} does not pin {requirement}; found {}",
                path.display(),
                python_repr(&json!(requirements))
            );
        }
    }
    Ok(())
}

type Headers = IndexMap<String, Vec<String>>;

fn verify_identity_headers(
    path: &Path,
    package: Package,
    version: &str,
    metadata: &Headers,
) -> anyhow::Result<()> {
    for (field, expected, label) in [
        ("version", version, "version"),
        ("name", package.distribution(), "distribution name"),
        ("license-expression", "MIT", "license expression"),
    ] {
        require_header(path, metadata, field, expected, label)?;
    }
    verify_licenses(path, package, metadata)
}

fn verify_licenses(path: &Path, package: Package, metadata: &Headers) -> anyhow::Result<()> {
    let expected = match package {
        Package::Sdk => vec!["LICENSE"],
        Package::Runtime => vec!["LICENSE", "THIRD_PARTY_NOTICES.md"],
    };
    let licenses = metadata
        .get("license-file")
        .into_iter()
        .flatten()
        .map(|name| {
            Path::new(name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        licenses == expected,
        "{} has license files {}, expected {}",
        path.display(),
        python_repr(&json!(licenses)),
        python_repr(&json!(expected))
    );
    Ok(())
}

fn read_headers(archive: &mut zip::ZipArchive<File>, name: &str) -> anyhow::Result<Headers> {
    let mut content = String::new();
    archive.by_name(name)?.read_to_string(&mut content)?;
    Ok(parse_headers(&content))
}

fn parse_headers(content: &str) -> Headers {
    let mut headers = Headers::new();
    let mut current = None::<String>;
    for line in content.lines() {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some(name) = &current
                && let Some(value) = headers.get_mut(name).and_then(|values| values.last_mut())
            {
                value.push('\n');
                value.push_str(line);
            }
        } else if let Some((name, value)) = line.split_once(':') {
            let name = name.to_ascii_lowercase();
            headers
                .entry(name.clone())
                .or_default()
                .push(value.trim_start_matches([' ', '\t']).to_owned());
            current = Some(name);
        } else {
            break;
        }
    }
    headers
}

fn require_header(
    path: &Path,
    headers: &Headers,
    key: &str,
    expected: &str,
    label: &str,
) -> anyhow::Result<()> {
    let actual = headers
        .get(key)
        .and_then(|values| values.first())
        .map(String::as_str);
    anyhow::ensure!(
        actual == Some(expected),
        "{} has {label} {}, expected {expected}",
        path.display(),
        actual.unwrap_or("None")
    );
    Ok(())
}

fn optional_list(value: Option<&Vec<String>>) -> String {
    python_repr(&value.map_or(Value::Null, |value| json!(value)))
}
