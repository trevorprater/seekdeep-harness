//! Client bundle faces, module-edge rules, CSS compilation, and browser source paths.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use lightningcss::{
    css_modules::{Config as CssModulesConfig, Pattern},
    stylesheet::{MinifyOptions, ParserOptions, PrinterOptions, StyleSheet},
};
use path_clean::PathClean as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Browser source maps for compiled Rust/WASM artifacts and their generated bindings.
pub mod sourcemaps;

/// Package identity passed to the browser module table.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ClientModuleId(pub String);

/// Entries whose runtime identities the browser shell shares with every plugin.
pub const CLIENT_EXTERNALS: &[&str] = &[
    "react",
    "react/jsx-runtime",
    "react-dom",
    "react-dom/client",
    "@seekdeep-ai/cordis",
    "@seekdeep-ai/seekdeep-client-ui-slots",
    "@seekdeep-ai/seekdeep-client-web-react",
    "@seekdeep-ai/seekdeep-client-ui-primitives",
    "@seekdeep-ai/seekdeep-client-ui-attachment",
    "@seekdeep-ai/seekdeep-client-schema-form",
    "@seekdeep-ai/seekdeep-client-runtime/client",
];

/// Build pass selecting a package's Host library or Client artifact.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildFace {
    /// Node artifacts required before Host reflection.
    Host,
    /// Browser artifacts plus libraries not emitted by the Host pass.
    Client,
}

/// Package-local configuration additions to the shared build preset.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientBundleOptions {
    /// Emit Node-side configurations during the Host pass.
    #[serde(default)]
    pub host_phase: bool,
    /// Additional Node-side configurations emitted with the primary library.
    #[serde(default)]
    pub companions: Vec<Value>,
    /// Overrides for the primary library's configuration.
    #[serde(default)]
    pub lib: serde_json::Map<String, Value>,
}

/// Parse the environment build face; absence selects the development preset.
///
/// # Errors
/// Returns an error for a value other than `host` or `client`.
pub fn build_face(value: Option<&Value>) -> anyhow::Result<Option<BuildFace>> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) if value == "host" => Ok(Some(BuildFace::Host)),
        Some(Value::String(value)) if value == "client" => Ok(Some(BuildFace::Client)),
        Some(value) => anyhow::bail!(
            "tsdown: --env.SEEKDEEP_BUILD_FACE must be host or client, received {}",
            js_display(value)
        ),
    }
}

fn js_display(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Object(_) => "[object Object]".to_owned(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    js_display(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Number(number) => number.as_f64().map_or_else(
            || number.to_string(),
            |number| ryu_js::Buffer::new().format(number).to_owned(),
        ),
        value => value.to_string(),
    }
}

/// Select arbitrary Client-only Node configurations for the current pass.
pub fn client_only(configs: &[Value], face: Option<BuildFace>) -> Vec<Value> {
    if face == Some(BuildFace::Host) {
        vec![json!({ "entry": "" })]
    } else {
        configs.to_vec()
    }
}

/// Construct the Node library half without cleaning its shared output directory.
///
/// # Panics
/// Panics only if the static object literal ceases to be a JSON object.
pub fn client_library_config(
    id: &ClientModuleId,
    entries: &[String],
    overrides: &serde_json::Map<String, Value>,
) -> Value {
    let mut config = json!({
        "name": id,
        "entry": entries,
        "outDir": "lib",
        "format": ["esm"],
        "platform": "node",
        "target": "es2024",
        "fixedExtension": false,
        "dts": false,
        "clean": false,
    });
    config
        .as_object_mut()
        .expect("library configuration is an object")
        .extend(overrides.clone());
    config
}

/// Construct the shared two-face preset as data consumed by build adapters.
pub fn client_bundle(
    id: &ClientModuleId,
    entries: &[String],
    options: &ClientBundleOptions,
    face: Option<BuildFace>,
    node_env: Option<&str>,
) -> Vec<Value> {
    let mut node = vec![client_library_config(id, entries, &options.lib)];
    node.extend(options.companions.clone());
    match face {
        Some(BuildFace::Host) if options.host_phase => node,
        Some(BuildFace::Host) => vec![json!({ "entry": "" })],
        face => {
            let entry = if face.is_none() {
                "src/client/index.ts"
            } else {
                "lib/types/client/index.js"
            };
            let client = browser_config(id, entry, node_env);
            if face == Some(BuildFace::Client) && options.host_phase {
                vec![client]
            } else {
                node.push(client);
                node
            }
        }
    }
}

fn browser_config(id: &ClientModuleId, entry: &str, node_env: Option<&str>) -> Value {
    let mode = node_env.unwrap_or("production");
    json!({
        "name": format!("{}/client", id.0),
        "entry": { "client": entry },
        "outDir": "lib",
        "format": "cjs",
        "platform": "browser",
        "dts": false,
        "sourcemap": true,
        "clean": false,
        "external": CLIENT_EXTERNALS,
        "define": {
            "process.env.NODE_ENV": Value::String(mode.to_owned()).to_string(),
            "import.meta.env.MODE": Value::String(mode.to_owned()).to_string(),
            "import.meta.env": json!({ "MODE": mode }).to_string(),
        },
        "plugins": [
            { "name": "seekdeep-client-bundle-purity" },
            { "name": "seekdeep-css-modules-inline" },
        ],
        "outputOptions": {
            "entryFileNames": "client.js",
            "banner": format!("window.__ModuleLoader__.load({{ id: {}, factory: (require) => {{", Value::String(id.0.clone())),
            "footer": "return module.exports; } });",
            "intro": "var module = { exports: {} }; var exports = module.exports;",
        },
    })
}

/// Whether the shared module table answers this exact import specifier.
pub fn is_external(specifier: &str) -> bool {
    CLIENT_EXTERNALS.contains(&specifier)
}

/// Reject cross-plugin value imports before an artifact reaches the browser.
///
/// # Errors
/// Returns an error for a scoped import outside the shared or inline-safe module sets.
pub fn check_import(specifier: &str) -> anyhow::Result<()> {
    let Some(scoped) = specifier.strip_prefix("@seekdeep-ai/") else {
        return Ok(());
    };
    if is_external(specifier)
        || ["cosmokit", "schemastery"]
            .iter()
            .any(|library| package_or_subpath(scoped, library))
        || ["host-apiproxy", "session", "llm", "tools", "brand"]
            .iter()
            .any(|package| package_or_subpath(scoped, &format!("seekdeep-{package}")))
        || generated_remote(scoped)
    {
        return Ok(());
    }
    anyhow::bail!(
        "client bundle purity: \"{specifier}\" is not a platform module (CLIENT_EXTERNALS), an inline-safe wire layer, or a generated /remote contribution — cross-plugin value imports are forbidden; collaborate through cordis services (type-only imports are erased and never reach this gate)"
    )
}

fn package_or_subpath(specifier: &str, package: &str) -> bool {
    specifier == package
        || specifier
            .strip_prefix(package)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn generated_remote(specifier: &str) -> bool {
    specifier
        .strip_prefix("seekdeep-")
        .and_then(|name| name.strip_suffix("/remote"))
        .is_some_and(|name| {
            name.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
        })
}

/// Rebase a lib-relative source map entry onto its repository package URL.
pub fn browser_source_path(source: &str, sourcemap: &Path, repository: &Path) -> String {
    if !source.starts_with('.') {
        return source.to_owned();
    }
    let physical = sourcemap
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(source)
        .clean();
    let Ok(relative) = physical.strip_prefix(repository.clean()) else {
        return source.to_owned();
    };
    let relative = relative
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    if relative.starts_with("packages/") {
        format!("../../../{relative}")
    } else {
        source.to_owned()
    }
}

/// Resolve a stylesheet beside emitted JavaScript, falling back to its source tree.
pub fn source_asset_path(source: &str, importer: &Path) -> PathBuf {
    let emitted = importer
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(source)
        .clean();
    if emitted.exists() {
        return emitted;
    }
    let components: Vec<_> = emitted.components().collect();
    let marker = components
        .windows(2)
        .position(|pair| pair[0].as_os_str() == "lib" && pair[1].as_os_str() == "types");
    let Some(boundary) = marker else {
        return emitted;
    };
    let mut source: PathBuf = components[..boundary].iter().collect();
    source.push("src");
    source.extend(components[boundary + 2..].iter());
    source
}

/// Compile-time data for one physical CSS Module and its ownership identity.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledCssModule {
    /// Minified CSS with scoped selectors.
    pub css: String,
    /// Local names and the exact scoped names used by the stylesheet.
    pub classes: BTreeMap<String, String>,
    /// Physical input that must remain part of the watch graph.
    pub watch_file: PathBuf,
    /// Plugin package owning the style tag.
    pub plugin: ClientModuleId,
    /// Per-plugin basename identity used to suppress duplicate style tags.
    pub tag_id: String,
}

/// Compile a CSS Module using the same pinned Rust compiler and pattern as the source.
///
/// # Errors
/// Returns file-read, stylesheet parse, transform, or serialization errors.
pub fn compile_css_module(
    plugin: &ClientModuleId,
    file: &Path,
) -> anyhow::Result<CompiledCssModule> {
    let bytes = std::fs::read(file)?;
    let source = String::from_utf8_lossy(&bytes);
    let mut stylesheet = StyleSheet::parse(
        &source,
        ParserOptions {
            filename: file.to_string_lossy().into_owned(),
            css_modules: Some(CssModulesConfig {
                pattern: Pattern::parse("[hash]_[local]")?,
                ..CssModulesConfig::default()
            }),
            ..ParserOptions::default()
        },
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    stylesheet
        .minify(MinifyOptions::default())
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let output = stylesheet
        .to_css(PrinterOptions {
            minify: true,
            ..PrinterOptions::default()
        })
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let classes = output
        .exports
        .unwrap_or_default()
        .into_iter()
        .map(|(local, export)| (local, export.name))
        .collect();
    Ok(CompiledCssModule {
        css: output.code,
        classes,
        watch_file: file.to_owned(),
        plugin: plugin.clone(),
        tag_id: format!(
            "{}/{}",
            plugin.0,
            file.file_name().unwrap_or_default().to_string_lossy()
        ),
    })
}
