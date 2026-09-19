//! Bounded one-level Host filesystem browsing and child creation.

use std::{
    cmp::Ordering,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
};

use futures::future::BoxFuture;
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences, options::CollatorOptions};
use icu_locale::Locale;
use seekdeep_cordis::Plugin;
use seekdeep_host_directory_picker::{
    DirectoryEntry, DirectoryListing, DirectoryPickerCapability, DirectoryPickerError,
    DirectoryPickerErrorCode, DirectoryPickerFailure, DirectoryPickerService,
};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};

/// Stable plugin name.
pub const NAME: &str = "host-directory-picker-browse";
/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "host-directory-picker-browse-invariant";
const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-host-directory-picker-browse";

/// Complete listing bound.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowseConfig {
    /// Maximum child-directory rows returned by one call.
    pub max_entries: usize,
    /// Schemastery object schemas preserve undeclared keys in non-strict mode.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for BrowseConfig {
    fn default() -> Self {
        Self {
            max_entries: 1000,
            extra: serde_json::Map::new(),
        }
    }
}

/// One streamed listing candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListingCandidate {
    /// Base name.
    pub name: String,
    /// Directory without a metadata probe.
    pub is_directory: bool,
    /// Symlink whose target must be probed.
    pub is_symbolic_link: bool,
}

/// Deterministic platform spelling for fully-qualified path checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathPlatform {
    /// POSIX absolute paths.
    Posix,
    /// Drive-qualified or complete UNC paths.
    Windows,
}

/// Tests whether a path identifies one location independently of process state.
#[must_use]
pub fn fully_qualified(path: &str, platform: PathPlatform) -> bool {
    match platform {
        PathPlatform::Posix => path.starts_with('/'),
        PathPlatform::Windows => {
            let bytes = path.as_bytes();
            let drive = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\');
            let normalized = path.replace('\\', "/");
            let unc = normalized.strip_prefix("//").is_some_and(|rest| {
                let Some(server_end) = rest.find('/') else {
                    return false;
                };
                server_end > 0 && !rest[server_end..].trim_start_matches('/').is_empty()
            });
            drive || unc
        }
    }
}

/// Inserts one candidate into a name-sorted bounded head window.
///
/// Returns whether a candidate was evicted or rejected beyond the tail.
///
/// # Panics
///
/// Panics when `keep` is zero; validated browse configuration never creates
/// such a window, matching the source helper's invalid-call failure.
pub fn bounded_insert(
    window: &mut Vec<ListingCandidate>,
    candidate: ListingCandidate,
    keep: usize,
) -> bool {
    assert!(
        keep >= 1,
        "bounded listing window must retain at least one candidate"
    );
    if window.len() == keep
        && window
            .last()
            .is_some_and(|tail| locale_compare(&candidate.name, &tail.name) != Ordering::Less)
    {
        return true;
    }
    let index = window.partition_point(|existing| {
        locale_compare(&candidate.name, &existing.name) != Ordering::Less
    });
    window.insert(index, candidate);
    if window.len() <= keep {
        false
    } else {
        window.pop();
        true
    }
}

/// Races an owned operation against caller cancellation while draining the loser.
///
/// # Errors
///
/// Returns the operation failure or the caller's exact first cancellation reason.
pub async fn race_abort<T>(
    operation: BoxFuture<'static, anyhow::Result<T>>,
    signal: Option<AbortSignal>,
) -> anyhow::Result<T>
where
    T: Send + 'static,
{
    let Some(signal) = signal else {
        return operation.await;
    };
    if signal.is_aborted() {
        tokio::spawn(async move {
            let _ = operation.await;
        });
        return Err(abort_error(&signal));
    }
    let mut operation = tokio::spawn(operation);
    tokio::select! {
        biased;
        () = signal.cancelled() => {
            tokio::spawn(async move { let _ = operation.await; });
            Err(abort_error(&signal))
        }
        result = &mut operation => result?,
    }
}

/// Lists one fully-qualified level, or the Host home when omitted.
///
/// # Errors
///
/// Returns caller cancellation unchanged and other failures as
/// `directory-unreadable`.
pub async fn list_directory(
    path: Option<String>,
    signal: AbortSignal,
    max_entries: usize,
) -> Result<DirectoryListing, DirectoryPickerFailure> {
    if let Some(path) = &path
        && !fully_qualified(path, current_platform())
    {
        return Err(DirectoryPickerError::new(
            DirectoryPickerErrorCode::DirectoryUnreadable,
            path,
            format!("cannot list \"{path}\": not a fully qualified path"),
        )
        .into());
    }
    if signal.is_aborted() {
        return Err(DirectoryPickerFailure::Internal(abort_error(&signal)));
    }
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Host home directory is unavailable"))?;
    let unresolved = path.map_or_else(|| home.clone(), PathBuf::from);
    let target = normalize_absolute(&unresolved);
    let keep = max_entries.saturating_add(1);
    let mut window = Vec::new();
    let mut evicted = false;
    let mut directory = tokio::select! {
        biased;
        () = signal.cancelled() => return Err(DirectoryPickerFailure::Internal(abort_error(&signal))),
        opened = tokio::fs::read_dir(&target) => opened.map_err(|error| unreadable(&target, &error))?,
    };
    loop {
        let entry = tokio::select! {
            biased;
            () = signal.cancelled() => return Err(DirectoryPickerFailure::Internal(abort_error(&signal))),
            entry = directory.next_entry() => entry.map_err(|error| unreadable(&target, &error))?,
        };
        let Some(entry) = entry else { break };
        let file_type = tokio::select! {
            biased;
            () = signal.cancelled() => return Err(DirectoryPickerFailure::Internal(abort_error(&signal))),
            file_type = entry.file_type() => file_type.map_err(|error| unreadable(&target, &error))?,
        };
        if !file_type.is_dir() && !file_type.is_symlink() {
            continue;
        }
        evicted |= bounded_insert(
            &mut window,
            ListingCandidate {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_directory: file_type.is_dir(),
                is_symbolic_link: file_type.is_symlink(),
            },
            keep,
        );
    }
    let mut entries = Vec::new();
    let mut truncated = evicted;
    for candidate in window {
        if signal.is_aborted() {
            return Err(DirectoryPickerFailure::Internal(abort_error(&signal)));
        }
        let path = target.join(&candidate.name);
        let enterable = if candidate.is_directory {
            true
        } else if candidate.is_symbolic_link {
            match tokio::select! {
                biased;
                () = signal.cancelled() => return Err(DirectoryPickerFailure::Internal(abort_error(&signal))),
                metadata = tokio::fs::metadata(&path) => metadata,
            } {
                Ok(metadata) => metadata.is_dir(),
                Err(_) => false,
            }
        } else {
            false
        };
        if !enterable {
            continue;
        }
        if entries.len() == max_entries {
            truncated = true;
            break;
        }
        entries.push(DirectoryEntry {
            hidden: candidate.name.starts_with('.'),
            name: candidate.name,
            path: path.to_string_lossy().into_owned(),
        });
    }
    Ok(DirectoryListing {
        path: target.to_string_lossy().into_owned(),
        home: home.to_string_lossy().into_owned(),
        crumbs: ancestry_crumbs(&target),
        entries,
        truncated,
    })
}

/// Creates one direct child under a fully-qualified parent.
///
/// # Errors
///
/// Returns segment validation, existing-target, or filesystem failures through
/// the closed picker vocabulary.
pub async fn create_directory(
    parent: String,
    name: String,
) -> Result<String, DirectoryPickerFailure> {
    if !fully_qualified(&parent, current_platform()) {
        return Err(DirectoryPickerError::new(
            DirectoryPickerErrorCode::DirectoryCreateFailed,
            &parent,
            format!("cannot create under \"{parent}\": not a fully qualified parent path"),
        )
        .into());
    }
    let parent_path = normalize_absolute(Path::new(&parent));
    let target = normalize_absolute(&parent_path.join(&name));
    if name.trim().is_empty()
        || matches!(name.as_str(), "." | "..")
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(DirectoryPickerError::new(
            DirectoryPickerErrorCode::DirectoryCreateFailed,
            target.to_string_lossy(),
            format!("\"{name}\" is not a single path segment"),
        )
        .into());
    }
    match tokio::fs::create_dir(&target).await {
        Ok(()) => Ok(target.to_string_lossy().into_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(DirectoryPickerError::new(
                DirectoryPickerErrorCode::DirectoryExists,
                target.to_string_lossy(),
                format!("{} already exists", target.display()),
            )
            .into())
        }
        Err(error) => Err(DirectoryPickerError::new(
            DirectoryPickerErrorCode::DirectoryCreateFailed,
            target.to_string_lossy(),
            format!("cannot create {}: {error}", target.display()),
        )
        .into()),
    }
}

/// Builds the browse backend Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, std::iter::empty::<String>(), |context, config| {
        Box::pin(async move {
            let config = parse_browse_config(&config)?;
            let list_config = config.clone();
            let service = DirectoryPickerService::new(DirectoryPickerCapability::Browse {
                list: Arc::new(move |path, signal| {
                    let max_entries = list_config.max_entries;
                    Box::pin(list_directory(path, signal, max_entries))
                }),
                create_directory: Arc::new(|path, name| Box::pin(create_directory(path, name))),
            });
            service.provide(&context)?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        let config = parse_browse_config(value)?;
        Ok(serde_json::to_value(config)?)
    })
}

/// Registers the stateless backend invariant.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}

fn ancestry_crumbs(target: &Path) -> Vec<DirectoryEntry> {
    target
        .ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|path| DirectoryEntry {
            name: path.file_name().map_or_else(
                || path.to_string_lossy().into_owned(),
                |name| name.to_string_lossy().into_owned(),
            ),
            path: path.to_string_lossy().into_owned(),
            hidden: false,
        })
        .collect()
}

/// Lexically resolves `.` and `..` like Node's `path.resolve` without
/// dereferencing symlinks or consulting the filesystem.
fn normalize_absolute(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
        }
    }
    normalized
}

fn abort_error(signal: &AbortSignal) -> anyhow::Error {
    if let Some(error) = signal.error_reason() {
        anyhow::Error::new(error)
    } else {
        anyhow::anyhow!(javascript_string(
            &signal.reason().unwrap_or(serde_json::Value::Null)
        ))
    }
}

fn parse_browse_config(value: &serde_json::Value) -> anyhow::Result<BrowseConfig> {
    let value = if value.is_null() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        value.clone()
    };
    let config: BrowseConfig = serde_json::from_value(value)?;
    anyhow::ensure!(config.max_entries >= 1, "maxEntries must be at least 1");
    Ok(config)
}

fn locale_compare(left: &str, right: &str) -> Ordering {
    static COLLATOR: OnceLock<CollatorBorrowed<'static>> = OnceLock::new();
    COLLATOR
        .get_or_init(|| {
            let locale = sys_locale::get_locale()
                .and_then(|locale| locale.parse::<Locale>().ok())
                .unwrap_or_else(|| "en-US".parse().expect("fallback locale is valid"));
            Collator::try_new(
                CollatorPreferences::from(&locale),
                CollatorOptions::default(),
            )
            .expect("compiled ICU collation data includes the active locale")
        })
        .compare(left, right)
}

fn javascript_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => ryu_js::Buffer::new()
            .format(value.as_f64().unwrap_or_default())
            .to_owned(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| match value {
                serde_json::Value::Null => String::new(),
                value => javascript_string(value),
            })
            .collect::<Vec<_>>()
            .join(","),
        serde_json::Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn unreadable(path: &Path, error: &std::io::Error) -> DirectoryPickerFailure {
    DirectoryPickerError::new(
        DirectoryPickerErrorCode::DirectoryUnreadable,
        path.to_string_lossy(),
        format!("cannot list {}: {error}", path.display()),
    )
    .into()
}

const fn current_platform() -> PathPlatform {
    if cfg!(windows) {
        PathPlatform::Windows
    } else {
        PathPlatform::Posix
    }
}
