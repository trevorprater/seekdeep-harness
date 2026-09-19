//! External `NodeNext` consumer validation for generated TypeScript declarations.

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

use regex::Regex;
use serde_json::Value;

/// One named workspace package and its compatibility manifest.
#[derive(Clone, Debug)]
pub struct NodeNextWorkspacePackage {
    /// Absolute package directory.
    pub directory: PathBuf,
    /// Published package name.
    pub name: String,
    manifest: Value,
}

impl NodeNextWorkspacePackage {
    /// Public declaration-bearing package specifiers exported by the manifest.
    #[must_use]
    pub fn public_specifiers(&self) -> Vec<String> {
        let mut specifiers = BTreeSet::new();
        if manifest_string(&self.manifest, "types").is_some() {
            specifiers.insert(self.name.clone());
        }
        if let Some(exports) = self.manifest.get("exports").and_then(Value::as_object) {
            for (key, target) in exports {
                if key.contains('*') || key == "./package.json" {
                    continue;
                }
                let Some(types) = target
                    .as_object()
                    .and_then(|target| target.get("types"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                if types.is_empty() {
                    continue;
                }
                specifiers.insert(if key == "." {
                    self.name.clone()
                } else if let Some(subpath) = key.strip_prefix("./") {
                    format!("{}/{subpath}", self.name)
                } else {
                    format!("{}/{}", self.name, key.get(2..).unwrap_or_default())
                });
            }
        }
        specifiers.into_iter().collect()
    }

    fn types_path(&self) -> Option<&str> {
        manifest_string(&self.manifest, "types")
    }
}

/// Source-compatible verifier outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeNextTypesReport {
    /// Every public compatibility declaration compiled in the consumer.
    Success {
        /// Named workspace packages linked into the consumer.
        packages: usize,
    },
    /// Generated declarations contain extensionless relative specifiers.
    MissingSpecifierExtensions(Vec<String>),
    /// Package-level `types` entries name outputs that do not exist.
    MissingOutputs(Vec<String>),
    /// The external `tsc` consumer returned unsuccessfully.
    ConsumerTypecheckFailed(String),
}

/// Discovers named vendor and depth-two workspace packages.
///
/// # Errors
///
/// Returns traversal, file-read, or JSON parse failures.
pub fn node_next_workspace_packages(root: &Path) -> anyhow::Result<Vec<NodeNextWorkspacePackage>> {
    let mut packages = Vec::new();
    for directory in candidate_package_directories(root)? {
        let manifest_path = directory.join("package.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = serde_json::from_str::<Value>(&std::fs::read_to_string(&manifest_path)?)?;
        let Some(name) = manifest.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        packages.push(NodeNextWorkspacePackage {
            directory,
            name: name.to_owned(),
            manifest,
        });
    }
    packages.sort_by(|left, right| left.name.encode_utf16().cmp(right.name.encode_utf16()));
    Ok(packages)
}

/// Scans generated declarations for extensionless relative module specifiers.
///
/// # Errors
///
/// Returns traversal, relative-path, or file-read failures.
pub fn relative_specifiers_missing_extensions(root: &Path) -> anyhow::Result<Vec<String>> {
    static SPECIFIER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r#"(?:from\s*|import\s*\(\s*|import\s+|declare\s+module\s*)["'](\.{0,2}(?:/[^"']*)?)["']"#,
        )
        .expect("static declaration specifier regex")
    });
    static HAS_EXTENSION: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\.[^/.]+$").expect("static extension regex"));

    let mut files = declaration_files(root)?;
    files.sort_by(|left, right| {
        slash_path(left.strip_prefix(root).unwrap_or(left))
            .encode_utf16()
            .cmp(slash_path(right.strip_prefix(root).unwrap_or(right)).encode_utf16())
    });
    let mut errors = Vec::new();
    for file in files {
        let relative = slash_path(file.strip_prefix(root)?);
        let source = std::fs::read_to_string(file)?;
        for captures in SPECIFIER.captures_iter(&source) {
            let Some(specifier) = captures.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            let relative_specifier =
                specifier == "." || specifier.starts_with("./") || specifier.starts_with("../");
            if relative_specifier && !HAS_EXTENSION.is_match(specifier) {
                errors.push(format!("{relative}: {specifier}"));
            }
        }
    }
    Ok(errors)
}

/// Runs the complete generated-declaration `NodeNext` consumer gate.
///
/// # Errors
///
/// Returns repository discovery, file, manifest, temporary-directory, symlink,
/// process-spawn, or cleanup preparation failures.
pub fn verify_node_next_types(root: &Path) -> anyhow::Result<NodeNextTypesReport> {
    let packages = node_next_workspace_packages(root)?;
    let bad_specifiers = relative_specifiers_missing_extensions(root)?;
    if !bad_specifiers.is_empty() {
        return Ok(NodeNextTypesReport::MissingSpecifierExtensions(
            bad_specifiers,
        ));
    }

    let missing_outputs = packages
        .iter()
        .filter_map(|package| {
            let types = package.types_path()?;
            (!package.directory.join(types).exists())
                .then(|| format!("{}: missing {types}", package.name))
        })
        .collect::<Vec<_>>();
    if !missing_outputs.is_empty() {
        return Ok(NodeNextTypesReport::MissingOutputs(missing_outputs));
    }

    let temporary = DeterministicTempDirectory::new(root)?;
    let node_modules = temporary.path.join("node_modules");
    std::fs::create_dir_all(&node_modules)?;
    for package in &packages {
        link_package(package, &node_modules)?;
    }
    let root_node_types = root.join("node_modules/@types/node");
    if root_node_types.exists() {
        let types_directory = node_modules.join("@types");
        std::fs::create_dir_all(&types_directory)?;
        symlink_directory(&root_node_types, &types_directory.join("node"))?;
    }

    write_consumer_files(&temporary.path, &packages)?;
    let output = Command::new("node")
        .arg("node_modules/typescript/bin/tsc")
        .arg("-p")
        .arg(temporary.path.join("tsconfig.json"))
        .arg("--pretty")
        .arg("false")
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        let mut diagnostic = String::from_utf8_lossy(&output.stdout).into_owned();
        diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
        return Ok(NodeNextTypesReport::ConsumerTypecheckFailed(diagnostic));
    }
    Ok(NodeNextTypesReport::Success {
        packages: packages.len(),
    })
}

/// Renders the source-compatible gate outcome.
#[must_use]
pub fn render_node_next_types_report(report: &NodeNextTypesReport) -> String {
    match report {
        NodeNextTypesReport::Success { packages } => format!(
            "verify-node-next-types: {packages} workspace package declaration API(s) compile under NodeNext.\n"
        ),
        NodeNextTypesReport::MissingSpecifierExtensions(errors) => format!(
            "verify-node-next-types: declaration files still contain relative specifiers without file extensions.\n{}\n",
            errors.join("\n")
        ),
        NodeNextTypesReport::MissingOutputs(errors) => format!(
            "verify-node-next-types: build outputs are missing; run `pnpm run build` first.\n{}\n",
            errors.join("\n")
        ),
        NodeNextTypesReport::ConsumerTypecheckFailed(diagnostic) => {
            let mut output =
                "verify-node-next-types: NodeNext consumer typecheck failed.\n\n".to_owned();
            output.push_str(diagnostic);
            if !diagnostic.ends_with('\n') {
                output.push('\n');
            }
            output
        }
    }
}

fn candidate_package_directories(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    let vendor = root.join("vendor");
    if vendor.is_dir() {
        for entry in std::fs::read_dir(vendor)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && !hidden_name(&entry.file_name()) {
                directories.push(entry.path());
            }
        }
    }
    let packages = root.join("packages");
    if packages.is_dir() {
        for group in std::fs::read_dir(packages)? {
            let group = group?;
            if !group.file_type()?.is_dir() || hidden_name(&group.file_name()) {
                continue;
            }
            for package in std::fs::read_dir(group.path())? {
                let package = package?;
                if package.file_type()?.is_dir() && !hidden_name(&package.file_name()) {
                    directories.push(package.path());
                }
            }
        }
    }
    Ok(directories)
}

fn declaration_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for package in candidate_package_directories(root)? {
        let types = package.join("lib/types");
        if !types.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(types)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !hidden_name(entry.file_name()))
        {
            let entry = entry?;
            if entry.file_type().is_file() && entry.file_name().to_string_lossy().ends_with(".d.ts")
            {
                files.push(entry.path().to_owned());
            }
        }
    }
    Ok(files)
}

fn write_consumer_files(
    temporary: &Path,
    packages: &[NodeNextWorkspacePackage],
) -> anyhow::Result<()> {
    std::fs::write(
        temporary.join("package.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "type": "module",
                "private": true,
            }))?
        ),
    )?;
    std::fs::write(
        temporary.join("tsconfig.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "compilerOptions": {
                    "target": "es2024",
                    "module": "NodeNext",
                    "moduleResolution": "NodeNext",
                    "strict": true,
                    "skipLibCheck": true,
                    "preserveSymlinks": true,
                    "noEmit": true,
                    "types": ["node"],
                },
                "include": ["index.ts"],
            }))?
        ),
    )?;
    let mut imports = String::new();
    let mut index = 0;
    for package in packages {
        for specifier in package.public_specifiers() {
            let _ = writeln!(
                imports,
                "import * as mod{index} from {};\nvoid mod{index};",
                serde_json::to_string(&specifier)?
            );
            index += 1;
        }
    }
    imports.push('\n');
    std::fs::write(temporary.join("index.ts"), imports)?;
    Ok(())
}

fn link_package(package: &NodeNextWorkspacePackage, node_modules: &Path) -> anyhow::Result<()> {
    let link = package
        .name
        .split('/')
        .fold(node_modules.to_owned(), |path, part| path.join(part));
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    symlink_directory(&package.directory, &link)?;
    Ok(())
}

#[cfg(unix)]
fn symlink_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, destination)
}

#[cfg(windows)]
fn symlink_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(source, destination)
}

#[cfg(not(any(unix, windows)))]
fn symlink_directory(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory symlinks are unsupported on this platform",
    ))
}

struct DeterministicTempDirectory {
    path: PathBuf,
}

impl DeterministicTempDirectory {
    fn new(root: &Path) -> anyhow::Result<Self> {
        for attempt in 0..1_000_u16 {
            let path = root.join(format!(".node-next-types-{}-{attempt}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("verify-node-next-types: could not allocate a temporary consumer directory")
    }
}

impl Drop for DeterministicTempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn manifest_string<'a>(manifest: &'a Value, key: &str) -> Option<&'a str> {
    manifest.get(key).and_then(Value::as_str)
}

fn hidden_name(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
