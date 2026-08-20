//! Session-visible workspace instruction state and dynamic reconciliation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_agent::Agent;
use seekdeep_core::session::{Session, derive_event_message};
use seekdeep_fs::FileSystem;
use seekdeep_fs::types::FsVersion;
use seekdeep_llm::{AbortSignal, ContentBlock, Message, MessageSource, UserMessage};
use serde_json::{Map, Value, json};

use crate::config::ResolvedConfig;
use crate::digest::{instruction_content_sha1, trimmed_instruction_digest};
use crate::files::{
    LoadedInstructionFile, ScopeInstructionProbe, ancestor_chain, descendant_dirs_between,
    find_project_root, probe_scope_instruction, read_scope_instruction, relative_display,
};
use crate::render::{
    AgentInstructionAction, AgentInstructionChange, ChangeRenderItem, USER_GLOBAL_DIRECTORY,
    USER_GLOBAL_FILE, candidate_scope_key, decode_scope_key, instruction_scope_key,
    render_instruction_changes,
};

/// Cordis plugin name.
pub const NAME: &str = "agent-instructions";

/// The source kind discriminator for workspace instruction context.
pub const AGENT_INSTRUCTIONS_KIND: &str = "agent-instructions";

/// Per-scope metadata cache; instruction prose is deliberately not retained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstructionVersionState {
    /// Model-facing path.
    pub path: String,
    /// Provider freshness token.
    pub version: FsVersion,
    /// Exact content digest.
    pub digest: String,
    /// Trimmed-content identity used for per-directory duplicate suppression.
    pub trimmed_digest: String,
}

/// Session-isolated fast-path state keyed by logical instruction scope.
#[derive(Clone, Default)]
pub struct InstructionVersionCache(
    Arc<Mutex<HashMap<usize, HashMap<String, InstructionVersionState>>>>,
);

/// A metadata-cache transition associated with one rendered instruction change.
#[derive(Clone, Debug)]
pub struct InstructionVersionUpdate {
    /// The rendered transition.
    pub change: AgentInstructionChange,
    /// New cache state, absent for a removal.
    pub state: Option<InstructionVersionState>,
}

/// Rendered reconciliation plus its metadata-cache transitions.
#[derive(Clone, Debug)]
pub struct ReconciledInstructionContext {
    /// The durable context message.
    pub context: UserMessage,
    /// Metadata-cache transitions deferred until commit.
    pub version_updates: Vec<InstructionVersionUpdate>,
}

/// Durable producer, file, and reconciliation facts for one workspace context.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentInstructionSource {
    /// Source form; always instructions.
    pub form: String,
    /// Marks the complete startup/resume baseline.
    pub baseline: Option<bool>,
    /// Discovery, precedence, and budget identity for a resumed baseline.
    pub baseline_identity: Option<String>,
    /// Reconciliation transitions.
    pub changes: Vec<AgentInstructionChange>,
}

impl InstructionVersionCache {
    pub(crate) fn lock(
        &self,
    ) -> parking_lot::MutexGuard<'_, HashMap<usize, HashMap<String, InstructionVersionState>>> {
        self.0.lock()
    }
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn workspace_context_hook(text: &str, changes: &[AgentInstructionChange]) -> UserMessage {
    let mut fields = Map::new();
    fields.insert("form".to_owned(), json!("instructions"));
    fields.insert(
        "changes".to_owned(),
        serde_json::to_value(changes).expect("changes serialize"),
    );
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource {
            kind: AGENT_INSTRUCTIONS_KIND.to_owned(),
            fields,
        },
    )
}

/// Builds the user-role message for a rendered baseline.
#[must_use]
pub fn workspace_context_message(text: &str) -> Message {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::plugin(NAME),
    )
    .into_message()
}

fn is_workspace_context_source(source: &MessageSource) -> bool {
    source.kind == AGENT_INSTRUCTIONS_KIND
        && source.fields.get("changes").is_some_and(Value::is_array)
}

fn workspace_instruction_changes(source: &MessageSource) -> Vec<AgentInstructionChange> {
    let Some(changes) = source.fields.get("changes").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for value in changes {
        let Some(object) = value.as_object() else {
            continue;
        };
        let Some(action) = object.get("action").and_then(Value::as_str) else {
            continue;
        };
        let action = match action {
            "set" => AgentInstructionAction::Set,
            "replace" => AgentInstructionAction::Replace,
            "remove" => AgentInstructionAction::Remove,
            _ => continue,
        };
        let Some(scope) = object.get("scope").and_then(Value::as_str) else {
            continue;
        };
        let Some(path) = object.get("path").and_then(Value::as_str) else {
            continue;
        };
        let digest = match object.get("digest") {
            None => None,
            Some(Value::String(digest)) => Some(digest.clone()),
            Some(_) => continue,
        };
        result.push(AgentInstructionChange {
            action,
            scope: scope.to_owned(),
            path: path.to_owned(),
            digest,
        });
    }
    result
}

fn same_instruction_change(a: &AgentInstructionChange, b: &AgentInstructionChange) -> bool {
    a.action == b.action && a.scope == b.scope && a.path == b.path && a.digest == b.digest
}

fn visible_instruction_changes(
    agent: &Agent,
    authority_messages: &[UserMessage],
) -> HashMap<String, AgentInstructionChange> {
    let visible_seqs: HashSet<u64> = agent.session().surface_nodes().into_iter().collect();
    let mut visible: HashMap<String, AgentInstructionChange> = HashMap::new();
    for (seq, event) in agent.session().events().iter().enumerate() {
        if event.event_type != "user/message" {
            continue;
        }
        let Some(message) = derive_event_message(event) else {
            continue;
        };
        if !is_workspace_context_source(message.source()) {
            continue;
        }
        for change in workspace_instruction_changes(message.source()) {
            if visible_seqs.contains(&(seq as u64)) {
                visible.insert(change.scope.clone(), change);
            }
        }
    }
    for message in authority_messages {
        if !is_workspace_context_source(message.source()) {
            continue;
        }
        for change in workspace_instruction_changes(message.source()) {
            visible.insert(change.scope.clone(), change);
        }
    }
    visible
}

/// Latest baseline changes and provider versions keyed by logical scope.
#[derive(Clone, Debug, Default)]
pub struct BaselineInstructionState {
    /// Latest baseline changes.
    pub changes: HashMap<String, AgentInstructionChange>,
    /// Provider versions for retained files.
    pub versions: HashMap<String, InstructionVersionState>,
}

/// Converts retained baseline files into comparison and metadata-cache state.
#[must_use]
pub fn baseline_instruction_state(files: &[LoadedInstructionFile]) -> BaselineInstructionState {
    let mut changes = HashMap::new();
    let mut versions = HashMap::new();
    for file in files {
        let digest = instruction_content_sha1(&file.content);
        let scope = instruction_scope_key(&file.display_path);
        changes.insert(
            scope.clone(),
            AgentInstructionChange {
                action: AgentInstructionAction::Set,
                scope: scope.clone(),
                path: file.display_path.clone(),
                digest: Some(digest.clone()),
            },
        );
        if let Some(version) = &file.version {
            versions.insert(
                scope,
                InstructionVersionState {
                    path: file.display_path.clone(),
                    version: version.clone(),
                    digest,
                    trimmed_digest: trimmed_instruction_digest(&file.content),
                },
            );
        }
    }
    BaselineInstructionState { changes, versions }
}

/// Keeps only cache updates represented by rendered changes.
#[must_use]
pub fn retained_instruction_version_updates(
    updates: &[InstructionVersionUpdate],
    rendered_changes: &[AgentInstructionChange],
) -> Vec<InstructionVersionUpdate> {
    updates
        .iter()
        .filter(|update| {
            rendered_changes
                .iter()
                .any(|change| same_instruction_change(&update.change, change))
        })
        .cloned()
        .collect()
}

/// Applies metadata-cache transitions without retaining instruction prose.
///
/// # Panics
///
/// Panics if the cache entry vanishes under a held lock, which cannot happen.
pub fn apply_instruction_version_updates(
    session: &Arc<Session>,
    updates: &[InstructionVersionUpdate],
    cache: &InstructionVersionCache,
) {
    if updates.is_empty() {
        return;
    }
    let key = session_key(session);
    let mut states = cache.0.lock();
    if !states.contains_key(&key) {
        return;
    }
    let scoped = states.get_mut(&key).expect("checked");
    for update in updates {
        match &update.state {
            Some(state) => {
                scoped.insert(update.change.scope.clone(), state.clone());
            }
            None => {
                scoped.remove(&update.change.scope);
            }
        }
    }
    if scoped.is_empty() {
        states.remove(&key);
    }
}

fn relative_scope(project_root: &str, dir: &str) -> String {
    let scope = relative_display(project_root, dir);
    if scope.is_empty() {
        ".".to_owned()
    } else {
        scope
    }
}

/// Reconciliation options passed by the plugin.
#[derive(Clone, Debug, Default)]
pub struct ReconcileOptions {
    /// Authoritative claimed context messages.
    pub authority_messages: Vec<UserMessage>,
    /// Pending workspace-only scope messages.
    pub scope_messages: Vec<UserMessage>,
    /// Touched absolute or cwd-relative paths.
    pub touched_paths: Vec<String>,
    /// Whether baseline scopes participate.
    pub include_baseline_scopes: bool,
    /// Excluded baseline scopes.
    pub excluded_baseline_scopes: Option<HashSet<String>>,
    /// Optional pre-resolved project root.
    pub project_root: Option<String>,
    /// Cancellation signal.
    pub signal: Option<AbortSignal>,
}

fn push_removal(
    items: &mut Vec<ChangeRenderItem>,
    version_updates: &mut Vec<InstructionVersionUpdate>,
    scope: &str,
    path: &str,
) {
    let change = AgentInstructionChange {
        action: AgentInstructionAction::Remove,
        scope: scope.to_owned(),
        path: path.to_owned(),
        digest: None,
    };
    items.push(ChangeRenderItem {
        change: change.clone(),
        file: LoadedInstructionFile {
            absolute_path: format!("removed:{scope}"),
            display_path: path.to_owned(),
            content: String::new(),
            version: None,
        },
    });
    version_updates.push(InstructionVersionUpdate {
        change,
        state: None,
    });
}

fn register_kept_trimmed(
    kept_trimmed_by_dir: &mut HashMap<String, HashSet<String>>,
    directory: &str,
    digest: &str,
) -> bool {
    !kept_trimmed_by_dir
        .entry(directory.to_owned())
        .or_default()
        .insert(digest.to_owned())
}

/// Compares visible state with provider-visible files and renders transitions.
///
/// # Errors
///
/// Returns an aborted, probe, or provider-read failure.
///
/// # Panics
///
/// Panics on the two unreachable defensive guards around validated prior changes.
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
pub async fn reconcile_instruction_context(
    agent: &Agent,
    resolved: &ResolvedConfig,
    version_cache: &InstructionVersionCache,
    file_system: &dyn FileSystem,
    options: &ReconcileOptions,
) -> anyhow::Result<Option<ReconciledInstructionContext>> {
    let session = agent.session().clone();
    let effective = visible_instruction_changes(agent, &options.authority_messages);
    let cwd = session.header().cwd.clone().unwrap_or_else(|| {
        std::env::current_dir().map_or_else(
            |_| "/".to_owned(),
            |path| path.to_string_lossy().into_owned(),
        )
    });
    let project_root = options.project_root.clone().unwrap_or(
        find_project_root(
            &cwd,
            &resolved.project_root_markers,
            Some(file_system),
            options.signal.as_ref(),
        )
        .await?,
    );

    let mut scopes: HashSet<String> = HashSet::new();
    let mut baseline_scopes: HashSet<String> = HashSet::new();
    let add_dir_scopes = |target: &mut HashSet<String>, directory: &str| {
        for candidate in &resolved.instruction_file_candidates {
            target.insert(candidate_scope_key(directory, candidate));
        }
        for candidate in &resolved.local_instruction_file_candidates {
            target.insert(candidate_scope_key(directory, candidate));
        }
    };
    baseline_scopes.insert(candidate_scope_key(USER_GLOBAL_DIRECTORY, USER_GLOBAL_FILE));
    for dir in ancestor_chain(&project_root, &cwd) {
        let scope = relative_scope(&project_root, &dir);
        add_dir_scopes(&mut baseline_scopes, &scope);
    }
    if options.include_baseline_scopes {
        scopes.extend(baseline_scopes.iter().cloned());
    }
    for message in &options.scope_messages {
        if !is_workspace_context_source(message.source()) {
            continue;
        }
        for change in workspace_instruction_changes(message.source()) {
            if !options.include_baseline_scopes && baseline_scopes.contains(&change.scope) {
                continue;
            }
            scopes.insert(change.scope);
        }
    }
    for scope in effective.keys() {
        if !options.include_baseline_scopes && baseline_scopes.contains(scope) {
            continue;
        }
        let (directory, _) = decode_scope_key(scope);
        if directory == USER_GLOBAL_DIRECTORY {
            scopes.insert(candidate_scope_key(USER_GLOBAL_DIRECTORY, USER_GLOBAL_FILE));
        } else {
            add_dir_scopes(&mut scopes, &directory);
        }
    }
    for touched_path in &options.touched_paths {
        for dir in descendant_dirs_between(&cwd, touched_path) {
            let scope = relative_scope(&project_root, &dir);
            add_dir_scopes(&mut scopes, &scope);
        }
    }

    let key = session_key(&session);
    let mut versions: HashMap<String, InstructionVersionState> = {
        let states = version_cache.0.lock();
        states.get(&key).cloned().unwrap_or_default()
    };
    let mut seen_absolute_paths: HashSet<String> = HashSet::new();
    let mut kept_trimmed_by_dir: HashMap<String, HashSet<String>> = HashMap::new();
    let mut items: Vec<ChangeRenderItem> = Vec::new();
    let mut version_updates: Vec<InstructionVersionUpdate> = Vec::new();

    let mut scopes_by_directory: HashMap<String, Vec<String>> = HashMap::new();
    for scope in &scopes {
        let (directory, _) = decode_scope_key(scope);
        scopes_by_directory
            .entry(directory)
            .or_default()
            .push(scope.clone());
    }
    for (directory, directory_scopes) in &scopes_by_directory {
        let mut probed_scopes: Vec<String> = Vec::new();
        for scope in directory_scopes {
            if options
                .excluded_baseline_scopes
                .as_ref()
                .is_some_and(|excluded| baseline_scopes.contains(scope) && excluded.contains(scope))
            {
                match effective.get(scope) {
                    None => {
                        versions.remove(scope);
                    }
                    Some(previous) if previous.action == AgentInstructionAction::Remove => {
                        versions.remove(scope);
                    }
                    Some(previous) => {
                        push_removal(&mut items, &mut version_updates, scope, &previous.path);
                    }
                }
            } else {
                probed_scopes.push(scope.clone());
            }
        }
        let item_start = items.len();
        let version_update_start = version_updates.len();
        let mut added_absolute_paths: Vec<String> = Vec::new();
        let prior_versions: HashMap<String, Option<InstructionVersionState>> = probed_scopes
            .iter()
            .map(|scope| (scope.clone(), versions.get(scope).cloned()))
            .collect();
        for scope in &probed_scopes {
            let previous = effective.get(scope).cloned();
            let probe = probe_scope_instruction(
                scope,
                &project_root,
                resolved,
                file_system,
                options.signal.as_ref(),
            )
            .await?;
            match probe {
                ScopeInstructionProbe::Unavailable => {
                    if previous.is_none_or(|change| change.action == AgentInstructionAction::Remove)
                    {
                        continue;
                    }
                    items.truncate(item_start);
                    version_updates.truncate(version_update_start);
                    for (candidate_scope, prior) in &prior_versions {
                        match prior {
                            Some(prior) => {
                                versions.insert(candidate_scope.clone(), prior.clone());
                            }
                            None => {
                                versions.remove(candidate_scope);
                            }
                        }
                    }
                    for absolute_path in &added_absolute_paths {
                        seen_absolute_paths.remove(absolute_path);
                    }
                    kept_trimmed_by_dir.remove(directory);
                    break;
                }
                ScopeInstructionProbe::Absent => match &previous {
                    None => {
                        versions.remove(scope);
                    }
                    Some(previous) if previous.action == AgentInstructionAction::Remove => {
                        versions.remove(scope);
                    }
                    Some(previous) => {
                        push_removal(&mut items, &mut version_updates, scope, &previous.path);
                    }
                },
                ScopeInstructionProbe::Present { file: probed_file } => {
                    if seen_absolute_paths.contains(&probed_file.absolute_path) {
                        continue;
                    }
                    seen_absolute_paths.insert(probed_file.absolute_path.clone());
                    added_absolute_paths.push(probed_file.absolute_path.clone());
                    let cached = versions.get(scope).cloned();
                    if let (Some(cached), Some(previous)) = (&cached, &previous)
                        && cached.path == probed_file.display_path
                        && cached.version == probed_file.version
                        && previous.action != AgentInstructionAction::Remove
                        && previous.path == cached.path
                        && previous.digest.as_deref() == Some(cached.digest.as_str())
                    {
                        if register_kept_trimmed(
                            &mut kept_trimmed_by_dir,
                            directory,
                            &cached.trimmed_digest,
                        ) {
                            push_removal(&mut items, &mut version_updates, scope, &previous.path);
                        }
                        continue;
                    }

                    let Some(file) = read_scope_instruction(
                        &probed_file,
                        resolved.max_source_bytes,
                        file_system,
                        options.signal.as_ref(),
                    )
                    .await?
                    else {
                        continue;
                    };
                    let current_digest = instruction_content_sha1(&file.content);
                    let trimmed_digest = trimmed_instruction_digest(&file.content);
                    if register_kept_trimmed(&mut kept_trimmed_by_dir, directory, &trimmed_digest) {
                        if previous
                            .as_ref()
                            .is_some_and(|change| change.action != AgentInstructionAction::Remove)
                        {
                            let path = previous.as_ref().expect("checked").path.clone();
                            push_removal(&mut items, &mut version_updates, scope, &path);
                        } else {
                            versions.remove(scope);
                        }
                        continue;
                    }
                    let next_version = InstructionVersionState {
                        path: file.display_path.clone(),
                        version: probed_file.version.clone(),
                        digest: current_digest.clone(),
                        trimmed_digest,
                    };
                    if previous.as_ref().is_some_and(|change| {
                        change.action != AgentInstructionAction::Remove
                            && change.path == file.display_path
                            && change.digest.as_deref() == Some(current_digest.as_str())
                    }) {
                        versions.insert(scope.clone(), next_version);
                        continue;
                    }
                    let action = if previous
                        .is_none_or(|change| change.action == AgentInstructionAction::Remove)
                    {
                        AgentInstructionAction::Set
                    } else {
                        AgentInstructionAction::Replace
                    };
                    let change = AgentInstructionChange {
                        action,
                        scope: scope.clone(),
                        path: file.display_path.clone(),
                        digest: Some(current_digest),
                    };
                    items.push(ChangeRenderItem {
                        change: change.clone(),
                        file,
                    });
                    version_updates.push(InstructionVersionUpdate {
                        change,
                        state: Some(next_version),
                    });
                }
            }
        }
    }

    {
        let mut states = version_cache.0.lock();
        if versions.is_empty() {
            states.remove(&key);
        } else {
            states.insert(key, versions);
        }
    }

    if items.is_empty() {
        return Ok(None);
    }
    let (text, rendered_changes) = render_instruction_changes(&items, resolved.max_bytes as usize);
    if text.is_empty() || rendered_changes.is_empty() {
        return Ok(None);
    }
    Ok(Some(ReconciledInstructionContext {
        context: workspace_context_hook(&text, &rendered_changes),
        version_updates: retained_instruction_version_updates(&version_updates, &rendered_changes),
    }))
}
