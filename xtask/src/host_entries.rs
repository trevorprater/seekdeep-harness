//! `cargo xtask host-entries [--check]`: the JavaScript entries of the Host packages.
//!
//! Every `packages/<group>/<package>` manifest declares the npm entry points the pinned source
//! built from TypeScript: `lib/index.js`, `lib/invariant.js`, and any further runtime export or
//! `bin`. The port compiles the Host packages' runtime into the `seekdeep` binary, so no build step
//! emitted those files and the publication gates (`publint`, the built-package invariant probe, the
//! release pack) rejected every Host manifest. This command writes the entries each manifest
//! declares. The invariant companion keeps the Loader contract the source published (`name`,
//! `inject`, `apply`) under its canonical companion name; its installer is the source's no-op where
//! the no-op catalog says so and otherwise fails, naming the compiled Host, because the installer's
//! checks live in Rust. Every other runtime entry fails as soon as it is loaded for the same reason:
//! a consumer never runs a silent stand-in for compiled behavior. Packages that
//! `cargo xtask build-client` compiles to `WebAssembly` are skipped, as are the Typert artifacts the
//! Remote contract generators own.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::Context as _;
use regex::Regex;
use serde_json::Value;

/// Captured public declaration model, relative to the repository root.
const DECLARATION_MODEL: &str = "crates/api-remotes-client/contracts/client-declarations.json";
/// Catalog of the source invariant installers that are exactly no-ops, relative to the root.
const NOOP_CATALOG: &str = "crates/invariants/src/noop/catalog.rs";
/// Client runtime package that `build-client` compiles without a bundle script.
const CLIENT_RUNTIME: &str = "@seekdeep-ai/seekdeep-client-runtime";
/// Package-relative path of the invariant companion entry.
const COMPANION_ENTRY: &str = "lib/invariant.js";
/// Runtime entries that `remote-declarations` and the Remote build own.
const FOREIGN_ENTRIES: &[&str] = &["lib/typert.host.js", "lib/typert.remote-client.js"];

/// Entries written for one Host package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPackageEntries {
    /// Repository-relative package directory.
    pub directory: String,
    /// Package name.
    pub name: String,
    /// Package-relative entry paths in sorted order.
    pub files: Vec<String>,
}

/// One run's inventory in package discovery order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostEntriesReport {
    /// Host packages whose entries were written or verified.
    pub packages: Vec<HostPackageEntries>,
}

impl HostEntriesReport {
    /// Total number of entries written or verified.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.packages
            .iter()
            .map(|package| package.files.len())
            .sum()
    }
}

/// Writes every Host package's JavaScript entries below `output_root`, or verifies them with
/// `check`.
///
/// # Errors
///
/// Returns unreadable manifests, models, or catalogs; a Host package whose manifest declares the
/// companion entry without a canonical companion declaration; a companion name that differs
/// between the declaration model and the no-op catalog; and, in check mode, every missing or stale
/// entry.
pub fn run(root: &Path, output_root: &Path, check: bool) -> anyhow::Result<HostEntriesReport> {
    let companions = canonical_companions(root)?;
    let catalog = noop_catalog(root)?;
    let mut report = HostEntriesReport::default();
    let mut stale = Vec::new();
    for (directory, manifest) in host_manifests(root)? {
        let name = manifest
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .with_context(|| format!("{directory}/package.json: package name absent"))?;
        let executables = script_leaves(manifest.get("bin"));
        let mut entries = executables.clone();
        entries.extend(script_leaves(manifest.get("main")));
        entries.extend(script_leaves(manifest.get("exports")));
        entries.retain(|entry| !FOREIGN_ENTRIES.contains(&entry.as_str()));
        let mut files = Vec::new();
        for entry in &entries {
            let content = if entry == COMPANION_ENTRY {
                let canonical = companions.get(&directory).with_context(|| {
                    format!(
                        "{directory}: no canonical invariant companion declaration in {DECLARATION_MODEL}"
                    )
                })?;
                let noop = match catalog.get(name) {
                    Some(listed) if listed != canonical => anyhow::bail!(
                        "{directory}: companion name {canonical:?} differs from the no-op catalog's {listed:?}"
                    ),
                    Some(_) => true,
                    None => false,
                };
                companion(name, &directory, canonical, noop)
            } else {
                unavailable(name, &directory, entry, executables.contains(entry))
            };
            let path = output_root.join(&directory).join(entry);
            if check {
                if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
                    stale.push(format!("{directory}/{entry}"));
                }
            } else {
                std::fs::create_dir_all(path.parent().context("entry parent absent")?)?;
                std::fs::write(&path, content)?;
            }
            files.push(entry.clone());
        }
        report.packages.push(HostPackageEntries {
            directory,
            name: name.to_owned(),
            files,
        });
    }
    anyhow::ensure!(
        stale.is_empty(),
        "stale Host package entries; run `cargo xtask host-entries`:\n{}",
        stale.join("\n")
    );
    Ok(report)
}

/// Whether `build-client` compiles this package, so its entries are not written here.
fn is_client_build(manifest: &Value) -> bool {
    manifest.get("name").and_then(Value::as_str) == Some(CLIENT_RUNTIME)
        || manifest
            .pointer("/scripts/bundle")
            .and_then(Value::as_str)
            .is_some()
}

/// Depth-two package manifests that are not Client builds, sorted by directory.
fn host_manifests(root: &Path) -> anyhow::Result<Vec<(String, Value)>> {
    let mut manifests = Vec::new();
    for group in directories(&root.join("packages"))? {
        for package in directories(&group)? {
            let path = package.join("package.json");
            if !path.is_file() {
                continue;
            }
            let manifest: Value = serde_json::from_slice(&std::fs::read(&path)?)
                .with_context(|| format!("{}: invalid package manifest", path.display()))?;
            if is_client_build(&manifest) {
                continue;
            }
            manifests.push((relative(root, &package), manifest));
        }
    }
    manifests.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
    Ok(manifests)
}

fn directories(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut output = Vec::new();
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("{}: cannot list packages", path.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            output.push(entry.path());
        }
    }
    output.sort();
    Ok(output)
}

/// Package-relative runtime script paths declared anywhere below a manifest field.
fn script_leaves(value: Option<&Value>) -> BTreeSet<String> {
    let mut leaves = Vec::new();
    if let Some(value) = value {
        collect_strings(value, &mut leaves);
    }
    leaves
        .into_iter()
        .map(|leaf| leaf.strip_prefix("./").unwrap_or(&leaf).to_owned())
        .filter(|leaf| !leaf.contains('*') && matches!(extension(leaf), Some("js" | "cjs" | "mjs")))
        .collect()
}

fn collect_strings(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(text) => output.push(text.clone()),
        Value::Object(members) => members
            .values()
            .for_each(|member| collect_strings(member, output)),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, output)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Canonical companion names by package directory, from the captured `invariant.d.ts` headers.
fn canonical_companions(root: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    static NAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^export declare const name = "([^"]+)";$"#)
            .expect("static companion name regex")
    });
    let path = root.join(DECLARATION_MODEL);
    let model: Value = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("{}: cannot read", path.display()))?,
    )?;
    let modules = model
        .get("modules")
        .and_then(Value::as_array)
        .with_context(|| format!("{DECLARATION_MODEL}: modules array absent"))?;
    let mut names = BTreeMap::new();
    for module in modules {
        let output = module
            .get("output")
            .and_then(Value::as_str)
            .with_context(|| format!("{DECLARATION_MODEL}: module output absent"))?;
        let Some(directory) = output.strip_suffix("/lib/types/invariant.d.ts") else {
            continue;
        };
        let content = module
            .get("content")
            .and_then(Value::as_str)
            .with_context(|| format!("{output}: declaration content absent"))?;
        let name = NAME
            .captures(content)
            .and_then(|captures| captures.get(1))
            .with_context(|| format!("{output}: companion declaration has no name literal"))?;
        names.insert(rename_identity(directory), rename_identity(name.as_str()));
    }
    Ok(names)
}

/// No-op installer catalog as package name to companion name.
fn noop_catalog(root: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    static DESCRIPTOR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"NoopInvariantDescriptor::new\(\s*"[^"]+",\s*"([^"]+)",\s*"([^"]+)",?\s*\)"#)
            .expect("static no-op descriptor regex")
    });
    let path = root.join(NOOP_CATALOG);
    let catalog = std::fs::read_to_string(&path)
        .with_context(|| format!("{}: cannot read", path.display()))?;
    Ok(DESCRIPTOR
        .captures_iter(&catalog)
        .map(|captures| (captures[2].to_owned(), captures[1].to_owned()))
        .collect())
}

/// The product identity rename the captured source model has not applied yet.
fn rename_identity(value: &str) -> String {
    value.replace("dsh-", "seekdeep-")
}

fn companion(name: &str, directory: &str, companion: &str, noop: bool) -> String {
    let installer = if noop {
        "// The pinned source installer is a no-op (crates/invariants/src/noop/catalog.rs).\n\
         const install = () => {};\n"
            .to_owned()
    } else {
        format!(
            "// The installer's checks run inside the compiled seekdeep Host; this companion reserves the\n\
             // package's registry ownership and fails if a consumer asks it to install.\n\
             const install = () => {{\n  throw new Error({});\n}};\n",
            js_string(&format!(
                "{name}: the {companion} installer runs inside the compiled seekdeep Host; run the harness through the seekdeep executable"
            ))
        )
    };
    format!(
        "{}const PACKAGE_NAME = {};\nexport const name = {};\nexport const inject = ['invariants'];\n{installer}export const apply = ctx => Promise.resolve(ctx.invariants.register(PACKAGE_NAME, install));\n",
        header(directory),
        js_string(name),
        js_string(companion)
    )
}

fn unavailable(name: &str, directory: &str, entry: &str, executable: bool) -> String {
    let message = js_string(&format!(
        "{name}/{entry}: this package runs inside the compiled seekdeep Host and ships no JavaScript runtime; run the harness through the seekdeep executable"
    ));
    let mut content = String::new();
    if executable {
        content.push_str("#!/usr/bin/env node\n");
    }
    content.push_str(&header(directory));
    content.push_str("// The runtime of this package is compiled into the seekdeep Host binary.\n");
    if extension(entry) == Some("cjs") {
        content.push_str("'use strict';\nmodule.exports = {};\n");
    } else {
        content.push_str("export {};\n");
    }
    let _ = writeln!(content, "throw new Error({message});");
    content
}

fn extension(entry: &str) -> Option<&str> {
    Path::new(entry)
        .extension()
        .and_then(|extension| extension.to_str())
}

fn header(directory: &str) -> String {
    format!(
        "// Generated by `cargo xtask host-entries` from {directory}/package.json; do not edit.\n"
    )
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
