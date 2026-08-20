//! Instruction-file discovery and bounded, abort-aware provider reads.

#![allow(clippy::single_match_else)]

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use path_clean::PathClean;
use seekdeep_fs::FileSystem;
use seekdeep_fs::types::{FsInfo, FsKind, FsTarget, FsVersion};
use seekdeep_llm::AbortSignal;
use seekdeep_util::home_paths::seekdeep_home_display;

use crate::config::{Config, ResolvedConfig, ResolvedDiscoveryConfig, resolve_discovery_config};
use crate::digest::trimmed_instruction_digest;
pub use crate::render::{InstructionFile, LoadedInstructionFile};
use crate::render::{
    RenderedWorkspaceContext, USER_GLOBAL_DIRECTORY, USER_GLOBAL_FILE, decode_scope_key,
    render_workspace_instruction_set,
};

/// Provider metadata for a probed scope candidate before its content is read.
#[derive(Clone, Debug)]
pub struct ProbedInstructionFile {
    /// Absolute filesystem path.
    pub absolute_path: String,
    /// Project-relative or user-global display path.
    pub display_path: String,
    /// Resolved provider target.
    pub target: FsTarget,
    /// Provider freshness token.
    pub version: FsVersion,
    /// Byte size, when the provider reported it.
    pub size: Option<u64>,
}

/// Rendered baseline plus the successfully read and byte-budget-retained files.
#[derive(Clone, Debug)]
pub struct RenderedInstructionSet {
    /// Bounded baseline rendering.
    pub rendered: RenderedWorkspaceContext,
    /// Successfully read candidates before content deduplication and byte budgeting.
    pub observed: Vec<LoadedInstructionFile>,
    /// Candidates retained by content deduplication and byte budgeting.
    pub included: Vec<LoadedInstructionFile>,
}

/// Tri-state scope probe distinguishing confirmed absence from provider failure.
#[derive(Clone, Debug)]
pub enum ScopeInstructionProbe {
    /// Present with metadata.
    Present {
        /// Probed candidate metadata.
        file: ProbedInstructionFile,
    },
    /// Confirmed absent.
    Absent,
    /// Temporarily unavailable.
    Unavailable,
}

#[derive(Clone, Debug)]
struct DiscoveredInstructionFile {
    absolute_path: String,
    display_path: String,
    target: Option<FsTarget>,
    size: Option<u64>,
    version: Option<FsVersion>,
}

#[derive(Clone, Debug)]
struct StatFileInfo {
    target: Option<FsTarget>,
    size: Option<u64>,
    version: Option<FsVersion>,
}

#[derive(Clone, Debug)]
enum StatFileProbe {
    Present { info: StatFileInfo },
    Absent,
    Unavailable,
}

fn ensure_not_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        anyhow::bail!("instruction read aborted");
    }
    Ok(())
}

fn join_path(dir: &str, name: &str) -> String {
    Path::new(dir).join(name).to_string_lossy().into_owned()
}

fn resolve_path(path: &str) -> String {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    absolute.clean().to_string_lossy().into_owned()
}

fn dirname_path(path: &str) -> String {
    match Path::new(path).parent() {
        Some(parent) => {
            let parent = parent.to_string_lossy();
            if parent.is_empty() {
                ".".to_owned()
            } else {
                parent.into_owned()
            }
        }
        None => path.to_owned(),
    }
}

fn relative_path(from: &str, to: &str) -> String {
    let from = Path::new(from).components().collect::<Vec<_>>();
    let to = Path::new(to).components().collect::<Vec<_>>();
    let mut common = 0;
    while common < from.len() && common < to.len() && from[common] == to[common] {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".to_owned());
    }
    for component in &to[common..] {
        if let std::path::Component::Normal(segment) = component {
            parts.push(segment.to_string_lossy().into_owned());
        }
    }
    parts.join(std::path::MAIN_SEPARATOR_STR)
}

async fn node_stat_file(path: &str, signal: Option<&AbortSignal>) -> anyhow::Result<StatFileProbe> {
    ensure_not_aborted(signal)?;
    let info = tokio::fs::metadata(path).await;
    ensure_not_aborted(signal)?;
    match info {
        Ok(meta) if meta.is_file() => Ok(StatFileProbe::Present {
            info: StatFileInfo {
                target: None,
                size: Some(meta.len()),
                version: None,
            },
        }),
        Ok(_) => Ok(StatFileProbe::Absent),
        Err(error) => {
            ensure_not_aborted(signal)?;
            if error.kind() == ErrorKind::NotFound || error.kind() == ErrorKind::NotADirectory {
                Ok(StatFileProbe::Absent)
            } else {
                Ok(StatFileProbe::Unavailable)
            }
        }
    }
}

async fn fs_stat_file(
    path: &str,
    file_system: &dyn FileSystem,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<StatFileProbe> {
    match file_system.resolve(path, None, signal).await {
        Ok(target) => {
            ensure_not_aborted(signal)?;
            let info = file_system.stat(&target, signal).await?;
            ensure_not_aborted(signal)?;
            match info {
                Some(info) if info.kind == FsKind::File => Ok(StatFileProbe::Present {
                    info: StatFileInfo {
                        target: Some(target),
                        version: Some(info.version),
                        size: info.size,
                    },
                }),
                _ => Ok(StatFileProbe::Absent),
            }
        }
        Err(_) => {
            ensure_not_aborted(signal)?;
            Ok(StatFileProbe::Unavailable)
        }
    }
}

async fn stat_file(
    path: &str,
    file_system: Option<&dyn FileSystem>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<StatFileProbe> {
    match file_system {
        Some(file_system) => fs_stat_file(path, file_system, signal).await,
        None => node_stat_file(path, signal).await,
    }
}

async fn exists_as_marker(
    path: &str,
    file_system: Option<&dyn FileSystem>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<bool> {
    match file_system {
        Some(file_system) => match file_system.resolve(path, None, signal).await {
            Ok(target) => Ok(file_system.stat(&target, signal).await?.is_some()),
            Err(_) => {
                ensure_not_aborted(signal)?;
                Ok(false)
            }
        },
        None => match tokio::fs::metadata(path).await {
            Ok(_) => {
                ensure_not_aborted(signal)?;
                Ok(true)
            }
            Err(_) => {
                ensure_not_aborted(signal)?;
                Ok(false)
            }
        },
    }
}

/// Walks upward to the first directory containing a configured root marker.
///
/// # Errors
///
/// Returns an aborted or provider probe failure.
pub async fn find_project_root(
    cwd: &str,
    markers: &[String],
    file_system: Option<&dyn FileSystem>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<String> {
    let mut current = resolve_path(cwd);
    loop {
        for marker in markers {
            if exists_as_marker(&join_path(&current, marker), file_system, signal).await? {
                return Ok(current);
            }
        }
        let parent = dirname_path(&current);
        if parent == current {
            return Ok(resolve_path(cwd));
        }
        current = parent;
    }
}

/// Builds the inclusive root-to-cwd directory chain.
#[must_use]
pub fn ancestor_chain(root: &str, cwd: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = resolve_path(cwd);
    let resolved_root = resolve_path(root);
    while current != resolved_root {
        chain.push(current.clone());
        let parent = dirname_path(&current);
        if parent == current {
            break;
        }
        current = parent;
    }
    chain.push(resolved_root);
    chain.reverse();
    chain
}

/// Finds descendant directories crossed between a cwd and a touched file.
#[must_use]
pub fn descendant_dirs_between(root: &str, touched_path: &str) -> Vec<String> {
    let resolved_root = resolve_path(root);
    let target_path = if Path::new(touched_path).is_absolute() {
        resolve_path(touched_path)
    } else {
        resolve_path(&join_path(&resolved_root, touched_path))
    };
    let target_dir = dirname_path(&target_path);
    let rel = relative_path(&resolved_root, &target_dir);
    if rel.is_empty() || rel.starts_with("..") || Path::new(&rel).is_absolute() {
        return Vec::new();
    }
    let mut chain = ancestor_chain(&resolved_root, &target_dir);
    if chain.len() > 1 {
        chain.remove(0);
    }
    chain
}

/// Converts an absolute instruction path to its project-root-relative display form.
#[must_use]
pub fn relative_display(root: &str, path: &str) -> String {
    relative_path(root, path)
}

async fn all_existing_instruction_files(
    dir: &str,
    root: &str,
    instruction_file_candidates: &[String],
    file_system: Option<&dyn FileSystem>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Vec<DiscoveredInstructionFile>> {
    let mut found = Vec::new();
    for candidate in instruction_file_candidates {
        let path = join_path(dir, candidate);
        match stat_file(&path, file_system, signal).await? {
            StatFileProbe::Present { info } => {
                let display_path = relative_display(root, &path);
                found.push(DiscoveredInstructionFile {
                    absolute_path: path,
                    display_path,
                    target: info.target,
                    size: info.size,
                    version: info.version,
                });
            }
            StatFileProbe::Absent | StatFileProbe::Unavailable => {}
        }
    }
    Ok(found)
}

async fn discover_instruction_files(
    config: &ResolvedDiscoveryConfig,
    options: &DiscoverOptions,
    file_system: Option<&dyn FileSystem>,
) -> anyhow::Result<Vec<DiscoveredInstructionFile>> {
    let mut files: Vec<DiscoveredInstructionFile> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add_file = |file: DiscoveredInstructionFile| {
        if seen.insert(file.absolute_path.clone()) {
            files.push(file);
        }
    };

    let user_global = join_path(&config.dsh_home, USER_GLOBAL_FILE);
    match stat_file(&user_global, file_system, options.signal.as_ref()).await? {
        StatFileProbe::Present { info } => {
            add_file(DiscoveredInstructionFile {
                absolute_path: user_global,
                display_path: user_global_display_path(&config.dsh_home),
                target: info.target,
                size: info.size,
                version: info.version,
            });
        }
        StatFileProbe::Absent | StatFileProbe::Unavailable => {}
    }

    let cwd = resolve_path(&options.cwd);
    let project_root = options.project_root.clone().unwrap_or(
        find_project_root(
            &cwd,
            &config.project_root_markers,
            file_system,
            options.signal.as_ref(),
        )
        .await?,
    );
    for dir in ancestor_chain(&project_root, &cwd) {
        for candidates in [
            &config.instruction_file_candidates,
            &config.local_instruction_file_candidates,
        ] {
            for file in all_existing_instruction_files(
                &dir,
                &project_root,
                candidates,
                file_system,
                options.signal.as_ref(),
            )
            .await?
            {
                add_file(file);
            }
        }
    }
    Ok(files)
}

/// Discovery options shared by baseline and reconciliation reads.
#[derive(Clone, Debug, Default)]
pub struct DiscoverOptions {
    /// Absolute session working directory.
    pub cwd: String,
    /// Optional harness home override.
    pub dsh_home: Option<String>,
    /// Optional project root markers.
    pub project_root_markers: Option<Vec<String>>,
    /// Optional instruction candidates.
    pub instruction_file_candidates: Option<Vec<String>>,
    /// Optional local overlay candidates.
    pub local_instruction_file_candidates: Option<Vec<String>>,
    /// Optional pre-resolved project root.
    pub project_root: Option<String>,
    /// Cancellation signal.
    pub signal: Option<AbortSignal>,
}

/// Loads and renders the baseline instruction chain.
///
/// # Errors
///
/// Returns an aborted or provider failure.
pub async fn load_baseline_instructions(
    options: &DiscoverOptions,
    max_bytes: u64,
    max_source_bytes: u64,
    replace_previous_baseline: Option<bool>,
    file_system: Option<&dyn FileSystem>,
) -> anyhow::Result<Option<RenderedWorkspaceContext>> {
    Ok(load_baseline_instruction_set(
        options,
        max_bytes,
        max_source_bytes,
        replace_previous_baseline,
        file_system,
    )
    .await?
    .map(|set| set.rendered))
}

/// Loads a baseline together with the files retained after rendering.
///
/// # Errors
///
/// Returns an aborted or provider failure.
#[allow(clippy::cast_possible_truncation)]
pub async fn load_baseline_instruction_set(
    options: &DiscoverOptions,
    max_bytes: u64,
    max_source_bytes: u64,
    replace_previous_baseline: Option<bool>,
    file_system: Option<&dyn FileSystem>,
) -> anyhow::Result<Option<RenderedInstructionSet>> {
    if max_bytes == 0 || max_source_bytes == 0 {
        return Ok(None);
    }
    let config = resolve_discovery_config(&Config {
        dsh_home: options.dsh_home.clone(),
        project_root_markers: options.project_root_markers.clone(),
        max_bytes,
        max_source_bytes: Some(max_source_bytes),
        instruction_file_candidates: options.instruction_file_candidates.clone(),
        local_instruction_file_candidates: options.local_instruction_file_candidates.clone(),
    })?;
    let discovered = discover_instruction_files(&config, options, file_system).await?;
    let mut loaded: Vec<LoadedInstructionFile> = Vec::new();
    for file in &discovered {
        let content =
            read_bounded(file, max_source_bytes, file_system, options.signal.as_ref()).await?;
        if let Some(content) = content {
            loaded.push(LoadedInstructionFile {
                absolute_path: file.absolute_path.clone(),
                display_path: file.display_path.clone(),
                content,
                version: file.version.clone(),
            });
        }
    }
    let deduped = dedup_instruction_files_by_directory(loaded.clone());
    if deduped.is_empty() {
        if replace_previous_baseline != Some(true) {
            return Ok(None);
        }
        let (rendered, included) = render_workspace_instruction_set(&[], max_bytes as usize, true);
        return Ok(Some(RenderedInstructionSet {
            rendered,
            observed: Vec::new(),
            included,
        }));
    }
    let (rendered, included) = render_workspace_instruction_set(
        &deduped,
        max_bytes as usize,
        replace_previous_baseline == Some(true),
    );
    Ok(Some(RenderedInstructionSet {
        rendered,
        observed: loaded,
        included,
    }))
}

async fn node_text_chunks(path: &str, signal: Option<&AbortSignal>) -> anyhow::Result<Vec<String>> {
    ensure_not_aborted(signal)?;
    let content = tokio::fs::read_to_string(path).await?;
    ensure_not_aborted(signal)?;
    Ok(vec![content])
}

async fn read_bounded(
    file: &DiscoveredInstructionFile,
    max_source_bytes: u64,
    file_system: Option<&dyn FileSystem>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Option<String>> {
    ensure_not_aborted(signal)?;
    if file.size.is_some_and(|size| size > max_source_bytes) {
        return Ok(None);
    }
    let outcome: anyhow::Result<Option<String>> = async {
        let mut parts: Vec<String> = Vec::new();
        let mut bytes = 0u64;
        match (file_system, &file.target) {
            (Some(file_system), Some(target)) => {
                let mut stream = file_system.stream_text(target, signal).await?;
                while let Some(chunk) = stream.next().await {
                    ensure_not_aborted(signal)?;
                    bytes += chunk.len() as u64;
                    if bytes > max_source_bytes {
                        return Ok(None);
                    }
                    parts.push(chunk);
                }
            }
            _ => {
                let chunks = node_text_chunks(&file.absolute_path, signal).await?;
                for chunk in chunks {
                    ensure_not_aborted(signal)?;
                    bytes += chunk.len() as u64;
                    if bytes > max_source_bytes {
                        return Ok(None);
                    }
                    parts.push(chunk);
                }
            }
        }
        ensure_not_aborted(signal)?;
        Ok(Some(parts.join("")))
    }
    .await;
    match outcome {
        Ok(result) => Ok(result),
        Err(_) => {
            ensure_not_aborted(signal)?;
            Ok(None)
        }
    }
}

/// Drops later candidates whose trimmed content duplicates an earlier sibling in
/// the same directory.
#[must_use]
pub fn dedup_instruction_files_by_directory(
    files: Vec<LoadedInstructionFile>,
) -> Vec<LoadedInstructionFile> {
    let mut kept_digests_by_dir: std::collections::HashMap<
        String,
        std::collections::HashSet<String>,
    > = std::collections::HashMap::new();
    let mut kept: Vec<LoadedInstructionFile> = Vec::new();
    for file in files {
        let dir = dirname_path(&file.display_path);
        let digests = kept_digests_by_dir.entry(dir).or_default();
        let digest = trimmed_instruction_digest(&file.content);
        if !digests.insert(digest) {
            continue;
        }
        kept.push(file);
    }
    kept
}

/// Probes the current provider metadata for one per-candidate instruction scope.
///
/// # Errors
///
/// Returns an aborted or provider probe failure.
pub async fn probe_scope_instruction(
    scope: &str,
    project_root: &str,
    resolved: &ResolvedConfig,
    file_system: &dyn FileSystem,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<ScopeInstructionProbe> {
    let (directory, candidate_name) = decode_scope_key(scope);
    let dir = if directory == USER_GLOBAL_DIRECTORY {
        resolved.dsh_home.clone()
    } else if directory == "." {
        project_root.to_owned()
    } else {
        join_path(project_root, &directory)
    };
    let absolute_path = join_path(&dir, &candidate_name);
    let (target, info): (FsTarget, Option<FsInfo>) =
        match file_system.resolve(&absolute_path, None, signal).await {
            Ok(target) => {
                let info = file_system.stat(&target, signal).await?;
                (target, info)
            }
            Err(_) => {
                ensure_not_aborted(signal)?;
                return Ok(ScopeInstructionProbe::Unavailable);
            }
        };
    let Some(info) = info else {
        return Ok(ScopeInstructionProbe::Absent);
    };
    if info.kind != FsKind::File {
        return Ok(ScopeInstructionProbe::Absent);
    }
    let display_path = if directory == USER_GLOBAL_DIRECTORY {
        user_global_display_path(&resolved.dsh_home)
    } else {
        relative_display(project_root, &absolute_path)
    };
    Ok(ScopeInstructionProbe::Present {
        file: ProbedInstructionFile {
            absolute_path,
            display_path,
            target,
            version: info.version,
            size: info.size,
        },
    })
}

/// Reads one already-probed scope candidate under the configured source cap.
///
/// # Errors
///
/// Returns an aborted or provider streaming failure.
pub async fn read_scope_instruction(
    file: &ProbedInstructionFile,
    max_source_bytes: u64,
    file_system: &dyn FileSystem,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Option<LoadedInstructionFile>> {
    let discovered = DiscoveredInstructionFile {
        absolute_path: file.absolute_path.clone(),
        display_path: file.display_path.clone(),
        target: Some(file.target.clone()),
        size: file.size,
        version: Some(file.version.clone()),
    };
    let content = read_bounded(&discovered, max_source_bytes, Some(file_system), signal).await?;
    Ok(content.map(|content| LoadedInstructionFile {
        absolute_path: file.absolute_path.clone(),
        display_path: file.display_path.clone(),
        content,
        version: Some(file.version.clone()),
    }))
}

/// Discovers host-visible user-global and root-to-cwd instruction candidates.
///
/// # Errors
///
/// Returns an aborted or provider probe failure.
pub async fn discover_baseline_instruction_files(
    options: &DiscoverOptions,
    file_system: Option<&dyn FileSystem>,
) -> anyhow::Result<Vec<InstructionFile>> {
    let config = resolve_discovery_config(&Config {
        dsh_home: options.dsh_home.clone(),
        project_root_markers: options.project_root_markers.clone(),
        max_bytes: 0,
        max_source_bytes: None,
        instruction_file_candidates: options.instruction_file_candidates.clone(),
        local_instruction_file_candidates: options.local_instruction_file_candidates.clone(),
    })?;
    Ok(discover_instruction_files(&config, options, file_system)
        .await?
        .into_iter()
        .map(|file| InstructionFile {
            absolute_path: file.absolute_path,
            display_path: file.display_path,
        })
        .collect())
}

fn user_global_display_path(dsh_home: &str) -> String {
    format!(
        "{}/{}",
        seekdeep_home_display(Path::new(dsh_home)).unwrap_or("$SEEKDEEP_HOME"),
        USER_GLOBAL_FILE
    )
}
