//! Layered local credential provider backed by an owner-only YAML document.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Weak},
    time::Duration,
};

use async_trait::async_trait;
use indexmap::IndexMap;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use path_clean::PathClean;
use seekdeep_cordis::{
    Context, Plugin,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_credentials::{
    CredentialInfo, CredentialNotifier, CredentialProvider, CredentialRef, CredentialService,
    ResolvedCredential, credential_ref,
};
use seekdeep_util::{
    atomic_write::{WriteFileAtomicOptions, with_file_lock, write_file_atomic},
    home_paths::{canonicalize_watch_path, resolve_process_seekdeep_home},
    launch_environment::{LaunchEnvironmentSource, launch_environment_of},
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor},
};
use serde_json::Value;
use tokio::{sync::watch, task::JoinHandle};
use yaml_edit::YamlFile;

/// Package-owned invariant companion.
pub mod invariant;

pub use invariant::{INVARIANT_NAME, register_invariant};

/// Basename of the credentials document inside the harness home.
pub const CREDENTIALS_FILENAME: &str = ".credentials.yaml";
/// Cordis plugin name.
pub const NAME: &str = "credentials-local";
/// This provider has no startup service dependency.
pub const INJECT: &[&str] = &[];

/// File location and hot-reload behavior.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct LocalCredentialConfig {
    /// Explicit credentials document path.
    pub path: Option<PathBuf>,
    /// Explicit harness home used when `path` is absent.
    pub seekdeep_home: Option<PathBuf>,
    /// Whether external document changes are watched.
    pub watch: bool,
    /// Stable-write debounce window in milliseconds.
    pub debounce_ms: f64,
}

impl Default for LocalCredentialConfig {
    fn default() -> Self {
        Self {
            path: None,
            seekdeep_home: None,
            watch: true,
            debounce_ms: 100.0,
        }
    }
}

/// Fully resolved provider parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSpec {
    /// Absolute credential document path.
    pub filename: PathBuf,
    /// Whether external changes are watched.
    pub watch: bool,
    /// Stable-write debounce window in milliseconds.
    pub debounce_ms: f64,
}

/// Resolves defaults and the absolute document path.
///
/// # Errors
///
/// Returns an invalid debounce or home/current-directory resolution failure.
pub fn resolve_spec(config: &LocalCredentialConfig) -> anyhow::Result<ResolvedSpec> {
    anyhow::ensure!(
        config.debounce_ms.is_finite() && config.debounce_ms >= 0.0,
        "debounceMs must be a finite number greater than or equal to 0"
    );
    let filename = if let Some(path) = &config.path {
        absolute_clean(path)?
    } else {
        resolve_process_seekdeep_home(config.seekdeep_home.as_deref().map(Path::as_os_str))?
            .join(CREDENTIALS_FILENAME)
            .clean()
    };
    Ok(ResolvedSpec {
        filename,
        watch: config.watch,
        debounce_ms: config.debounce_ms,
    })
}

fn absolute_clean(path: &Path) -> std::io::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    }
    .clean())
}

#[derive(Debug, Default)]
struct ProviderState {
    text: Option<String>,
    values: IndexMap<String, String>,
    closed: bool,
}

/// File-backed provider with launch-environment layering.
pub struct LocalCredentialProvider {
    context: Context,
    spec: ResolvedSpec,
    notifier: CredentialNotifier,
    state: Mutex<ProviderState>,
    operation: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for LocalCredentialProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalCredentialProvider")
            .field("spec", &self.spec)
            .field("closed", &self.state.lock().closed)
            .finish_non_exhaustive()
    }
}

impl LocalCredentialProvider {
    async fn open(context: &Context, spec: ResolvedSpec) -> anyhow::Result<Arc<Self>> {
        let provider = Arc::new(Self {
            context: context.clone(),
            notifier: CredentialNotifier::new(context),
            spec,
            state: Mutex::new(ProviderState::default()),
            operation: tokio::sync::Mutex::new(()),
        });
        provider.load_initial().await?;
        Ok(provider)
    }

    /// Absolute credentials document path.
    #[must_use]
    pub fn filename(&self) -> &Path {
        &self.spec.filename
    }

    fn inherited(&self, reference: &CredentialRef) -> Option<String> {
        launch_environment_of(&self.context)
            .get_from(reference, &[LaunchEnvironmentSource::Process])
            .filter(|entry| !entry.value.is_empty())
            .map(|entry| entry.value)
    }

    fn dotenv_fallback(&self, reference: &CredentialRef) -> Option<(String, String)> {
        launch_environment_of(&self.context)
            .get_from(
                reference,
                &[
                    LaunchEnvironmentSource::ProjectEnv,
                    LaunchEnvironmentSource::UserEnv,
                ],
            )
            .filter(|entry| !entry.value.is_empty())
            .map(|entry| (entry.value, source_label(entry.source).to_owned()))
    }

    fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    fn assert_unshadowed(&self, reference: &CredentialRef, verb: &str) -> anyhow::Result<()> {
        if self.inherited(reference).is_some() {
            anyhow::bail!(
                "credentials-local: \"{reference}\" is supplied read-only by the launching environment, so {verb} would be shadowed; unset it in the shell you start seekdeep from instead"
            );
        }
        Ok(())
    }

    async fn load_initial(&self) -> anyhow::Result<()> {
        assert_owner_only(&self.spec.filename).await?;
        let Some(text) = read_optional(&self.spec.filename).await? else {
            return Ok(());
        };
        let values = parse_credentials_document(&text, &self.spec.filename)?;
        let mut state = self.state.lock();
        state.text = Some(text);
        state.values = values;
        Ok(())
    }

    async fn refresh(&self) -> anyhow::Result<()> {
        if self.is_closed() {
            return Ok(());
        }
        let _operation = self.operation.lock().await;
        if let Err(error) = self.reconcile_from_disk().await {
            if error
                .downcast_ref::<seekdeep_invariants::InvariantError>()
                .is_some()
            {
                return Err(error);
            }
            tracing::warn!(
                path = %self.spec.filename.display(),
                "credentials-local: reload failed; keeping the last good document"
            );
            tracing::warn!(%error, "credentials-local reload error");
        }
        Ok(())
    }

    async fn reconcile_from_disk(&self) -> anyhow::Result<()> {
        assert_owner_only(&self.spec.filename).await?;
        let text = read_optional(&self.spec.filename).await?;
        {
            let state = self.state.lock();
            if text == state.text || state.closed {
                return Ok(());
            }
        }
        let next = text.as_deref().map_or_else(
            || Ok(IndexMap::new()),
            |text| parse_credentials_document(text, &self.spec.filename),
        )?;
        let changed = {
            let mut state = self.state.lock();
            let changed = changed_refs(&state.values, &next)?;
            state.text = text;
            state.values = next;
            changed
        };
        for reference in changed {
            self.notifier.notify_updated(&reference)?;
        }
        Ok(())
    }

    async fn write_value(
        &self,
        reference: &CredentialRef,
        value: Option<&str>,
    ) -> anyhow::Result<()> {
        let verb = if value.is_some() { "set" } else { "unset" };
        anyhow::ensure!(
            !self.is_closed(),
            "credentials-local is disposed: cannot {verb} \"{reference}\""
        );
        self.assert_unshadowed(reference, verb)?;
        let _operation = self.operation.lock().await;
        anyhow::ensure!(
            !self.is_closed(),
            "credentials-local was disposed before the queued \"{reference}\" {verb} ran"
        );
        self.assert_unshadowed(reference, verb)?;
        create_private_parents(&self.spec.filename).await?;

        with_file_lock(&self.spec.filename, || async {
            self.reconcile_from_disk().await?;
            let (text, exists) = {
                let state = self.state.lock();
                (
                    state.text.clone(),
                    state.values.contains_key(reference.as_str()),
                )
            };
            if value.is_none() && !exists {
                return Ok::<(), anyhow::Error>(());
            }
            let next_text = render_document(text.as_deref(), reference, value)?;
            write_file_atomic(
                &self.spec.filename,
                next_text.as_bytes(),
                WriteFileAtomicOptions {
                    mode: 0o600,
                    dir_mode: Some(0o700),
                },
            )
            .await?;
            {
                let mut state = self.state.lock();
                state.text = Some(next_text);
                if let Some(value) = value {
                    state
                        .values
                        .insert(reference.as_str().to_owned(), value.to_owned());
                } else {
                    state.values.shift_remove(reference.as_str());
                }
            }
            self.notifier.notify_updated(reference)
        })
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    fn begin_shutdown(&self) {
        self.state.lock().closed = true;
    }

    async fn drain(&self) {
        let _drain = self.operation.lock().await;
    }
}

#[async_trait]
impl CredentialProvider for LocalCredentialProvider {
    async fn resolve(
        &self,
        reference: &CredentialRef,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        if let Some(value) = self.inherited(reference) {
            return Ok(Some(ResolvedCredential {
                value,
                source: "env".to_owned(),
            }));
        }
        if let Some(value) = self.state.lock().values.get(reference.as_str()).cloned() {
            return Ok(Some(ResolvedCredential {
                value,
                source: "file".to_owned(),
            }));
        }
        Ok(self
            .dotenv_fallback(reference)
            .map(|(value, source)| ResolvedCredential { value, source }))
    }

    async fn describe(&self, reference: &CredentialRef) -> anyhow::Result<CredentialInfo> {
        if self.inherited(reference).is_some() {
            return Ok(CredentialInfo {
                configured: true,
                source: Some("env".to_owned()),
                writable: false,
            });
        }
        if self.state.lock().values.contains_key(reference.as_str()) {
            return Ok(CredentialInfo {
                configured: true,
                source: Some("file".to_owned()),
                writable: true,
            });
        }
        if let Some((_, source)) = self.dotenv_fallback(reference) {
            return Ok(CredentialInfo {
                configured: true,
                source: Some(source),
                writable: true,
            });
        }
        Ok(CredentialInfo {
            configured: false,
            source: None,
            writable: true,
        })
    }

    async fn set(&self, reference: &CredentialRef, value: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !value.is_empty(),
            "credentials-local: an empty value cannot be stored for \"{reference}\"; use unset"
        );
        self.write_value(reference, Some(value)).await
    }

    async fn unset(&self, reference: &CredentialRef) -> anyhow::Result<()> {
        self.write_value(reference, None).await
    }
}

fn source_label(source: LaunchEnvironmentSource) -> &'static str {
    match source {
        LaunchEnvironmentSource::Process => "env",
        LaunchEnvironmentSource::ProjectEnv => "project-env",
        LaunchEnvironmentSource::UserEnv => "user-env",
    }
}

/// Parses a strict credential-reference-to-nonempty-string mapping.
///
/// Parser diagnostics are intentionally reduced to a generic code and source
/// position so a malformed secret value can never appear in logs or errors.
///
/// # Errors
///
/// Rejects malformed YAML, non-mapping roots, invalid references, non-string
/// values, empty values, and duplicate keys.
pub fn parse_credentials_document(
    text: &str,
    filename: &Path,
) -> anyhow::Result<IndexMap<String, String>> {
    let document = serde_yml::from_str::<serde_yml::Value>(text).map_err(|error| {
        let location = error.location().map_or_else(String::new, |location| {
            format!(" at line {}, column {}", location.line(), location.column())
        });
        anyhow::anyhow!(
            "credentials-local: invalid document at {}: YAML_PARSE{location}",
            filename.display()
        )
    })?;
    match document {
        serde_yml::Value::Null => return Ok(IndexMap::new()),
        serde_yml::Value::Mapping(_) => {}
        _ => anyhow::bail!(
            "credentials-local: {} must be a mapping of credential reference to value",
            filename.display()
        ),
    }
    let mapping = serde_yml::from_str::<StrictMapping>(text)
        .map_err(|_| {
            anyhow::anyhow!(
                "credentials-local: invalid document at {}: DUPLICATE_KEY",
                filename.display()
            )
        })?
        .0;
    let mut entries = IndexMap::new();
    for (key, value) in mapping {
        let serde_yml::Value::String(key) = key else {
            anyhow::bail!(
                "credentials-local: {} must be a mapping of credential reference to value",
                filename.display()
            );
        };
        credential_ref(&key)?;
        let serde_yml::Value::String(value) = value else {
            anyhow::bail!(
                "credentials-local: the value for \"{key}\" in {} must be a string",
                filename.display()
            );
        };
        anyhow::ensure!(
            !value.is_empty(),
            "credentials-local: the value for \"{key}\" in {} is empty; remove the key instead",
            filename.display()
        );
        anyhow::ensure!(
            entries.insert(key.clone(), value).is_none(),
            "credentials-local: invalid document at {}: DUPLICATE_KEY",
            filename.display()
        );
    }
    Ok(entries)
}

struct StrictMapping(Vec<(serde_yml::Value, serde_yml::Value)>);

impl<'de> Deserialize<'de> for StrictMapping {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MappingVisitor;

        impl<'de> Visitor<'de> for MappingVisitor {
            type Value = StrictMapping;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a mapping with unique keys")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut entries = Vec::new();
                let mut keys = HashSet::new();
                while let Some((key, value)) =
                    access.next_entry::<serde_yml::Value, serde_yml::Value>()?
                {
                    if !keys.insert(key.clone()) {
                        return Err(serde::de::Error::custom("DUPLICATE_KEY"));
                    }
                    entries.push((key, value));
                }
                Ok(StrictMapping(entries))
            }
        }

        deserializer.deserialize_map(MappingVisitor)
    }
}

fn render_document(
    text: Option<&str>,
    reference: &CredentialRef,
    value: Option<&str>,
) -> anyhow::Result<String> {
    let edited_text = match (text, value) {
        (Some(text), None) => Some(strip_entry_annotation(text, reference)),
        (Some(text), Some(_)) => Some(text.to_owned()),
        (None, _) => None,
    };
    let file = match edited_text.as_deref() {
        Some(text) => YamlFile::from_str(text).map_err(|_| {
            anyhow::anyhow!("credentials-local: validated document could not be edited")
        })?,
        None => YamlFile::new(),
    };
    file.ensure_document();
    let document = file
        .document()
        .ok_or_else(|| anyhow::anyhow!("credentials-local: editor has no document"))?;
    let mapping = document
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("credentials-local: editor root is not a mapping"))?;
    if let Some(value) = value {
        if let Some(existing) = mapping.get(reference.as_str())
            && let Some(scalar) = existing.as_scalar()
        {
            let range = scalar.byte_range();
            let mut replacement = render_scalar_like(&scalar.value(), value);
            let mut rendered = edited_text.expect("an existing scalar has source text");
            if rendered[range.start as usize..range.end as usize].ends_with('\n') {
                replacement.push('\n');
            }
            rendered.replace_range(range.start as usize..range.end as usize, &replacement);
            if !rendered.ends_with('\n') {
                rendered.push('\n');
            }
            return Ok(rendered);
        }
        if text.is_none() {
            return Ok(format!(
                "{}: {}\n",
                reference.as_str(),
                render_scalar(value)
            ));
        }
        if mapping.is_empty() {
            let source = edited_text.as_deref().unwrap_or_default();
            if source.trim() == "{}" {
                return Ok(format!(
                    "{{ {}: {} }}\n",
                    reference.as_str(),
                    render_scalar(value)
                ));
            }
            if source
                .lines()
                .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
            {
                return Ok(format!(
                    "{}\n\n{}: {}\n",
                    source.trim_end_matches(['\r', '\n']),
                    reference.as_str(),
                    render_scalar(value)
                ));
            }
        }
        if let Some(interior) = flow_mapping_interior(edited_text.as_deref().unwrap_or_default()) {
            return Ok(format!(
                "{{ {interior}, {}: {} }}\n",
                reference.as_str(),
                render_scalar(value)
            ));
        }
        if !mapping.is_flow_style() && !mapping.is_empty() {
            let source = edited_text.expect("a nonempty mapping has source text");
            let scalar_end = mapping
                .entries()
                .last()
                .and_then(|entry| entry.value_node())
                .and_then(|node| node.as_scalar().cloned())
                .map(|scalar| scalar.byte_range().end as usize)
                .ok_or_else(|| anyhow::anyhow!("credentials-local: mapping entry has no scalar"))?;
            let entry_end = if source[..scalar_end].ends_with('\n') {
                scalar_end
            } else {
                source[scalar_end..]
                    .find('\n')
                    .map_or(source.len(), |offset| scalar_end + offset + 1)
            };
            return Ok(append_block_entry(&source, entry_end, reference, value));
        }
        mapping.set(reference.as_str(), scalar_node(value)?);
    } else {
        mapping.remove(reference.as_str());
    }
    let mut rendered = file.to_string();
    if rendered.trim().is_empty() {
        return Ok("{}\n".to_owned());
    }
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}

fn flow_mapping_interior(source: &str) -> Option<String> {
    let trimmed = source.trim();
    let interior = trimmed.strip_prefix('{')?.strip_suffix('}')?;
    let interior = interior.trim().trim_end_matches(',').trim();
    (!interior.is_empty()).then(|| interior.to_owned())
}

fn append_block_entry(
    source: &str,
    mapping_end: usize,
    reference: &CredentialRef,
    value: &str,
) -> String {
    let mut rendered = source[..mapping_end]
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    rendered.push('\n');
    rendered.push_str(reference.as_str());
    rendered.push_str(": ");
    rendered.push_str(&render_scalar(value));
    rendered.push('\n');

    let suffix = source[mapping_end..].trim_start_matches(['\r', '\n']);
    if !suffix.is_empty() {
        if !suffix.trim_start().starts_with("...") {
            rendered.push('\n');
        }
        rendered.push_str(suffix);
    }
    rendered
}

fn scalar_node(value: &str) -> anyhow::Result<yaml_edit::YamlNode> {
    let scalar = render_scalar(value);
    let temporary = YamlFile::from_str(&format!("value: {scalar}\n"))
        .map_err(|_| anyhow::anyhow!("credentials-local: scalar could not be rendered"))?;
    temporary
        .document()
        .and_then(|document| document.as_mapping())
        .and_then(|mapping| mapping.get("value"))
        .ok_or_else(|| anyhow::anyhow!("credentials-local: rendered scalar is absent"))
}

fn render_scalar(value: &str) -> String {
    if value.contains('\n') {
        return render_block_scalar('|', value);
    }
    let candidate = format!("value: {value}\n");
    let is_plain_string = serde_yml::from_str::<serde_yml::Value>(&candidate)
        .ok()
        .and_then(|document| match document {
            serde_yml::Value::Mapping(mapping) => mapping
                .get(serde_yml::Value::String("value".to_owned()))
                .cloned(),
            _ => None,
        })
        .is_some_and(|parsed| parsed == serde_yml::Value::String(value.to_owned()));
    if is_plain_string {
        value.to_owned()
    } else {
        serde_json::to_string(value).expect("serializing a string cannot fail")
    }
}

fn render_scalar_like(existing: &str, value: &str) -> String {
    if existing.starts_with('|') {
        return render_block_scalar('|', value);
    }
    if existing.starts_with('>') {
        return render_block_scalar('>', value);
    }
    if existing.starts_with('\'') {
        return format!("'{}'", value.replace('\'', "''"));
    }
    if existing.starts_with('"') {
        return serde_json::to_string(value).expect("serializing a string cannot fail");
    }
    render_scalar(value)
}

fn render_block_scalar(style: char, value: &str) -> String {
    let trailing_newlines = value.chars().rev().take_while(|ch| *ch == '\n').count();
    let indicator = match trailing_newlines {
        0 => format!("{style}-"),
        1 => style.to_string(),
        _ => format!("{style}+"),
    };
    let content = value.trim_end_matches('\n');
    let content = if style == '>' {
        expand_folded_newlines(content)
    } else {
        content.to_owned()
    };
    let mut body = content
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    for _ in 1..trailing_newlines {
        body.push('\n');
    }
    format!("{indicator}\n{body}")
}

fn expand_folded_newlines(value: &str) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\n' {
            expanded.push(character);
            continue;
        }
        let mut count = 1;
        while characters.next_if_eq(&'\n').is_some() {
            count += 1;
        }
        for _ in 0..=count {
            expanded.push('\n');
        }
    }
    expanded
}

fn strip_entry_annotation(text: &str, reference: &CredentialRef) -> String {
    let mut lines = text.split_inclusive('\n').collect::<Vec<_>>();
    let quoted_single = format!("'{}':", reference.as_str());
    let quoted_double = format!("\"{}\":", reference.as_str());
    let bare = format!("{}:", reference.as_str());
    let Some(index) = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        line.len() == trimmed.len()
            && (trimmed.starts_with(&bare)
                || trimmed.starts_with(&quoted_single)
                || trimmed.starts_with(&quoted_double))
    }) else {
        return text.to_owned();
    };
    let mut start = index;
    while start > 0 {
        let previous = lines[start - 1].trim();
        if previous.is_empty() || previous.starts_with('#') {
            start -= 1;
        } else {
            break;
        }
    }
    lines.drain(start..index);
    lines.concat()
}

fn changed_refs(
    previous: &IndexMap<String, String>,
    next: &IndexMap<String, String>,
) -> anyhow::Result<Vec<CredentialRef>> {
    let mut seen = HashSet::new();
    let mut changed = Vec::new();
    for key in previous.keys().chain(next.keys()) {
        if !seen.insert(key.as_str()) || previous.get(key) == next.get(key) {
            continue;
        }
        changed.push(credential_ref(key)?);
    }
    Ok(changed)
}

async fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

async fn assert_owner_only(path: &Path) -> anyhow::Result<()> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            canonicalize_watch_path(path).await?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode();
        let offending = mode & 0o077;
        anyhow::ensure!(
            offending == 0,
            "credentials-local: {} is readable beyond its owner (mode {:o}); run \"chmod 600 {}\" before starting again",
            path.display(),
            mode & 0o777,
            path.display()
        );
    }
    Ok(())
}

async fn create_private_parents(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        builder.mode(0o700);
    }
    builder.create(parent).await
}

fn watch_root(target: &Path) -> anyhow::Result<PathBuf> {
    let mut current = target.to_path_buf();
    loop {
        match std::fs::metadata(&current) {
            Ok(metadata) if metadata.is_dir() => return Ok(current),
            Ok(_) => {
                anyhow::ensure!(current.pop(), "watch path has no directory ancestor");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                anyhow::ensure!(current.pop(), "watch path has no existing ancestor");
            }
            Err(error) => return Err(error.into()),
        }
    }
}

struct WatchLifecycle {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl WatchLifecycle {
    async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

async fn start_watcher(
    provider: &Arc<LocalCredentialProvider>,
) -> anyhow::Result<Option<WatchLifecycle>> {
    if !provider.spec.watch {
        return Ok(None);
    }
    let target = canonicalize_watch_path(&provider.spec.filename).await?;
    let root = watch_root(&target)?;
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = events_tx.send(event);
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    if let Err(error) = provider.refresh().await {
        tracing::error!(path = %target.display(), %error, "credentials-local: reload commit failed");
    }
    let (stop, stop_rx) = watch::channel(false);
    let weak = Arc::downgrade(provider);
    let debounce = Duration::from_secs_f64(provider.spec.debounce_ms / 1_000.0);
    let task = tokio::spawn(watcher_loop(
        weak, watcher, events_rx, stop_rx, target, debounce,
    ));
    Ok(Some(WatchLifecycle { stop, task }))
}

async fn watcher_loop(
    provider: Weak<LocalCredentialProvider>,
    _watcher: RecommendedWatcher,
    mut events: tokio::sync::mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    mut stop: watch::Receiver<bool>,
    target: PathBuf,
    debounce: Duration,
) {
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    Err(error) => {
                        tracing::warn!(path = %target.display(), %error, "credentials-local: watcher error");
                    }
                    Ok(event) if relevant_event(&event, &target) => {
                        if wait_for_settle(&mut events, &mut stop, &target, debounce).await {
                            break;
                        }
                        if let Some(provider) = provider.upgrade()
                            && let Err(error) = provider.refresh().await
                        {
                            tracing::error!(path = %target.display(), %error, "credentials-local: reload commit failed");
                        }
                    }
                    Ok(_) => {}
                }
            }
        }
    }
}

async fn wait_for_settle(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    stop: &mut watch::Receiver<bool>,
    target: &Path,
    debounce: Duration,
) -> bool {
    let timer = tokio::time::sleep(debounce);
    tokio::pin!(timer);
    loop {
        tokio::select! {
            changed = stop.changed() => {
                return changed.is_err() || *stop.borrow();
            }
            () = &mut timer => return false,
            event = events.recv() => {
                match event {
                    None => return false,
                    Some(Err(error)) => {
                        tracing::warn!(path = %target.display(), %error, "credentials-local: watcher error");
                    }
                    Some(Ok(event)) if relevant_event(&event, target) => {
                        timer.as_mut().reset(tokio::time::Instant::now() + debounce);
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

fn relevant_event(event: &notify::Event, target: &Path) -> bool {
    event.paths.is_empty() || event.paths.iter().any(|path| path.clean() == target)
}

/// Builds the Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: LocalCredentialConfig = serde_json::from_value(config)?;
            let spec = resolve_spec(&config)?;
            let provider = LocalCredentialProvider::open(&context, spec).await?;
            let service = CredentialService::new(provider.clone());
            service.provide(&context)?;
            let watcher = start_watcher(&provider).await?;
            let cleanup_provider = provider.clone();
            context.own(EffectHandle::new(
                "credentials-local drain",
                move || -> DisposeFuture {
                    Box::pin(async move {
                        cleanup_provider.begin_shutdown();
                        if let Some(watcher) = watcher {
                            watcher.stop().await;
                        }
                        cleanup_provider.drain().await;
                        Ok(())
                    })
                },
            ))?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        let config: LocalCredentialConfig = serde_json::from_value(value.clone())?;
        resolve_spec(&config)?;
        Ok(serde_json::to_value(config)?)
    })
}

/// Installs the provider plugin.
///
/// # Errors
///
/// Returns inactive-context or configuration serialization failures.
pub fn install(
    context: &Context,
    config: LocalCredentialConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}
