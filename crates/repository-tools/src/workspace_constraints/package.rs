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
    source_entry: bool,
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
    let first_party_package = directory.starts_with("packages/")
        && name.is_some_and(|name| name.starts_with("@seekdeep-ai/seekdeep-"));
    if compiled(manifest) && !first_party_package {
        errors.push(format!(
            "{label}: seekdeep.compiled applies only to first-party packages under packages/"
        ));
    }
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
    if first_party_package {
        errors.extend(first_party(manifest, label, version, source_entry));
    }
    Ok(errors)
}

/// Whether the manifest marks a package whose implementation is a compiled Rust crate.
///
/// The source's rule assumes every package is a TypeScript library with a `lib/index.js` entry
/// point. The port replaces most packages with crates and keeps their manifests for workspace
/// resolution and type declarations only, so such a package publishes declarations and must not
/// promise runtime entry files that no build emits; `publint` verifies the built package.
fn compiled(manifest: &Value) -> bool {
    manifest["seekdeep"]["compiled"] == true
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

fn first_party(manifest: &Value, label: &str, version: &Value, source_entry: bool) -> Vec<String> {
    let mut errors = Vec::new();
    let compiled = compiled(manifest);
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
    if manifest["type"] != "module" {
        errors.push(format!(
            "{label}: package.json must set \"type\": \"module\""
        ));
    }
    if compiled {
        errors.extend(compiled_entry_points(manifest, label, source_entry));
    } else {
        for (field, expected) in [("main", "lib/index.js"), ("types", "lib/types/index.d.ts")] {
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
    }
    if compiled {
        // The companion declaration is generated for every package; the runtime companion is
        // the Rust crate's, so the export names the declaration alone.
        let invariant = &manifest["exports"]["./invariant"];
        if !invariant.is_null()
            && *invariant != serde_json::json!({"types": "./lib/types/invariant.d.ts"})
        {
            errors.push(format!(
                "{label}: package.json exports[\"./invariant\"] must be exactly {{\"types\": \"./lib/types/invariant.d.ts\"}} for a compiled package"
            ));
        }
    } else if let Some(invariant) = manifest["exports"]["./invariant"].as_object() {
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
    if !compiled && manifest["exports"]["./invariant"].is_array() {
        errors.push(format!("{label}: package.json exports[\"./invariant\"] must declare both types and default targets"));
    }
    check_files(
        label,
        manifest,
        &expected_files(manifest, compiled),
        &mut errors,
    );
    errors
}

/// The entry-point shape of a compiled package: no `main` or `bin`, and the root declaration
/// export present only together with `types`, both naming the generated index declaration.
fn compiled_entry_points(manifest: &Value, label: &str, source_entry: bool) -> Vec<String> {
    let mut errors = Vec::new();
    if source_entry {
        errors.push(format!(
            "{label}: seekdeep.compiled package must not keep a src/index.ts entry point"
        ));
    }
    for field in ["main", "bin"] {
        if !manifest[field].is_null() {
            errors.push(format!(
                "{label}: seekdeep.compiled package must not set {}",
                json(field)
            ));
        }
    }
    let types = &manifest["types"];
    let root = &manifest["exports"]["."];
    match (types.is_null(), root.is_null()) {
        (true, true) => {}
        (false, false) => {
            if *types != "lib/types/index.d.ts" {
                errors.push(format!(
                    "{label}: package.json must set \"types\": \"lib/types/index.d.ts\" when it declares a root export"
                ));
            }
            if *root != serde_json::json!({"types": "./lib/types/index.d.ts"}) {
                errors.push(format!(
                    "{label}: package.json exports[\".\"] must be exactly {{\"types\": \"./lib/types/index.d.ts\"}} for a compiled package"
                ));
            }
        }
        _ => errors.push(format!(
            "{label}: seekdeep.compiled package must declare \"types\" and exports[\".\"] together or neither"
        )),
    }
    errors
}

fn expected_files(manifest: &Value, compiled: bool) -> Vec<&str> {
    let mut files = if compiled {
        Vec::new()
    } else {
        vec!["lib/index.js", "lib/invariant.js"]
    };
    if !compiled && truthy(&manifest["bin"]) {
        files.push("lib/bin.js");
    }
    if !compiled && truthy(&manifest["exports"]["./worker"]) {
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
    files.extend(
        match manifest["name"].as_str() {
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
            Some("@seekdeep-ai/seekdeep-subprocess-local") => {
                vec!["scripts/ensure-spawn-helper.mjs"]
            }
            _ => vec![],
        }
        .into_iter()
        // A compiled package publishes no JavaScript of its own, so a name-specific runtime
        // entry (a packaged bin, a runner) is not part of its publication.
        .filter(|file| {
            !compiled
                || !std::path::Path::new(file)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("js"))
        }),
    );
    files.extend(wasm_publication_files(manifest["name"].as_str()));
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

/// Publication entries the source's rule cannot know about: browser packages are Rust compiled to
/// `WebAssembly` (`AGENTS.md`), so each ships its compiled module and the bindgen glue, and the
/// primitives package its highlight and markdown backends and the `KaTeX` assets, beside the entry
/// point. Without them the published tarball's `lib/index.js` would import files that were never
/// packed.
fn wasm_publication_files(name: Option<&str>) -> &'static [&'static str] {
    match name {
        Some("@seekdeep-ai/seekdeep-client-ui-primitives") => &[
            "lib/internal.js",
            "lib/client.js",
            "lib/client_bg.wasm",
            "lib/highlight-backend.js",
            "lib/markdown-backend.js",
            "lib/katex/**/*",
        ],
        Some("@seekdeep-ai/seekdeep-client-ui-attachment") => {
            &["lib/client.js", "lib/client_bg.wasm"]
        }
        Some("@seekdeep-ai/seekdeep-client-web") => &[
            "lib/client.js",
            "lib/client.d.ts",
            "lib/client_bg.wasm",
            "lib/base.css",
        ],
        Some(
            "@seekdeep-ai/seekdeep-client-web-react"
            | "@seekdeep-ai/seekdeep-client-ui-slots"
            | "@seekdeep-ai/seekdeep-client-modules"
            | "@seekdeep-ai/seekdeep-client-schema-form"
            | "@seekdeep-ai/seekdeep-client-test-runtime",
        ) => &["lib/wasm.js", "lib/wasm.d.ts", "lib/wasm_bg.wasm"],
        _ => &[],
    }
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
