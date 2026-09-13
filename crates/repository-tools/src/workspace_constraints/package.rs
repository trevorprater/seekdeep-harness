use serde_json::Value;

use crate::publication_payload::{has_typert_remote_navigation, is_forbidden_publication_file};

const REPOSITORY: &str = "git+https://github.com/deepseek-ai/seekdeep-harness.git";
const LANDLOCK_REPOSITORY: &str = "git+https://github.com/seekdeep-harness/seekdeep-harness.git";
const CORDIS: &str = "@seekdeep-ai/cordis";

pub(super) fn check(
    directory: &str,
    manifest: &Value,
    version: &Value,
    landlock_version: &Value,
) -> anyhow::Result<Vec<String>> {
    anyhow::ensure!(
        manifest["name"].is_null() || manifest["name"].is_string(),
        "package name must be a string: {directory}"
    );
    let name = manifest["name"].as_str();
    let label = name.unwrap_or(directory);
    let landlock = directory.starts_with("native/landlock-run/packages/");
    let public_landlock = landlock
        && matches!(
            name,
            Some(
                "@seekdeep-ai/node-addon-landlock-run"
                    | "@seekdeep-ai/node-addon-landlock-run-linux-arm64"
                    | "@seekdeep-ai/node-addon-landlock-run-linux-x64"
            )
        );
    let mut errors = publication(directory, manifest, public_landlock);
    if name.is_some_and(is_vendor) {
        return Ok(errors);
    }
    if name.is_some_and(|name| name.starts_with("@seekdeep-ai/")) {
        for file in manifest["files"].as_array().into_iter().flatten() {
            let file = file.as_str().ok_or_else(|| {
                anyhow::anyhow!("package publication file must be a string: {directory}")
            })?;
            if is_forbidden_publication_file(file)
                && !(name == Some("@seekdeep-ai/node-addon-landlock-run") && file == "src/main.c")
            {
                errors.push(format!(
                    "{label}: package.json files must not publish {}",
                    json(file)
                ));
            }
        }
    }
    if directory.starts_with("apps/") && name.is_some_and(|name| name.starts_with("@seekdeep-ai/"))
    {
        let files = match name {
            Some("@seekdeep-ai/seekdeep") => Some(vec!["lib/*.js", "config"]),
            Some("@seekdeep-ai/seekdeep-web-frontend") => Some(vec!["dist", "!dist/**/*.map"]),
            _ => None,
        };
        match files {
            None => errors.push(format!(
                "{label}: app package has no publication files policy"
            )),
            Some(files) => check_files(label, manifest, &files, &mut errors),
        }
    }
    if landlock {
        if !public_landlock {
            errors.push(format!(
                "{label}: unexpected package in the public Landlock package family"
            ));
        }
        if manifest["version"] != *landlock_version {
            errors.push(format!(
                "{label}: package.json version must match Landlock workspace version {}",
                version_label(landlock_version)
            ));
        }
    }
    if directory.starts_with("packages/")
        && name.is_some_and(|name| name.starts_with("@seekdeep-ai/seekdeep-"))
    {
        errors.extend(first_party(manifest, label, version));
    }
    Ok(errors)
}

fn publication(directory: &str, manifest: &Value, landlock: bool) -> Vec<String> {
    let label = manifest["name"].as_str().unwrap_or(directory);
    let parts: Vec<_> = directory.split('/').collect();
    let member = matches!(
        parts.as_slice(),
        ["packages", _, _] | ["apps" | "vendor", _]
    );
    let mut errors = Vec::new();
    if landlock || member {
        let kind = if landlock {
            "published Landlock package"
        } else {
            "release member"
        };
        if manifest["private"] == true {
            errors.push(format!("{label}: {kind} must not set \"private\": true"));
        }
        if manifest["publishConfig"]["access"] != "public" {
            errors.push(format!(
                "{label}: {kind} must set publishConfig.access to \"public\""
            ));
        }
        let repository = if landlock {
            LANDLOCK_REPOSITORY
        } else {
            REPOSITORY
        };
        if manifest["repository"]["type"] != "git"
            || manifest["repository"]["url"] != repository
            || manifest["repository"]["directory"] != directory
        {
            let suffix = if landlock {
                " for trusted publishing"
            } else {
                ""
            };
            errors.push(format!("{label}: {kind} repository must use {repository} with directory {directory}{suffix}"));
        }
    } else if manifest["private"] != true {
        errors.push(format!("{label}: package.json must set \"private\": true"));
    }
    errors
}

fn first_party(manifest: &Value, label: &str, version: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let peer = &manifest["peerDependencies"][CORDIS];
    let dev = &manifest["devDependencies"][CORDIS];
    if !truthy(peer) {
        errors.push(format!("{label}: {CORDIS} must be a peerDependency"));
    }
    if !truthy(dev) {
        errors.push(format!("{label}: {CORDIS} must also be a devDependency"));
    }
    if truthy(peer) && truthy(dev) && peer != dev {
        errors.push(format!(
            "{label}: {CORDIS} peer ({}) and dev ({}) ranges must match",
            version_label(peer),
            version_label(dev)
        ));
    }
    if manifest["version"] != *version {
        errors.push(format!(
            "{label}: package.json version must match root version {}",
            version_label(version)
        ));
    }
    for (field, expected) in [
        ("type", "module"),
        ("main", "lib/index.js"),
        ("types", "lib/types/index.d.ts"),
    ] {
        if manifest[field] != expected {
            errors.push(format!(
                "{label}: package.json must set {}: {}",
                json(field),
                json(expected)
            ));
        }
    }
    let root = &manifest["exports"]["."];
    for (field, expected) in [
        ("types", "./lib/types/index.d.ts"),
        ("default", "./lib/index.js"),
    ] {
        if root[field] != expected {
            errors.push(format!(
                "{label}: package.json exports[\".\"].{field} must be {}",
                json(expected)
            ));
        }
    }
    if let Some(invariant) = manifest["exports"]["./invariant"].as_object() {
        for (field, expected) in [
            ("types", "./lib/types/invariant.d.ts"),
            ("default", "./lib/invariant.js"),
        ] {
            if invariant.get(field).is_some_and(|value| value != expected) {
                errors.push(format!(
                    "{label}: package.json exports[\"./invariant\"].{field} must be {}",
                    json(expected)
                ));
            }
        }
        if !invariant.contains_key("types") || !invariant.contains_key("default") {
            errors.push(format!("{label}: package.json exports[\"./invariant\"] must declare both types and default targets"));
        }
    }
    if manifest["exports"]["./invariant"].is_array() {
        errors.push(format!("{label}: package.json exports[\"./invariant\"] must declare both types and default targets"));
    }
    check_files(label, manifest, &expected_files(manifest), &mut errors);
    errors
}

fn expected_files(manifest: &Value) -> Vec<&str> {
    let mut files = vec!["lib/index.js", "lib/invariant.js"];
    if truthy(&manifest["bin"]) {
        files.push("lib/bin.js");
    }
    if truthy(&manifest["exports"]["./worker"]) {
        files.push("lib/worker.cjs");
    }
    for (subpath, path, file) in [
        ("./client", "./lib/client.js", "lib/client.js"),
        ("./loader", "./lib/loader.js", "lib/loader.js"),
        ("./store", "./lib/store/index.js", "lib/store/index.js"),
        ("./startup", "./lib/startup.js", "lib/startup.js"),
    ] {
        if export_default(&manifest["exports"][subpath]) == Some(path) {
            files.push(file);
        }
    }
    files.extend(match manifest["name"].as_str() {
        Some(
            "@seekdeep-ai/seekdeep-base"
            | "@seekdeep-ai/seekdeep-web-app"
            | "@seekdeep-ai/seekdeep-headless",
        ) => vec!["cordis.patch.yml"],
        Some("@seekdeep-ai/seekdeep-client-ui-theme") => vec!["lib/styles"],
        Some("@seekdeep-ai/seekdeep-sdk-jsonrpc-demo") => vec!["lib/packaged-bin.js"],
        Some("@seekdeep-ai/seekdeep-sandbox-windows-acl") => {
            vec!["lib/runner.js", "lib/types-*.js"]
        }
        Some("@seekdeep-ai/seekdeep-skill-badge") => vec!["assets"],
        Some("@seekdeep-ai/seekdeep-subprocess-local") => vec!["scripts/ensure-spawn-helper.mjs"],
        _ => vec![],
    });
    if manifest["exports"].as_object().is_some_and(|exports| {
        exports
            .values()
            .any(|entry| export_default(entry).is_some_and(|path| path.starts_with("./lib/types/")))
    }) {
        files.push("lib/types/**/*.js");
    }
    files.push("lib/types/**/*.d.ts");
    for (subpath, types, runtime, entries) in [
        (
            "./typert",
            "./lib/typert.host.d.ts",
            "./lib/typert.host.js",
            ["lib/typert.host.js", "lib/typert.host.d.ts"],
        ),
        (
            "./client/typert",
            "./lib/typert.client.d.ts",
            "./lib/typert.client.js",
            ["lib/typert.client.js", "lib/typert.client.d.ts"],
        ),
    ] {
        let entry = &manifest["exports"][subpath];
        if entry["types"] == types && entry["default"] == runtime {
            files.extend(entries);
        }
    }
    if has_typert_remote_navigation(manifest) {
        files.extend([
            "lib/typert.remote-client.js",
            "lib/typert.remote-client.d.ts",
        ]);
    }
    files
}

fn export_default(entry: &Value) -> Option<&str> {
    entry
        .as_str()
        .or_else(|| entry.get("default").and_then(Value::as_str))
}

fn check_files(label: &str, manifest: &Value, expected: &[&str], errors: &mut Vec<String>) {
    if manifest["files"] != serde_json::json!(expected) {
        errors.push(format!(
            "{label}: package.json files must be {}",
            serde_json::to_string(expected).expect("string list")
        ));
    }
}

fn is_vendor(name: &str) -> bool {
    matches!(
        name,
        "@seekdeep-ai/cordis"
            | "@seekdeep-ai/cosmokit"
            | "@seekdeep-ai/schemastery"
            | "@seekdeep-ai/cordis-plugin-loader"
            | "@seekdeep-ai/cordis-plugin-include"
            | "@seekdeep-ai/cordis-plugin-group"
            | "@seekdeep-ai/cordis-plugin-timer"
            | "@seekdeep-ai/cordis-plugin-hmr"
            | "@seekdeep-ai/cordis-plugin-logger-console"
    )
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn version_label(value: &Value) -> String {
    if value.is_null() {
        "(missing)".into()
    } else {
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned)
    }
}

fn json(value: &str) -> String {
    serde_json::to_string(value).expect("string")
}
