//! Manifest-owned Client builds and dependency-aware development watching.

use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use anyhow::Context as _;
use notify::Watcher as _;
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

pub(super) fn inline_store_dependency(root: &Path, bundle: &str) -> anyhow::Result<String> {
    use std::io::Write as _;
    let dependencies = root.join("support/browser-dependencies/node_modules");
    let esbuild = dependencies.join("esbuild/bin/esbuild");
    let package = dependencies.join("immer/dist/immer.mjs");
    anyhow::ensure!(
        esbuild.is_file() && package.is_file(),
        "install the pinned browser build dependencies: pnpm --dir support/browser-dependencies install --ignore-workspace --frozen-lockfile"
    );
    let mode = serde_json::to_string(
        &std::env::var("NODE_ENV").unwrap_or_else(|_| "production".to_owned()),
    )?;
    // A trailing import leaves the existing binding-map offsets intact. ES modules still
    // initialize the imported dependency before this module's body executes.
    let entry = format!(
        "{bundle}\nimport {{ produce as __seekdeepStoreProduce }} from {};\n",
        serde_json::to_string(&package.to_string_lossy())?
    );
    let mut child = std::process::Command::new("node")
        .arg(esbuild)
        .args([
            "--bundle",
            "--format=iife",
            "--platform=browser",
            "--target=es2024",
            "--sourcemap=inline",
            "--legal-comments=inline",
            "--sourcefile=client-runtime.js",
        ])
        .arg(format!("--define:process.env.NODE_ENV={mode}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("Client dependency compiler has no stdin")?
        .write_all(entry.as_bytes())?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "Client store dependency bundle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WatchMode {
    Native,
    Poll { interval: Duration, display: String },
}

fn parse_watch_mode(args: &[String]) -> anyhow::Result<WatchMode> {
    let poll = args
        .iter()
        .find(|arg| *arg == "--poll" || arg.starts_with("--poll="));
    anyhow::ensure!(
        args.iter().all(|arg| Some(arg) == poll),
        "dev-web: usage: cargo xtask dev-web [--poll[=ms]]"
    );
    let Some(poll) = poll else {
        return Ok(WatchMode::Native);
    };
    let value = poll.split('=').nth(1).unwrap_or("500");
    let interval = js_number(value);
    anyhow::ensure!(
        interval.is_finite() && interval.fract() == 0.0 && interval > 0.0,
        "dev-web: invalid --poll interval \"{poll}\""
    );
    let display = ryu_js::Buffer::new().format(interval).to_owned();
    // Node clamps timer intervals outside its signed 32-bit timer range to one millisecond.
    let interval = if interval > f64::from(i32::MAX) {
        1
    } else {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            interval as u64
        }
    };
    Ok(WatchMode::Poll {
        interval: Duration::from_millis(interval),
        display,
    })
}

fn js_number(value: &str) -> f64 {
    let value = value.trim_matches(|character: char| {
        matches!(character,
            '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
            '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
            '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    });
    if value.is_empty() {
        return 0.0;
    }
    let radix = [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ]
    .into_iter()
    .find_map(|(prefix, radix)| value.strip_prefix(prefix).map(|digits| (digits, radix)));
    if let Some((digits, radix)) = radix {
        return radix_number(digits, radix).unwrap_or(f64::NAN);
    }
    if value.eq_ignore_ascii_case("inf")
        || value.eq_ignore_ascii_case("infinity")
            && !matches!(value, "Infinity" | "+Infinity" | "-Infinity")
    {
        return f64::NAN;
    }
    value.parse().unwrap_or(f64::NAN)
}

fn radix_number(digits: &str, radix: u32) -> Option<f64> {
    if digits.is_empty() {
        return None;
    }
    let digits = digits
        .chars()
        .map(|digit| digit.to_digit(radix))
        .collect::<Option<Vec<_>>>()?;
    let Some(first) = digits.iter().position(|digit| *digit != 0) else {
        return Some(0.0);
    };
    let bits_per_digit = radix.trailing_zeros();
    let width = (digits.len() - first - 1)
        .saturating_mul(usize::try_from(bits_per_digit).ok()?)
        .saturating_add(usize::try_from(u32::BITS - digits[first].leading_zeros()).ok()?);
    if width > 1024 {
        return Some(f64::INFINITY);
    }
    let mut significand = 0_u64;
    let mut count = 0;
    let mut guard = false;
    let mut sticky = false;
    for digit in &digits[first..] {
        for shift in (0..bits_per_digit).rev() {
            let bit = (digit >> shift) & 1;
            if count == 0 && bit == 0 {
                continue;
            }
            match count {
                0..=52 => significand = (significand << 1) | u64::from(bit),
                53 => guard = bit != 0,
                _ => sticky |= bit != 0,
            }
            count += 1;
        }
    }
    significand <<= 53 - count.min(53);
    // Round once to binary64; rounding each input digit can lose the final tie-breaking bit.
    significand += u64::from(guard && (sticky || significand & 1 != 0));
    let mut exponent = u64::try_from(width).ok()? + 1022;
    if significand == 1 << 53 {
        significand >>= 1;
        exponent += 1;
    }
    if exponent >= 2047 {
        return Some(f64::INFINITY);
    }
    Some(f64::from_bits(
        (exponent << 52) | (significand & ((1 << 52) - 1)),
    ))
}

fn grouped_manifests(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let directory = root.join("packages");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for group in std::fs::read_dir(directory)? {
        let group = group?;
        if !group.path().is_dir() || group.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        for package in std::fs::read_dir(group.path())? {
            let package = package?;
            if package.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let manifest = package.path().join("package.json");
            if manifest.is_file() {
                result.push(manifest);
            }
        }
    }
    result.sort();
    Ok(result)
}

fn read_manifest(path: &Path) -> anyhow::Result<Value> {
    serde_json::from_slice(&std::fs::read(path)?)
        .with_context(|| format!("invalid package manifest: {}", path.display()))
}

fn is_web_plugin(manifest: &Value) -> bool {
    manifest
        .pointer("/seekdeep/client/platform")
        .and_then(Value::as_str)
        == Some("web")
}

fn discover_plugin_dirs(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    grouped_manifests(root)?
        .into_iter()
        .filter_map(|path| match read_manifest(&path) {
            Ok(manifest) if is_web_plugin(&manifest) => Some(Ok(path
                .parent()
                .expect("manifest has a package directory")
                .strip_prefix(root)
                .expect("manifest is beneath root")
                .to_owned())),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BuildRecipe {
    id: String,
    directory: PathBuf,
    package: String,
    artifact: String,
    output: PathBuf,
    remote: bool,
}

impl BuildRecipe {
    fn from_manifest(directory: &Path, manifest: &Value) -> anyhow::Result<Option<Self>> {
        let id = manifest.get("name").and_then(Value::as_str);
        let script = manifest.pointer("/scripts/bundle").and_then(Value::as_str);
        if id == Some("@seekdeep-ai/seekdeep-client-runtime") {
            return Ok(Some(Self {
                id: id.expect("matched package name").to_owned(),
                directory: directory.to_owned(),
                package: "seekdeep-client-runtime".to_owned(),
                artifact: "seekdeep_client_runtime".to_owned(),
                output: directory.join("lib"),
                remote: false,
            }));
        }
        if script == Some("cargo xtask remote-artifacts") {
            return Ok(Some(Self {
                id: id.context("Client package has no name")?.to_owned(),
                directory: directory.to_owned(),
                package: "seekdeep-api-remotes-client".to_owned(),
                artifact: "seekdeep_api_remotes_client".to_owned(),
                output: directory.join("lib"),
                remote: true,
            }));
        }
        let Some(script) =
            script.and_then(|script| script.strip_prefix("cargo xtask wasm-package "))
        else {
            anyhow::ensure!(
                !is_web_plugin(manifest),
                "Client package {} has no Rust bundle command",
                directory.display()
            );
            return Ok(None);
        };
        let words = script.split_whitespace().collect::<Vec<_>>();
        anyhow::ensure!(
            words.len() == 8,
            "invalid Rust Client bundle command in {}",
            directory.display()
        );
        let mut values = BTreeMap::new();
        for option in words.chunks_exact(2) {
            anyhow::ensure!(
                matches!(
                    option[0],
                    "--package" | "--artifact" | "--module-id" | "--out-dir"
                ) && values.insert(option[0], option[1]).is_none(),
                "invalid Rust Client bundle option {} in {}",
                option[0],
                directory.display()
            );
        }
        let required = |key| {
            values
                .get(key)
                .copied()
                .with_context(|| format!("missing {key} in {}", directory.display()))
        };
        let id = id.context("Client package has no name")?;
        anyhow::ensure!(
            required("--module-id")? == id,
            "Client bundle module identity differs from package name in {}",
            directory.display()
        );
        let output = PathBuf::from(required("--out-dir")?);
        anyhow::ensure!(
            output == directory.join("lib"),
            "Client bundle output must preserve its package library at {}/lib",
            directory.display()
        );
        Ok(Some(Self {
            id: id.to_owned(),
            directory: directory.to_owned(),
            package: required("--package")?.to_owned(),
            artifact: required("--artifact")?.to_owned(),
            output,
            remote: false,
        }))
    }

    fn arguments(&self) -> Vec<String> {
        if self.remote {
            return vec!["remote-artifacts".to_owned()];
        }
        vec![
            "wasm-package".to_owned(),
            "--package".to_owned(),
            self.package.clone(),
            "--artifact".to_owned(),
            self.artifact.clone(),
            "--module-id".to_owned(),
            self.id.clone(),
            "--out-dir".to_owned(),
            self.output.to_string_lossy().into_owned(),
        ]
    }
}

#[derive(Clone, Debug, Deserialize)]
struct CargoDependency {
    path: Option<PathBuf>,
    kind: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct CargoPackage {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<CargoDependency>,
}

#[derive(Debug, Deserialize)]
struct CargoWorkspace {
    workspace_root: PathBuf,
    packages: Vec<CargoPackage>,
}

impl CargoWorkspace {
    fn read() -> anyhow::Result<Self> {
        let output = std::process::Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn dependency_roots(&self, package: &str) -> anyhow::Result<BTreeSet<PathBuf>> {
        let first = self
            .packages
            .iter()
            .find(|candidate| candidate.name == package)
            .with_context(|| format!("Cargo package {package:?} is not in this workspace"))?;
        let mut pending = vec![first];
        let mut roots = BTreeSet::new();
        while let Some(package) = pending.pop() {
            let root = package
                .manifest_path
                .parent()
                .context("Cargo manifest has no parent")?
                .to_owned();
            if !roots.insert(root) {
                continue;
            }
            for dependency in &package.dependencies {
                if dependency.kind.as_deref() == Some("dev") {
                    continue;
                }
                if let Some(path) = &dependency.path {
                    if let Some(package) = self
                        .packages
                        .iter()
                        .find(|package| package.manifest_path.parent() == Some(path.as_path()))
                    {
                        pending.push(package);
                    } else {
                        // Path dependencies outside the workspace remain observable build inputs.
                        roots.insert(path.clone());
                    }
                }
            }
        }
        Ok(roots)
    }
}

fn discover_recipes(
    workspace: &CargoWorkspace,
    plugins: &[PathBuf],
) -> anyhow::Result<Vec<BuildRecipe>> {
    let root = &workspace.workspace_root;
    let mut manifests = grouped_manifests(root)?;
    for vendor in ["cordis", "loader"] {
        let path = root.join("vendor").join(vendor).join("package.json");
        if path.is_file() {
            manifests.push(path);
        }
    }
    let mut recipes = Vec::new();
    for path in manifests {
        let directory = path
            .parent()
            .context("package has no parent")?
            .strip_prefix(root)?;
        if let Some(recipe) = BuildRecipe::from_manifest(directory, &read_manifest(&path)?)? {
            workspace.dependency_roots(&recipe.package)?;
            recipes.push(recipe);
        }
    }
    for plugin in plugins {
        anyhow::ensure!(
            recipes.iter().any(|recipe| &recipe.directory == plugin),
            "no Rust Client build recipe for {}",
            plugin.display()
        );
    }
    // Cargo owns compilation dependency ordering. Emitting platform packages first also makes
    // their declaration and ESM artifacts available to the subsequent plugin packaging pass.
    recipes.sort_by_key(|recipe| {
        (
            !matches!(
                recipe.id.as_str(),
                "@seekdeep-ai/cordis" | "@seekdeep-ai/cordis-plugin-loader"
            ),
            !recipe.id.contains("ui-slots"),
            !recipe.id.contains("ui-primitives"),
            recipe.id.clone(),
        )
    });
    Ok(recipes)
}

type Snapshot = BTreeMap<PathBuf, u64>;

fn input_entry(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    !matches!(
        name.as_ref(),
        "lib" | "target" | "node_modules" | ".git" | ".cache" | ".dist" | ".generated"
    )
}

fn snapshot(roots: &BTreeSet<PathBuf>) -> anyhow::Result<Snapshot> {
    let mut result = Snapshot::new();
    let mut scanned = Vec::<&Path>::new();
    for root in roots {
        if !root.exists() || scanned.iter().any(|parent| root.starts_with(parent)) {
            continue;
        }
        scanned.push(root);
        for entry in walkdir::WalkDir::new(root)
            .follow_links(true)
            .into_iter()
            .filter_entry(input_entry)
        {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            if matches!(
                entry.path().extension().and_then(std::ffi::OsStr::to_str),
                Some("md" | "yaml")
            ) {
                continue;
            }
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            std::fs::read(entry.path())?.hash(&mut hash);
            result.insert(entry.path().to_owned(), hash.finish());
        }
    }
    Ok(result)
}

fn recipe_roots(
    workspace: &CargoWorkspace,
    recipe: &BuildRecipe,
    recipes: &[BuildRecipe],
) -> anyhow::Result<BTreeSet<PathBuf>> {
    let root = &workspace.workspace_root;
    let mut roots = workspace.dependency_roots(&recipe.package)?;
    let cargo_roots = roots.clone();
    // Embedded browser assets live beside package metadata, including assets of Rust dependencies.
    for other in recipes {
        if workspace
            .packages
            .iter()
            .find(|package| package.name == other.package)
            .and_then(|package| package.manifest_path.parent())
            .is_some_and(|path| cargo_roots.contains(path))
        {
            roots.insert(root.join(&other.directory));
        }
    }
    roots.insert(root.join(&recipe.directory));
    for input in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo",
        "xtask/src",
    ] {
        roots.insert(root.join(input));
    }
    Ok(roots)
}

struct BuildState {
    roots: Vec<BTreeSet<PathBuf>>,
    previous: Vec<Snapshot>,
    pending: BTreeSet<usize>,
    initialized: BTreeSet<usize>,
}

impl BuildState {
    fn new(roots: Vec<BTreeSet<PathBuf>>) -> anyhow::Result<Self> {
        let previous = Self::capture(&roots)?;
        let pending = (0..roots.len()).collect();
        Ok(Self {
            roots,
            previous,
            pending,
            initialized: BTreeSet::new(),
        })
    }

    fn capture(roots: &[BTreeSet<PathBuf>]) -> anyhow::Result<Vec<Snapshot>> {
        let all = roots.iter().flatten().cloned().collect();
        let inputs = snapshot(&all)?;
        Ok(roots
            .iter()
            .map(|roots| {
                inputs
                    .iter()
                    .filter(|(path, _)| roots.iter().any(|root| path.starts_with(root)))
                    .map(|(path, hash)| (path.clone(), *hash))
                    .collect()
            })
            .collect())
    }

    fn refresh(&mut self) -> anyhow::Result<()> {
        for (index, next) in Self::capture(&self.roots)?.into_iter().enumerate() {
            if self.previous[index] != next {
                self.previous[index] = next;
                self.pending.insert(index);
            }
        }
        Ok(())
    }

    fn completed(&mut self, index: usize, success: bool) {
        if success {
            self.initialized.insert(index);
        }
    }

    fn ready(&self) -> bool {
        self.initialized.len() == self.roots.len()
    }
}

async fn shutdown_signal() -> i32 {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM listener registration failed");
        tokio::select! { _ = tokio::signal::ctrl_c() => 130, _ = terminate.recv() => 143 }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        130
    }
}

enum BuildResult {
    Exited(ExitStatus),
    Interrupted(i32),
}

async fn build_recipe(
    executable: &Path,
    root: &Path,
    recipe: &BuildRecipe,
    shutdown: &mut std::pin::Pin<&mut impl std::future::Future<Output = i32>>,
) -> anyhow::Result<BuildResult> {
    let mut command = Command::new(executable);
    command
        .args(recipe.arguments())
        .current_dir(root)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start Client build for {}", recipe.id))?;
    tokio::select! {
        result = child.wait() => Ok(BuildResult::Exited(result?)),
        signal = shutdown.as_mut() => {
            #[cfg(unix)]
            if let Some(id) = child.id() {
                let id = i32::try_from(id).context("Client build process id exceeds platform range")?;
                let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(id), nix::sys::signal::Signal::SIGTERM);
            }
            #[cfg(not(unix))]
            child.start_kill()?;
            if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_err() {
                #[cfg(unix)]
                if let Some(id) = child.id() {
                    let id = i32::try_from(id).context("Client build process id exceeds platform range")?;
                    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(id), nix::sys::signal::Signal::SIGKILL);
                }
                child.kill().await?;
            }
            Ok(BuildResult::Interrupted(signal))
        }
    }
}

pub(super) fn build() -> anyhow::Result<()> {
    let workspace = CargoWorkspace::read()?;
    let plugins = discover_plugin_dirs(&workspace.workspace_root)?;
    anyhow::ensure!(
        !plugins.is_empty(),
        "dev-web: no seekdeep.client (platform \"web\") packages found under packages/"
    );
    let recipes = discover_recipes(&workspace, &plugins)?;
    let executable = std::env::current_exe()?;
    let interrupted = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut shutdown = std::pin::pin!(shutdown_signal());
            for recipe in &recipes {
                let status = build_recipe(
                    &executable,
                    &workspace.workspace_root,
                    recipe,
                    &mut shutdown,
                )
                .await?;
                let status = match status {
                    BuildResult::Exited(status) => status,
                    BuildResult::Interrupted(signal) => return Ok(Some(signal)),
                };
                anyhow::ensure!(
                    status.success(),
                    "Client build failed for {} ({status})",
                    recipe.id
                );
            }
            println!(
                "built {} Rust Client package artifacts ({} browser plugins)",
                recipes.len(),
                plugins.len()
            );
            Ok::<_, anyhow::Error>(None)
        })?;
    if let Some(signal) = interrupted {
        std::process::exit(signal);
    }
    Ok(())
}

pub(super) fn dev(args: &[String]) -> anyhow::Result<()> {
    let workspace = CargoWorkspace::read()?;
    let plugins = discover_plugin_dirs(&workspace.workspace_root)?;
    anyhow::ensure!(
        !plugins.is_empty(),
        "dev-web: no seekdeep.client (platform \"web\") packages found under packages/"
    );
    let mode = parse_watch_mode(args)?;
    let recipes = discover_recipes(&workspace, &plugins)?;
    let roots = recipes
        .iter()
        .map(|recipe| recipe_roots(&workspace, recipe, &recipes))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let executable = std::env::current_exe()?;
    let signal = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(watch(
            &workspace.workspace_root,
            &executable,
            &plugins,
            &recipes,
            roots,
            mode,
        ))?;
    std::process::exit(signal)
}

async fn watch(
    root: &Path,
    executable: &Path,
    plugins: &[PathBuf],
    recipes: &[BuildRecipe],
    roots: Vec<BTreeSet<PathBuf>>,
    mode: WatchMode,
) -> anyhow::Result<i32> {
    let (changes, mut changed) = tokio::sync::mpsc::unbounded_channel();
    let mut native = if mode == WatchMode::Native {
        Some(notify::recommended_watcher(
            move |event: notify::Result<notify::Event>| {
                let _ = changes.send(event);
            },
        )?)
    } else {
        None
    };
    if let Some(watcher) = native.as_mut() {
        let mut watched = BTreeSet::<PathBuf>::new();
        for path in roots
            .iter()
            .flat_map(BTreeSet::iter)
            .filter(|path| path.exists())
        {
            if watched.iter().any(|parent| path.starts_with(parent)) {
                continue;
            }
            watcher.watch(path, notify::RecursiveMode::Recursive)?;
            watched.insert(path.clone());
        }
    }
    let mut state = BuildState::new(roots)?;
    let mut announced = false;
    let mut shutdown = std::pin::pin!(shutdown_signal());
    loop {
        while let Some(index) = state.pending.pop_first() {
            let recipe = &recipes[index];
            let status = match build_recipe(executable, root, recipe, &mut shutdown).await? {
                BuildResult::Exited(status) => status,
                BuildResult::Interrupted(signal) => return Ok(signal),
            };
            if !status.success() {
                eprintln!(
                    "dev-web: Client build failed for {} ({status}); waiting for changes",
                    recipe.id
                );
            }
            state.completed(index, status.success());
        }
        // Inputs are sampled before the initial build and after every batch: an edit that lands
        // while Cargo runs is retained, including same-length rewrites with preserved timestamps.
        state.refresh()?;
        if !state.pending.is_empty() {
            continue;
        }
        if state.ready() && !announced {
            let suffix = match &mode {
                WatchMode::Native => String::new(),
                WatchMode::Poll { display, .. } => format!(" (polling {display}ms)"),
            };
            println!(
                "dev-web: watching {} seekdeep.client plugin packages{suffix}:\n  {}",
                plugins.len(),
                plugins
                    .iter()
                    .map(|path| path
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/"))
                    .collect::<Vec<_>>()
                    .join("\n  ")
            );
            announced = true;
        }
        match &mode {
            WatchMode::Poll { interval, .. } => tokio::select! {
                signal = shutdown.as_mut() => return Ok(signal),
                () = tokio::time::sleep(*interval) => {},
            },
            WatchMode::Native => tokio::select! {
                signal = shutdown.as_mut() => return Ok(signal),
                event = changed.recv() => {
                    match event {
                        Some(Ok(_)) => {},
                        Some(Err(error)) => return Err(error.into()),
                        None => anyhow::bail!("dev-web: native watcher stopped unexpectedly"),
                    }
                    while let Ok(event) = changed.try_recv() { event?; }
                },
            },
        }
        state.refresh()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_arguments_match_javascript_numeric_and_duplicate_semantics() {
        let parse = |args: &[&str]| {
            parse_watch_mode(
                &args
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(parse(&[]).unwrap(), WatchMode::Native);
        for (input, interval) in [
            ("--poll", 500),
            ("--poll=50", 50),
            ("--poll=5e2", 500),
            ("--poll=0x20", 32),
            ("--poll=0x20=trailing", 32),
            ("--poll= 5 ", 5),
            ("--poll=2147483648", 1),
        ] {
            assert_eq!(
                parse(&[input]).unwrap(),
                WatchMode::Poll {
                    interval: Duration::from_millis(interval),
                    display: if input == "--poll=2147483648" {
                        "2147483648".to_owned()
                    } else {
                        interval.to_string()
                    }
                }
            );
        }
        // The source compares argument values, so repeated identical --poll flags are accepted.
        assert!(parse(&["--poll", "--poll"]).is_ok());
        for args in [
            vec!["--poll="],
            vec!["--poll=0"],
            vec!["--poll=-1"],
            vec!["--poll=1.5"],
            vec!["--poll=NaN"],
            vec!["--poll=Infinity"],
            vec!["--poll", "--poll=500"],
            vec!["--unknown"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        assert_eq!(
            parse(&["--poll=\"invalid\"\n"]).unwrap_err().to_string(),
            "dev-web: invalid --poll interval \"--poll=\"invalid\"\n\""
        );
        for (number, expected) in [
            ("0x1000000000000081", "1152921504606847200"),
            ("0x1000000000000080", "1152921504606847000"),
            ("0x1000000000000180", "1152921504606847500"),
        ] {
            let display = ryu_js::Buffer::new().format(js_number(number)).to_owned();
            assert_eq!(display, expected);
        }
    }

    #[test]
    fn poll_numbers_match_javascript_binary64_rounding() {
        let mut values = [
            "",
            "-0",
            "0x",
            "0o8",
            "0b2",
            "0x1000000000000081",
            "\u{feff}12\u{2028}",
            "\u{0085}12",
            "Infinity",
            "inf",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        for (prefix, digit) in [("0x", "f"), ("0o", "7"), ("0b", "1")] {
            for width in [1, 13, 14, 16, 17, 25, 128, 255, 256, 257, 1024] {
                for tail in ["0", "1", "10", "11"] {
                    values.push(format!("{prefix}1{}{tail}", "0".repeat(width)));
                }
                values.push(format!("{prefix}{}1", digit.repeat(width)));
            }
        }
        values.push(format!("0x1{}g", "0".repeat(1024)));
        let script = r"process.stdout.write(JSON.stringify(process.argv.slice(1).map(value => { const number = Number(value); if (Number.isNaN(number)) return 'nan'; const view = new DataView(new ArrayBuffer(8)); view.setFloat64(0, number); return view.getBigUint64(0).toString(16).padStart(16, '0'); })));";
        let output = std::process::Command::new("node")
            .args(["-e", script, "--"])
            .args(&values)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(expected.len(), values.len());
        for (value, expected) in values.iter().zip(expected) {
            let number = js_number(value);
            let actual = if number.is_nan() {
                "nan".to_owned()
            } else {
                format!("{:016x}", number.to_bits())
            };
            assert_eq!(actual, expected, "{value}");
        }
    }

    #[test]
    fn plugin_discovery_reads_exact_group_depth_and_sibling_roles() {
        let root = tempfile::tempdir().unwrap();
        for (path, value) in [
            (
                "packages/client/current/package.json",
                json!({"seekdeep":{"bundle":{},"client":{"platform":"web"},"profile":{}}}),
            ),
            (
                "packages/host/dual/package.json",
                json!({"seekdeep":{"client":{"platform":"web"}}}),
            ),
            (
                "packages/client/native/package.json",
                json!({"seekdeep":{"client":{"platform":"native"}}}),
            ),
            (
                "packages/client/nested/child/package.json",
                json!({"seekdeep":{"client":{"platform":"web"}}}),
            ),
        ] {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, value.to_string()).unwrap();
        }
        assert_eq!(
            discover_plugin_dirs(root.path()).unwrap(),
            [
                PathBuf::from("packages/client/current"),
                PathBuf::from("packages/host/dual")
            ]
        );
        std::fs::write(root.path().join("packages/client/native/package.json"), "{").unwrap();
        assert!(discover_plugin_dirs(root.path()).is_err());
    }

    #[test]
    fn watch_retains_build_time_edits_and_excludes_emitted_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let crate_root = root.path().join("crates/widget");
        let shared_root = root.path().join("crates/shared");
        let assets = root.path().join("packages/client/widget");
        for path in [&crate_root, &shared_root, &assets] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(shared_root.join("lib.rs"), "pub const VERSION: u32 = 1;").unwrap();
        std::fs::write(assets.join("style.module.css"), ".root {color:red}").unwrap();
        let mut state = BuildState::new(vec![BTreeSet::from([
            crate_root,
            shared_root.clone(),
            assets.clone(),
        ])])
        .unwrap();
        assert_eq!(state.pending.pop_first(), Some(0));
        state.completed(0, false);
        assert!(!state.ready());
        std::fs::write(shared_root.join("lib.rs"), "pub const VERSION: u32 = 2;").unwrap();
        state.refresh().unwrap();
        assert_eq!(state.pending.pop_first(), Some(0));
        state.completed(0, true);
        assert!(state.ready());
        std::fs::create_dir_all(assets.join("lib")).unwrap();
        std::fs::write(assets.join("lib/client.js"), "generated").unwrap();
        state.refresh().unwrap();
        assert!(state.pending.is_empty());
        std::fs::write(assets.join("style.module.css"), ".root {color:tan}").unwrap();
        state.refresh().unwrap();
        assert_eq!(state.pending.pop_first(), Some(0));
        std::fs::remove_file(shared_root.join("lib.rs")).unwrap();
        state.refresh().unwrap();
        assert_eq!(state.pending.pop_first(), Some(0));
    }

    #[test]
    fn cargo_dependency_closure_includes_build_inputs_and_avoids_dev_cycles() {
        let workspace: CargoWorkspace = serde_json::from_value(json!({
            "workspace_root":"/fixture",
            "packages":[
                {"name":"plugin","manifest_path":"/fixture/plugin/Cargo.toml","dependencies":[{"path":"/fixture/shared","kind":null},{"path":"/fixture/test","kind":"dev"}]},
                {"name":"shared","manifest_path":"/fixture/shared/Cargo.toml","dependencies":[{"path":"/fixture/build","kind":"build"}]},
                {"name":"build","manifest_path":"/fixture/build/Cargo.toml","dependencies":[{"path":"/fixture/plugin","kind":null}]}
            ]
        })).unwrap();
        assert_eq!(
            workspace.dependency_roots("plugin").unwrap(),
            BTreeSet::from([
                PathBuf::from("/fixture/plugin"),
                PathBuf::from("/fixture/shared"),
                PathBuf::from("/fixture/build")
            ])
        );
        assert!(workspace.dependency_roots("missing").is_err());
    }

    use serde_json::json;
}
