//! Exact publication-view staging and bounded Publint orchestration.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use regex::Regex;
use serde_json::Value;

const CONCURRENCY_ENV: &str = "SEEKDEEP_PUBLINT_CONCURRENCY";
const DEFAULT_FILES: &[&str] = &[
    "README*",
    "LICENSE*",
    "LICENCE*",
    "CHANGELOG*",
    "CHANGES*",
    "HISTORY*",
    "NOTICE*",
];

/// One discovered workspace package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublintPackage {
    /// Repository-relative package directory.
    pub path: String,
    /// Absolute package directory.
    pub directory: PathBuf,
    /// Parsed package manifest.
    pub manifest: Value,
}

/// One exact file in the package-manager publication view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationFile {
    /// Tar-style `package/<relative>` path.
    pub name: String,
    /// Exact file bytes.
    pub data: Vec<u8>,
}

/// Terminal package lint status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublintStatus {
    /// No Publint error messages.
    Passed,
    /// Publint error or staging/process failure.
    Failed,
}

/// Buffered result for one package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublintResult {
    /// Repository-relative package directory.
    pub path: String,
    /// Terminal status.
    pub status: PublintStatus,
    /// Publint-formatted standard output.
    pub output: String,
    /// Independent process standard error.
    pub error_output: String,
    /// Staging or process failure before ordinary Publint messages.
    pub failure: Option<String>,
}

/// Process dependencies for the real external Publint CLI boundary.
#[derive(Clone, Debug)]
pub struct PublintProcess {
    /// Node executable.
    pub node_executable: PathBuf,
    /// Pnpm JavaScript entrypoint.
    pub pnpm_entrypoint: PathBuf,
    /// Complete child environment.
    pub environment: BTreeMap<OsString, OsString>,
    /// Repository working directory used to resolve the Publint dependency.
    pub cwd: PathBuf,
}

impl PublintProcess {
    /// Discovers pnpm and Node paths from a package-script process.
    ///
    /// # Errors
    ///
    /// Returns a missing pnpm entrypoint diagnostic.
    pub fn from_process() -> anyhow::Result<Self> {
        let environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
        let pnpm_entrypoint = environment
            .get(OsStr::new("npm_execpath"))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "publint-all: npm_execpath is unavailable; invoke through the pnpm package script"
                )
            })?;
        let node_executable = environment
            .get(OsStr::new("npm_node_execpath"))
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from("node"), PathBuf::from);
        Ok(Self {
            node_executable,
            pnpm_entrypoint,
            environment,
            cwd: std::env::current_dir()?,
        })
    }
}

/// Discovers sorted `packages/<group>/<package>` targets.
///
/// # Errors
///
/// Returns traversal, manifest-read, or JSON failures.
pub fn workspace_packages(root: &Path) -> anyhow::Result<Vec<PublintPackage>> {
    let mut paths = Vec::new();
    let packages = root.join("packages");
    if !packages.is_dir() {
        return Ok(Vec::new());
    }
    for group in child_directories(&packages)? {
        for package in child_directories(&group)? {
            let manifest_path = package.join("package.json");
            if manifest_path.is_file() {
                paths.push(manifest_path);
            }
        }
    }
    paths.sort_by(|left, right| {
        relative(root, left)
            .encode_utf16()
            .cmp(relative(root, right).encode_utf16())
    });
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_by(|left, right| {
        slash_path(left)
            .encode_utf16()
            .cmp(slash_path(right).encode_utf16())
    });
    paths
        .into_iter()
        .map(|manifest_path| {
            let directory = manifest_path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("manifest has no package directory"))?
                .to_owned();
            Ok(PublintPackage {
                path: relative(root, &directory),
                directory,
                manifest: serde_json::from_str(&std::fs::read_to_string(manifest_path)?)?,
            })
        })
        .collect()
}

/// Resolves the bounded package worker count.
///
/// # Errors
///
/// Returns a canonical positive-integer diagnostic for invalid overrides.
pub fn publint_concurrency(
    total: usize,
    environment: &BTreeMap<OsString, OsString>,
    available: usize,
) -> anyhow::Result<usize> {
    if total == 0 {
        return Ok(0);
    }
    let Some(raw) = environment
        .get(OsStr::new(CONCURRENCY_ENV))
        .filter(|value| !value.is_empty())
        .and_then(|value| value.to_str())
    else {
        return Ok(total.min(available));
    };
    let parsed = raw.parse::<usize>().ok().filter(|value| *value >= 1);
    let Some(parsed) = parsed.filter(|value| value.to_string() == raw) else {
        anyhow::bail!(
            "publint-all: {CONCURRENCY_ENV} must be a positive integer, got {}.",
            serde_json::to_string(raw)?
        );
    };
    Ok(total.min(parsed))
}

/// Builds the exact manifest-declared npm publication file view.
///
/// Explicit directories recurse through dot-prefixed members. Glob patterns
/// follow npm's ordinary dot-excluding discovery behavior.
///
/// # Errors
///
/// Returns invalid manifest, missing declared path, traversal, or file-read failures.
pub fn publication_files(target: &PublintPackage) -> anyhow::Result<Vec<PublicationFile>> {
    let mut paths = BTreeSet::<PathBuf>::new();
    add_path(&target.directory.join("package.json"), &mut paths)?;
    let declared = target
        .manifest
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for pattern in declared.into_iter().chain(DEFAULT_FILES.iter().copied()) {
        add_pattern(target, pattern, &mut paths)?;
    }
    paths
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(&target.directory)?;
            Ok(PublicationFile {
                name: format!("package/{}", slash_path(relative)),
                data: std::fs::read(path)?,
            })
        })
        .collect()
}

fn add_pattern(
    target: &PublintPackage,
    pattern: &str,
    output: &mut BTreeSet<PathBuf>,
) -> anyhow::Result<()> {
    if !pattern.contains(['*', '?']) {
        let path = target.directory.join(pattern);
        return if path.exists() {
            add_path(&path, output)
        } else {
            Ok(())
        };
    }
    let matcher = glob_regex(pattern)?;
    for entry in walkdir::WalkDir::new(&target.directory).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(&target.directory)?;
        if has_dot_segment(relative) || !matcher.is_match(&slash_path(relative)) {
            continue;
        }
        add_path(entry.path(), output)?;
    }
    Ok(())
}

fn add_path(path: &Path, output: &mut BTreeSet<PathBuf>) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(path)?;
    if metadata.is_file() {
        output.insert(path.to_owned());
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in walkdir::WalkDir::new(path).min_depth(1) {
            let entry = entry?;
            if entry.file_type().is_file() {
                output.insert(entry.path().to_owned());
            }
        }
    }
    Ok(())
}

fn glob_regex(pattern: &str) -> anyhow::Result<Regex> {
    let mut expression = "^".to_owned();
    let normalized = pattern.replace('\\', "/");
    let mut characters = normalized.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '*' if characters.peek() == Some(&'*') => {
                characters.next();
                if characters.peek() == Some(&'/') {
                    expression.push_str("(?:.*/)?");
                    characters.next();
                } else {
                    expression.push_str(".*");
                }
            }
            '*' => {
                expression.push_str("[^/]*");
            }
            '?' => {
                expression.push_str("[^/]");
            }
            character => {
                expression.push_str(&regex::escape(&character.to_string()));
            }
        }
    }
    expression.push('$');
    Ok(Regex::new(&expression)?)
}

/// Runs every package through an injected linter with bounded workers.
///
/// Results always retain package discovery order.
///
/// # Errors
///
/// Returns invalid concurrency, worker-channel, or missing-result failures.
pub fn run_all<R>(
    targets: Vec<PublintPackage>,
    concurrency: usize,
    runner: R,
) -> anyhow::Result<Vec<PublintResult>>
where
    R: Fn(PublintPackage) -> PublintResult + Send + Sync + 'static,
{
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    anyhow::ensure!(
        concurrency > 0,
        "publint-all: worker count must be positive"
    );
    let targets = Arc::<[PublintPackage]>::from(targets);
    let runner = Arc::new(runner);
    let next = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = std::sync::mpsc::channel();
    let workers = concurrency.min(targets.len());
    for _ in 0..workers {
        let targets = Arc::clone(&targets);
        let runner = Arc::clone(&runner);
        let next = Arc::clone(&next);
        let sender = sender.clone();
        std::thread::spawn(move || {
            loop {
                let index = next.fetch_add(1, Ordering::SeqCst);
                let Some(target) = targets.get(index).cloned() else {
                    break;
                };
                let result = runner(target);
                if sender.send((index, result)).is_err() {
                    break;
                }
            }
        });
    }
    drop(sender);
    let mut results = (0..targets.len()).map(|_| None).collect::<Vec<_>>();
    for (index, result) in receiver {
        results[index] = Some(result);
    }
    results
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            result.ok_or_else(|| {
                anyhow::anyhow!("publint-all: missing result for {}.", targets[index].path)
            })
        })
        .collect()
}

/// Runs real Publint against one isolated staged publication view.
#[must_use]
pub fn run_publint(target: PublintPackage, process: &PublintProcess) -> PublintResult {
    match run_publint_inner(&target, process) {
        Ok(result) => result,
        Err(error) => PublintResult {
            path: target.path,
            status: PublintStatus::Failed,
            output: String::new(),
            error_output: String::new(),
            failure: Some(error.to_string()),
        },
    }
}

fn run_publint_inner(
    target: &PublintPackage,
    process: &PublintProcess,
) -> anyhow::Result<PublintResult> {
    let temporary = tempfile::Builder::new()
        .prefix("seekdeep-publint-all-")
        .tempdir()?;
    let package = temporary.path().join("package");
    for file in publication_files(target)? {
        let relative = file
            .name
            .strip_prefix("package/")
            .ok_or_else(|| anyhow::anyhow!("invalid publication file name {}", file.name))?;
        let destination = package.join(relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(destination, file.data)?;
    }
    let output = Command::new(&process.node_executable)
        .arg(&process.pnpm_entrypoint)
        .args(["exec", "publint", "run"])
        .arg(&package)
        .args(["--pack=false", "--level=suggestion"])
        .current_dir(&process.cwd)
        .env_clear()
        .envs(&process.environment)
        .stdin(Stdio::null())
        .output()?;
    Ok(PublintResult {
        path: target.path.clone(),
        status: if output.status.success() {
            PublintStatus::Passed
        } else {
            PublintStatus::Failed
        },
        output: String::from_utf8_lossy(&output.stdout).into_owned(),
        error_output: String::from_utf8_lossy(&output.stderr).into_owned(),
        failure: None,
    })
}

/// Renders one deterministic package result block.
#[must_use]
pub fn render_publint_result(result: &PublintResult) -> String {
    format!(
        "{}{}",
        render_publint_stdout(result),
        render_publint_stderr(result)
    )
}

/// Renders one package's standard-output block.
#[must_use]
pub fn render_publint_stdout(result: &PublintResult) -> String {
    let mut output = format!("Running publint for {}...\n", result.path);
    output.push_str(&result.output);
    if result.status == PublintStatus::Passed
        && result.failure.is_none()
        && result.output.trim().is_empty()
        && result.error_output.trim().is_empty()
    {
        output.push_str("All good!\n");
    }
    output
}

/// Renders one package's failure and standard-error block.
#[must_use]
pub fn render_publint_stderr(result: &PublintResult) -> String {
    let mut output = String::new();
    if let Some(failure) = &result.failure {
        output.push_str(failure);
        output.push('\n');
    }
    output.push_str(&result.error_output);
    output
}

fn child_directories(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            directories.push(entry.path());
        }
    }
    Ok(directories)
}

fn relative(root: &Path, path: &Path) -> String {
    slash_path(path.strip_prefix(root).unwrap_or(path))
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn has_dot_segment(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str().to_string_lossy().starts_with('.'))
}
