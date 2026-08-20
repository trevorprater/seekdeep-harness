//! Workspace instruction loader for AGENTS.md-compatible files.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentEvent, PreStepDecision};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin};
use seekdeep_core::session::{Session, SessionEvent, derive_event_message};
use seekdeep_fs::FS;
use seekdeep_llm::{AbortSignal, ContentBlock, Message, MessageSource, UserMessage};
use seekdeep_tools::{ToolExecution, ToolExecutionResult, ToolExecutionToken};
use serde_json::{Map, Value, json};

use crate::config::{Config, ResolvedConfig, resolve_config, workspace_baseline_identity};
use crate::files::{DiscoverOptions, find_project_root, load_baseline_instruction_set};
use crate::render::{AgentInstructionAction, AgentInstructionChange};
use crate::state::{
    AGENT_INSTRUCTIONS_KIND, InstructionVersionCache, ReconcileOptions,
    apply_instruction_version_updates, baseline_instruction_state, reconcile_instruction_context,
    workspace_context_message,
};

/// Cordis plugin name.
pub const NAME: &str = "agent-instructions";

/// Services required by the workspace instruction loader.
pub const INJECT: &[&str] = &[];

/// The source-compatible admission schema for Config.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> seekdeep_schemastery::Schema {
    crate::config::config_schema()
}

#[derive(Clone)]
struct BaselinePreparation {
    identity: String,
    excluded_scopes: std::collections::HashSet<String>,
}

#[derive(Clone)]
struct ProjectionTouch {
    agent: Arc<Agent>,
    path: String,
}

struct InstructionRuntime {
    context: Context,
    resolved: ResolvedConfig,
    instruction_versions: InstructionVersionCache,
    baseline_preparations: Mutex<HashMap<usize, BaselinePreparation>>,
    projection_lifecycle: AbortSignal,
    execution_touches: Mutex<HashMap<ToolExecutionToken, Vec<ProjectionTouch>>>,
    projection_tails: Mutex<HashMap<usize, tokio::task::JoinHandle<()>>>,
    open_steps: Mutex<HashMap<usize, bool>>,
    step_touches: Mutex<HashMap<usize, Vec<ProjectionTouch>>>,
}

fn agent_key(agent: &Arc<Agent>) -> usize {
    Arc::as_ptr(agent) as usize
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn file_path_from_execution(exec: &ToolExecution) -> Option<String> {
    const FILE_TOUCH_TOOL_NAMES: [&str; 3] = ["read", "write", "edit"];
    if !FILE_TOUCH_TOOL_NAMES.contains(&exec.name.as_str()) {
        return None;
    }
    let path = exec.arguments.get("file_path")?.as_str()?.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_owned())
    }
}

fn is_workspace_context(message: &UserMessage) -> bool {
    message.source().kind == AGENT_INSTRUCTIONS_KIND
}

fn same_context_payload(left: &Message, right: &Message) -> bool {
    left.content() == right.content() && left.source() == right.source()
}

fn visible_baseline_source(
    agent: &Agent,
    authority_messages: &[UserMessage],
) -> Option<MessageSource> {
    for message in authority_messages.iter().rev() {
        let source = message.source();
        if source.kind == AGENT_INSTRUCTIONS_KIND
            && source.fields.get("baseline").and_then(Value::as_bool) == Some(true)
        {
            return Some(source.clone());
        }
    }
    for seq in agent.session().surface_nodes().iter().rev() {
        let index = usize::try_from(*seq).ok()?;
        let events = agent.session().events();
        let event = events.get(index)?;
        if event.event_type != "user/message" {
            continue;
        }
        let message = derive_event_message(event)?;
        let source = message.source();
        if source.kind == AGENT_INSTRUCTIONS_KIND
            && source.fields.get("baseline").and_then(Value::as_bool) == Some(true)
        {
            return Some(source.clone());
        }
    }
    None
}

fn instruction_changes_from_source(source: &MessageSource) -> Vec<AgentInstructionChange> {
    let Some(changes) = source.fields.get("changes").and_then(Value::as_array) else {
        return Vec::new();
    };
    changes
        .iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            let action = match object.get("action")?.as_str()? {
                "set" => AgentInstructionAction::Set,
                "replace" => AgentInstructionAction::Replace,
                "remove" => AgentInstructionAction::Remove,
                _ => return None,
            };
            let scope = object.get("scope")?.as_str()?.to_owned();
            let path = object.get("path")?.as_str()?.to_owned();
            let digest = match object.get("digest") {
                None => None,
                Some(Value::String(digest)) => Some(digest.clone()),
                Some(_) => return None,
            };
            Some(AgentInstructionChange {
                action,
                scope,
                path,
                digest,
            })
        })
        .collect()
}

fn agent_instruction_source(
    changes: Vec<AgentInstructionChange>,
    baseline: bool,
    baseline_identity: Option<String>,
) -> MessageSource {
    let mut fields = Map::new();
    fields.insert("form".to_owned(), json!("instructions"));
    if baseline {
        fields.insert("baseline".to_owned(), json!(true));
    }
    if let Some(identity) = baseline_identity {
        fields.insert("baselineIdentity".to_owned(), json!(identity));
    }
    fields.insert(
        "changes".to_owned(),
        serde_json::to_value(changes).expect("changes serialize"),
    );
    MessageSource {
        kind: AGENT_INSTRUCTIONS_KIND.to_owned(),
        fields,
    }
}

fn ensure_not_aborted(signal: &AbortSignal) -> anyhow::Result<()> {
    if signal.is_aborted() {
        anyhow::bail!("agent-instructions disposed");
    }
    Ok(())
}

impl InstructionRuntime {
    fn new(context: &Context, config: &Config) -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            context: context.clone(),
            resolved: resolve_config(config)?,
            instruction_versions: InstructionVersionCache::default(),
            baseline_preparations: Mutex::new(HashMap::new()),
            projection_lifecycle: AbortSignal::default(),
            execution_touches: Mutex::new(HashMap::new()),
            projection_tails: Mutex::new(HashMap::new()),
            open_steps: Mutex::new(HashMap::new()),
            step_touches: Mutex::new(HashMap::new()),
        }))
    }

    #[allow(clippy::too_many_lines)]
    async fn compose(
        self: &Arc<Self>,
        agent: &Arc<Agent>,
        signal: &AbortSignal,
        claimed: &[UserMessage],
        pending: &[UserMessage],
        touched_paths: &[String],
    ) -> anyhow::Result<Option<UserMessage>> {
        ensure_not_aborted(signal)?;
        if self.resolved.max_bytes == 0 {
            return Ok(None);
        }
        let Some(file_system) = self.context.get(FS) else {
            return Ok(None);
        };
        let file_system = file_system.filesystem();
        if touched_paths.is_empty() && !pending.is_empty() {
            return Ok(Some(pending[0].clone()));
        }

        let mut content: Vec<ContentBlock> = Vec::new();
        let mut changes: Vec<AgentInstructionChange> = Vec::new();
        let mut desired_baseline = false;
        let mut authority_messages = claimed.to_vec();
        let cwd = agent.session().header().cwd.clone().unwrap_or_else(|| {
            std::env::current_dir().map_or_else(
                |_| "/".to_owned(),
                |path| path.to_string_lossy().into_owned(),
            )
        });
        let project_root = find_project_root(
            &cwd,
            &self.resolved.project_root_markers,
            Some(file_system.as_ref()),
            Some(signal),
        )
        .await?;
        let identity = workspace_baseline_identity(&self.resolved, &cwd, &project_root);
        let visible_baseline = visible_baseline_source(agent, &authority_messages);
        let baseline_present = visible_baseline.is_some();
        let keep_visible_baseline = visible_baseline.as_ref().and_then(|source| {
            source
                .fields
                .get("baselineIdentity")
                .and_then(Value::as_str)
        }) == Some(identity.as_str());
        let prepared = self
            .baseline_preparations
            .lock()
            .get(&session_key(agent.session()))
            .cloned();
        let mut excluded_baseline_scopes = if keep_visible_baseline
            && prepared
                .as_ref()
                .is_some_and(|prep| prep.identity == identity)
        {
            prepared.as_ref().map(|prep| prep.excluded_scopes.clone())
        } else {
            None
        };
        let mut next_preparation: Option<BaselinePreparation> = None;
        if !baseline_present || !keep_visible_baseline || excluded_baseline_scopes.is_none() {
            let replace_previous_baseline = baseline_present && !keep_visible_baseline;
            let instructions = load_baseline_instruction_set(
                &DiscoverOptions {
                    cwd: cwd.clone(),
                    dsh_home: Some(self.resolved.dsh_home.clone()),
                    project_root_markers: Some(self.resolved.project_root_markers.clone()),
                    instruction_file_candidates: Some(
                        self.resolved.instruction_file_candidates.clone(),
                    ),
                    local_instruction_file_candidates: Some(
                        self.resolved.local_instruction_file_candidates.clone(),
                    ),
                    project_root: Some(project_root.clone()),
                    signal: Some(signal.clone()),
                },
                self.resolved.max_bytes,
                self.resolved.max_source_bytes,
                Some(replace_previous_baseline),
                Some(file_system.as_ref()),
            )
            .await?;
            let included = instructions
                .as_ref()
                .map_or_else(Vec::new, |set| set.included.clone());
            let observed = instructions
                .as_ref()
                .map_or_else(Vec::new, |set| set.observed.clone());
            let baseline = baseline_instruction_state(&included);
            let observed_baseline = baseline_instruction_state(&observed);
            let mut excluded_scopes: std::collections::HashSet<String> =
                observed_baseline.changes.keys().cloned().collect();
            for scope in baseline.changes.keys() {
                excluded_scopes.remove(scope);
            }
            excluded_baseline_scopes = Some(excluded_scopes.clone());
            next_preparation = Some(BaselinePreparation {
                identity: identity.clone(),
                excluded_scopes,
            });
            if !baseline.versions.is_empty() {
                let mut versions = self.instruction_versions.lock();
                let scoped = versions.entry(session_key(agent.session())).or_default();
                for (scope, state) in &baseline.versions {
                    scoped.insert(scope.clone(), state.clone());
                }
            }
            let rendered_text = instructions
                .as_ref()
                .map(|set| set.rendered.text.clone())
                .unwrap_or_default();
            if !keep_visible_baseline && !rendered_text.is_empty() {
                content.extend(workspace_context_message(&rendered_text).content().to_vec());
                let replacement_scopes: std::collections::HashSet<String> =
                    baseline.changes.keys().cloned().collect();
                let replacement_removals = if replace_previous_baseline {
                    visible_baseline
                        .as_ref()
                        .map(|source| {
                            instruction_changes_from_source(source)
                                .into_iter()
                                .filter(|change| {
                                    change.action != AgentInstructionAction::Remove
                                        && !replacement_scopes.contains(&change.scope)
                                })
                                .map(|change| AgentInstructionChange {
                                    action: AgentInstructionAction::Remove,
                                    scope: change.scope,
                                    path: change.path,
                                    digest: None,
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let mut baseline_changes = replacement_removals;
                baseline_changes.extend(baseline.changes.values().cloned());
                changes.extend(baseline_changes.clone());
                authority_messages.push(UserMessage::new(
                    workspace_context_message(&rendered_text).content().to_vec(),
                    agent_instruction_source(
                        baseline_changes.clone(),
                        true,
                        Some(identity.clone()),
                    ),
                ));
                desired_baseline = true;
            }
        }
        let update = reconcile_instruction_context(
            agent,
            &self.resolved,
            &self.instruction_versions,
            file_system.as_ref(),
            &ReconcileOptions {
                authority_messages: authority_messages.clone(),
                scope_messages: pending.to_vec(),
                include_baseline_scopes: keep_visible_baseline,
                excluded_baseline_scopes: if keep_visible_baseline {
                    excluded_baseline_scopes.clone()
                } else {
                    None
                },
                touched_paths: touched_paths.to_vec(),
                project_root: Some(project_root),
                signal: Some(signal.clone()),
            },
        )
        .await?;
        if let Some(update) = update {
            content.extend(update.context.content().to_vec());
            if update.context.source().kind == AGENT_INSTRUCTIONS_KIND {
                changes.extend(instruction_changes_from_source(update.context.source()));
            }
            apply_instruction_version_updates(
                agent.session(),
                &update.version_updates,
                &self.instruction_versions,
            );
        }
        if let Some(next_preparation) = next_preparation {
            self.baseline_preparations
                .lock()
                .insert(session_key(agent.session()), next_preparation);
        }
        if content.is_empty() {
            return Ok(None);
        }
        Ok(Some(UserMessage::new(
            content,
            agent_instruction_source(
                changes,
                desired_baseline,
                desired_baseline.then_some(identity),
            ),
        )))
    }

    fn sync_inbox(agent: &Arc<Agent>, claimed: &[UserMessage], desired: Option<&UserMessage>) {
        let inbox = agent.inbox();
        let pending = inbox
            .next_step()
            .into_iter()
            .filter(is_workspace_context)
            .collect::<Vec<_>>();
        let already_supplied = desired.is_some_and(|desired| {
            claimed
                .iter()
                .any(|message| same_context_payload(message, desired))
                || agent.session().surface_nodes().iter().any(|seq| {
                    let Ok(index) = usize::try_from(*seq) else {
                        return false;
                    };
                    let events = agent.session().events();
                    let Some(event) = events.get(index) else {
                        return false;
                    };
                    if event.event_type != "user/message" {
                        return false;
                    }
                    let Some(message) = derive_event_message(event) else {
                        return false;
                    };
                    same_context_payload(&message, desired)
                })
        });
        if desired.is_none() || already_supplied {
            for message in &pending {
                let _ = inbox.remove(message.id());
            }
            return;
        }
        let desired = desired.expect("checked");
        let reusable = pending
            .iter()
            .find(|message| same_context_payload(message, desired));
        if let Some(reusable) = reusable {
            for message in &pending {
                if message.id() != reusable.id() {
                    let _ = inbox.remove(message.id());
                }
            }
            return;
        }
        let replaced = pending.first().cloned();
        match replaced {
            None => {
                let _ = inbox.prepend(seekdeep_agent::InboxTarget::NextStep, desired.clone());
            }
            Some(replaced) => {
                let _ = inbox.replace(replaced.id(), desired.clone());
            }
        }
        for message in pending.iter().skip(1) {
            let _ = inbox.remove(message.id());
        }
    }

    async fn compose_and_sync(
        self: &Arc<Self>,
        agent: &Arc<Agent>,
        signal: &AbortSignal,
        claimed: &[UserMessage],
        touched_paths: &[String],
    ) -> anyhow::Result<()> {
        let pending = agent
            .inbox()
            .next_step()
            .into_iter()
            .filter(is_workspace_context)
            .collect::<Vec<_>>();
        let desired = self
            .compose(agent, signal, claimed, &pending, touched_paths)
            .await?;
        ensure_not_aborted(signal)?;
        InstructionRuntime::sync_inbox(agent, claimed, desired.as_ref());
        Ok(())
    }

    fn queue_projection(self: &Arc<Self>, agent: &Arc<Agent>, touched_path: String) {
        let key = agent_key(agent);
        let previous = self.projection_tails.lock().remove(&key);
        let runtime = self.clone();
        let agent = agent.clone();
        let handle = tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let result = runtime
                .compose_and_sync(&agent, &runtime.projection_lifecycle, &[], &[touched_path])
                .await;
            if let Err(error) = result
                && !runtime.projection_lifecycle.is_aborted()
            {
                tracing::warn!("workspace instruction refresh failed: {error}");
            }
            let mut tails = runtime.projection_tails.lock();
            if tails
                .get(&key)
                .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                tails.remove(&key);
            }
        });
        self.projection_tails.lock().insert(key, handle);
    }

    async fn wait_for_projections(&self, agent: &Arc<Agent>) {
        loop {
            let handle = self.projection_tails.lock().remove(&agent_key(agent));
            let Some(handle) = handle else {
                return;
            };
            let _ = handle.await;
        }
    }

    fn step_is_open(&self, session: &Arc<Session>) -> bool {
        let key = session_key(session);
        if let Some(known) = self.open_steps.lock().get(&key).copied() {
            return known;
        }
        let mut open = false;
        for event in session.events() {
            match event.event_type.as_str() {
                "step/start" => open = true,
                "step/end" | "turn/end" => open = false,
                _ => {}
            }
        }
        self.open_steps.lock().insert(key, open);
        open
    }

    fn project_touch(self: &Arc<Self>, touch: ProjectionTouch) {
        let session = touch.agent.session().clone();
        if !self.step_is_open(&session) {
            self.queue_projection(&touch.agent, touch.path);
            return;
        }
        let key = session_key(&session);
        self.step_touches.lock().entry(key).or_default().push(touch);
    }
}

#[allow(clippy::too_many_lines)]
fn register_listeners(runtime: &Arc<InstructionRuntime>, context: &Context) -> anyhow::Result<()> {
    let session_runtime = runtime.clone();
    context.events().on_sync(
        context,
        "session/event",
        move |_, args| {
            let Some(session) = args.get::<Session>(0) else {
                return Ok(EventReply::Undefined);
            };
            let Some(event) = args.get::<SessionEvent>(1) else {
                return Ok(EventReply::Undefined);
            };
            let key = session_key(&session);
            match event.event_type.as_str() {
                "step/start" => {
                    session_runtime.open_steps.lock().insert(key, true);
                }
                "turn/end" => {
                    session_runtime.open_steps.lock().insert(key, false);
                }
                "step/end" => {
                    session_runtime.open_steps.lock().insert(key, false);
                    let pending = session_runtime.step_touches.lock().remove(&key);
                    if let Some(pending) = pending {
                        for touch in pending {
                            session_runtime.queue_projection(&touch.agent, touch.path);
                        }
                    }
                }
                _ => {}
            }
            Ok(EventReply::Undefined)
        },
        EventOptions::default(),
    )?;

    let pre_step_runtime = runtime.clone();
    context.events().on_waterfall(
        context,
        "agent/pre-step",
        move |_, args, next| {
            let Some(event) = args.get::<AgentEvent<AgentPreStepEvent>>(0) else {
                return Box::pin(async move {
                    Err(anyhow::anyhow!("agent/pre-step lacks its payload"))
                });
            };
            let agent = event.agent.clone();
            let messages = event.payload.messages.clone();
            let step = event.payload.step;
            let signal = event.payload.signal.clone();
            let runtime = pre_step_runtime.clone();
            Box::pin(async move {
                let reply = next.run().await?;
                let decision = reply
                    .downcast::<PreStepDecision>()
                    .map(|decision| (*decision).clone())
                    .ok_or_else(|| anyhow::anyhow!("agent/pre-step returned an invalid decision"))?;
                runtime.wait_for_projections(&agent).await;
                let pending = agent
                    .inbox()
                    .next_step()
                    .into_iter()
                    .filter(is_workspace_context)
                    .collect::<Vec<_>>();
                let desired = runtime.compose(&agent, &signal, &messages, &pending, &[]).await;
                let desired = match desired {
                    Ok(desired) => desired,
                    Err(error) => {
                        if !signal.is_aborted() {
                            tracing::warn!("workspace instruction compose failed: {error}");
                        }
                        None
                    }
                };
                if signal.is_aborted() {
                    anyhow::bail!("agent-instructions aborted");
                }
                if matches!(decision, PreStepDecision::Reject)
                    || (step == 1
                        && matches!(&decision, PreStepDecision::Enter { messages } if messages.is_empty()))
                {
                    InstructionRuntime::sync_inbox(&agent, &messages, desired.as_ref());
                    return Ok(EventReply::Value(Arc::new(decision)));
                }
                for message in &pending {
                    let _ = agent.inbox().remove(message.id());
                }
                let already_entered = matches!(&decision, PreStepDecision::Enter { messages } if messages.iter().any(|message| desired.as_ref().is_some_and(|desired| same_context_payload(message, desired))));
                if desired.is_none() || already_entered {
                    return Ok(EventReply::Value(Arc::new(decision)));
                }
                let PreStepDecision::Enter { messages: entered } = decision else {
                    return Ok(EventReply::Value(Arc::new(PreStepDecision::Reject)));
                };
                let last_claimed_index = entered
                    .iter()
                    .rposition(|message| messages.contains(message))
                    .unwrap_or(entered.len());
                let mut entered = entered;
                entered.insert(last_claimed_index + 1, desired.expect("checked"));
                Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                    messages: entered,
                })))
            })
        },
        EventOptions::default(),
    )?;

    let result_runtime = runtime.clone();
    context.events().on_sync(
        context,
        "tools/result",
        move |_, args| {
            let Some(exec) = args.get::<ToolExecution>(0) else {
                return Ok(EventReply::Undefined);
            };
            let Some(result) = args.get::<ToolExecutionResult>(1) else {
                return Ok(EventReply::Undefined);
            };
            let mut touches = result_runtime
                .execution_touches
                .lock()
                .remove(&exec.token)
                .unwrap_or_default();
            let is_error = matches!(result.as_ref(), ToolExecutionResult::Failure(_));
            if !is_error
                && !exec.signal().is_aborted()
                && let Some(agent) = exec.agent.as_ref()
                && let Some(path) = file_path_from_execution(&exec)
            {
                touches.push(ProjectionTouch {
                    agent: agent.clone(),
                    path,
                });
            }
            if let Some(parent) = exec.parent {
                if !touches.is_empty() {
                    let mut map = result_runtime.execution_touches.lock();
                    map.entry(parent).or_default().extend(touches);
                }
            } else {
                for touch in touches {
                    result_runtime.project_touch(touch);
                }
            }
            Ok(EventReply::Undefined)
        },
        EventOptions::default(),
    )?;
    Ok(())
}

/// Installs the workspace instruction loader.
///
/// # Errors
///
/// Returns configuration resolution or listener registration failures.
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let runtime = InstructionRuntime::new(context, config)?;
    let cleanup_runtime = runtime.clone();
    context.own(seekdeep_cordis::fiber::EffectHandle::new(
        "agent-instructions lifecycle",
        move || {
            Box::pin(async move {
                cleanup_runtime.projection_lifecycle.abort();
                cleanup_runtime.execution_touches.lock().clear();
                Ok(())
            })
        },
    ))?;
    register_listeners(&runtime, context)?;
    Ok(())
}

/// Builds the loader-compatible workspace instruction plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}
