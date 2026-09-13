//! Package-owned artifact validation and publication for analyzed workspace models.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Result, TypertGeneratorError,
    emitter::{FaceModelEmitter, ModelEmitResult},
    model::{TypertFace, WorkspaceModel},
};

/// One package-face artifact and its workspace-relative package root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEmitResult {
    /// Complete emitted package-face artifacts.
    #[serde(flatten)]
    pub artifact: ModelEmitResult,
    /// Package root supplied by the analyzer.
    pub package_root: String,
}

/// Emits an analyzed model in face/package order, validating each owning manifest.
///
/// This function does not discover packages or analyze source, and writes no files.
///
/// # Errors
/// Propagates model projection, manifest reading, JSON, and artifact-publication violations.
pub fn emit_workspace_model(
    root: &Path,
    model: &WorkspaceModel,
) -> Result<Vec<WorkspaceEmitResult>> {
    let mut artifacts = Vec::new();
    for face in &model.faces {
        let emitter = FaceModelEmitter::new(face);
        for package in &face.packages {
            let artifact = emitter.emit(&package.name)?;
            let manifest_path = root.join(&package.root).join("package.json");
            let contents = std::fs::read(&manifest_path).map_err(|error| {
                TypertGeneratorError::Workspace(format!("{}: {error}", manifest_path.display()))
            })?;
            let manifest: Value = serde_json::from_slice(&contents).map_err(|error| {
                TypertGeneratorError::Syntax(format!("{}: {error}", manifest_path.display()))
            })?;
            validate_artifact_manifest(&manifest, &artifact)?;
            artifacts.push(WorkspaceEmitResult {
                artifact,
                package_root: package.root.clone(),
            });
        }
    }
    Ok(artifacts)
}

/// Validates source-compatible opt-in exports and packaged file membership.
///
/// Additional export conditions are allowed; the `types` and `default` targets
/// must match the root-level artifacts exactly. Remote maps remain workspace-only.
///
/// # Errors
/// Reports the first local export/file violation before checking Remote publication.
pub fn validate_artifact_manifest(manifest: &Value, artifact: &ModelEmitResult) -> Result<()> {
    let face = artifact.face.as_str();
    let subpath = if artifact.face == TypertFace::Host {
        "./typert"
    } else {
        "./client/typert"
    };
    let expected = json!({"types":format!("./lib/typert.{face}.d.ts"),"default":format!("./lib/typert.{face}.js")});
    require_export(manifest, artifact, subpath, &expected)?;
    for file in [
        format!("lib/typert.{face}.js"),
        format!("lib/typert.{face}.d.ts"),
    ] {
        require_file(manifest, artifact, &file)?;
    }
    if artifact.face != TypertFace::Host {
        return Ok(());
    }
    let remote_files = [
        "lib/typert.remote-client.js",
        "lib/typert.remote-client.d.ts",
    ];
    if artifact.remote.is_none() {
        if manifest["exports"].get("./remote").is_some()
            || remote_files
                .iter()
                .any(|file| includes_file(manifest, file))
        {
            return Err(TypertGeneratorError::Analysis(format!(
                "typert(host): {} publishes Remote artifacts but has no Remote methods",
                artifact.package
            )));
        }
        return Ok(());
    }
    require_export(
        manifest,
        artifact,
        "./remote",
        &json!({"types":"./lib/typert.remote-client.d.ts","default":"./lib/typert.remote-client.js"}),
    )?;
    for file in remote_files {
        require_file(manifest, artifact, file)?;
    }
    Ok(())
}

fn require_export(
    manifest: &Value,
    artifact: &ModelEmitResult,
    subpath: &str,
    expected: &Value,
) -> Result<()> {
    let actual = &manifest["exports"][subpath];
    if actual.is_object()
        && actual["types"] == expected["types"]
        && actual["default"] == expected["default"]
    {
        return Ok(());
    }
    Err(TypertGeneratorError::Analysis(format!(
        "typert({}): {} must export {subpath} as {expected}",
        artifact.face.as_str(),
        artifact.package
    )))
}

fn require_file(manifest: &Value, artifact: &ModelEmitResult, file: &str) -> Result<()> {
    if includes_file(manifest, file) {
        return Ok(());
    }
    Err(TypertGeneratorError::Analysis(format!(
        "typert({}): {} package files must include {file}",
        artifact.face.as_str(),
        artifact.package
    )))
}

fn includes_file(manifest: &Value, file: &str) -> bool {
    manifest["files"]
        .as_array()
        .is_some_and(|files| files.iter().any(|entry| entry.as_str() == Some(file)))
}

/// Whether a manifest explicitly opts into a Typert or Remote artifact export.
pub fn has_typert_export(exports: &Value) -> bool {
    exports.as_object().is_some_and(|exports| {
        ["./typert", "./client/typert", "./remote"]
            .iter()
            .any(|key| exports.contains_key(*key))
    })
}

/// Writes only the owning package's root-level artifacts.
///
/// A Host pass without Remote methods removes stale Remote files. A Client-only
/// pass leaves Host artifacts untouched. Publication is ordered, not transactional,
/// matching the source build hook's failure behavior.
///
/// # Errors
/// Propagates directory creation, writing, and stale-file removal errors.
pub fn write_artifacts(
    package_root: &Path,
    artifacts: &[WorkspaceEmitResult],
) -> std::io::Result<()> {
    let output = package_root.join("lib");
    std::fs::create_dir_all(&output)?;
    let mut emitted_remote = false;
    for item in artifacts {
        let artifact = &item.artifact;
        let face = artifact.face.as_str();
        std::fs::write(output.join(format!("typert.{face}.js")), &artifact.js)?;
        std::fs::write(output.join(format!("typert.{face}.d.ts")), &artifact.dts)?;
        if let Some(remote) = &artifact.remote {
            emitted_remote = true;
            std::fs::write(output.join("typert.remote-client.js"), &remote.js)?;
            std::fs::write(output.join("typert.remote-client.d.ts"), &remote.dts)?;
            std::fs::write(
                output.join("typert.remote-client.d.ts.map"),
                &remote.dts_map,
            )?;
        }
    }
    if !emitted_remote
        && artifacts
            .iter()
            .any(|item| item.artifact.face == TypertFace::Host)
    {
        for name in [
            "typert.remote-client.js",
            "typert.remote-client.d.ts",
            "typert.remote-client.d.ts.map",
        ] {
            let path = output.join(name);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

/// Finds the nearest aggregate Host workspace above a bundle directory.
///
/// # Errors
/// Reports a missing workspace root or an inaccessible current directory.
pub fn workspace_root(start: &Path) -> Result<PathBuf> {
    let mut current = absolute(start)?;
    loop {
        if current.join("tsconfig.host.json").exists() {
            return Ok(current);
        }
        if !current.pop() {
            return Err(TypertGeneratorError::Workspace(format!(
                "typert-generator: cannot find workspace root above {}",
                start.display()
            )));
        }
    }
}

/// Finds the nearest package manifest without treating the workspace itself as a package.
///
/// # Errors
/// Propagates current-directory lookup errors for a relative start path.
pub fn package_root(start: &Path, workspace: &Path) -> Result<Option<PathBuf>> {
    let mut current = absolute(start)?;
    while current != workspace {
        if current.join("package.json").exists() {
            return Ok(Some(current));
        }
        if !current.pop() {
            return Ok(None);
        }
    }
    Ok(None)
}

fn absolute(path: &Path) -> Result<PathBuf> {
    crate::analyzer::source_path_for_export(path, ".")
}

/// Discover, analyze, and emit package reflection from independent faces.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct WorkspaceTypertGenerator {
    root: PathBuf,
    caches: Option<std::rc::Rc<std::cell::RefCell<crate::analyzer::WorkspaceCaches>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl WorkspaceTypertGenerator {
    /// Binds generation to one workspace root containing the face aggregate tsconfigs.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            caches: None,
        }
    }

    /// Binds generation to one workspace root over a shared compiler memo.
    pub fn with_caches(
        root: impl Into<PathBuf>,
        caches: std::rc::Rc<std::cell::RefCell<crate::analyzer::WorkspaceCaches>>,
    ) -> Self {
        Self {
            root: root.into(),
            caches: Some(caches),
        }
    }

    fn analyzer(
        &self,
        packages: Option<Vec<String>>,
        faces: Option<Vec<TypertFace>>,
    ) -> Result<crate::analyzer::WorkspaceAnalyzer> {
        crate::analyzer::WorkspaceAnalyzer::new(crate::analyzer::WorkspaceAnalyzerOptions {
            root: self.root.clone(),
            packages,
            faces,
            caches: self.caches.clone(),
            ..Default::default()
        })
    }

    /// Finds public package faces that contribute Cordis services/events or
    /// explicitly tagged Typert roots, in stable package-name order.
    ///
    /// # Errors
    /// Propagates configuration and discovery failures.
    pub fn discover(
        &self,
        faces: Option<Vec<TypertFace>>,
    ) -> Result<Vec<crate::analyzer::DiscoveredTypertPackage>> {
        self.analyzer(None, faces)?.discover_packages()
    }

    /// Generates all discovered contributors, or an explicit package subset,
    /// producing one artifact per package face.
    ///
    /// # Errors
    /// Propagates analysis, emission, and manifest-validation failures.
    pub fn generate(
        &self,
        packages: Option<Vec<String>>,
        faces: Option<Vec<TypertFace>>,
    ) -> Result<Vec<WorkspaceEmitResult>> {
        let selected = match packages {
            Some(packages) => packages,
            None => self
                .discover(faces.clone())?
                .into_iter()
                .map(|candidate| candidate.package)
                .collect(),
        };
        let model = self.analyzer(Some(selected), faces)?.analyze()?;
        emit_workspace_model(&self.root, &model)
    }
}
