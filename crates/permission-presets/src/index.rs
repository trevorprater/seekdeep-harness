//! User-facing permission presets over the independent sandbox-mode and
//! approval-policy knobs.

use std::sync::Arc;

use indexmap::IndexMap;
use seekdeep_commands::{COMMANDS, CommandDefinition, CommandInvocation, CommandResult};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, ServiceKey};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_core::session_store::SESSIONS;
use seekdeep_sandbox::SandboxMode;
use seekdeep_sandbox_policy::{effective_sandbox_mode, set_sandbox_mode};
use seekdeep_schemastery::Schema;
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SESSION_PROJECTIONS,
};
use seekdeep_settings::{install_settings_section, settings_namespace};
use seekdeep_shell::SHELL;
use seekdeep_user_approval::{
    APPROVAL, ApprovalPolicy, effective_approval_policy, set_approval_policy,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::types::{PermissionSelect, PresetOption};

/// Typed Cordis slot corresponding to ctx.permissionPresets.
pub const PERMISSION_PRESETS: ServiceKey<PermissionPresetService> =
    ServiceKey::new("permissionPresets");

/// Cordis plugin name.
pub const NAME: &str = "permission-presets";

/// Services required by the permission-preset service.
pub const INJECT: &[&str] = &["shell", "approval", "sessions"];

/// Returned when effective knob values match no table entry.
pub const CUSTOM_PRESET: &str = "custom";

/// Settings namespace carrying the default for future sessions.
pub const PERMISSION_SETTINGS_NAMESPACE: &str = "permission";

/// One preset's sandbox/approval bundle and optional client presentation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetSpec {
    /// The sandbox/mode value the preset writes through.
    pub sandbox: SandboxMode,
    /// The approval/policy value the preset writes through.
    pub approval: ApprovalPolicy,
    /// Display label; the raw table key when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// One user-facing sentence; omitted when not configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The permission-preset service config: preset table and composition default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// The preset table in declaration order.
    pub presets: IndexMap<String, PresetSpec>,
    /// Default for new sessions.
    pub default_preset: Option<String>,
}

/// The source-compatible admission schema for Config.
#[must_use]
pub fn config_schema() -> Schema {
    let preset_schema = Schema::object([
        ("sandbox", Schema::string().required()),
        ("approval", Schema::string().required()),
        ("name", Schema::string()),
        ("description", Schema::string()),
    ]);
    Schema::object([
        (
            "presets",
            Schema::dict(preset_schema).with_default(json!({
                "workspace-write": {
                    "sandbox": "workspace-write",
                    "approval": "ask",
                    "name": "workspace-write",
                    "description": "Write inside the workspace and permitted temporary directories; wider retries require approval."
                },
                "danger-full-access": {
                    "sandbox": "danger-full-access",
                    "approval": "never",
                    "name": "danger-full-access",
                    "description": "Full file access without approval prompts."
                }
            })),
        ),
        ("defaultPreset", Schema::string()),
    ])
}

/// The projection unit's state: the last seen value of each knob event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnobState {
    /// Last permission/preset payload, or null.
    pub preset: Option<String>,
    /// Last sandbox/mode payload, or null.
    pub sandbox: Option<SandboxMode>,
    /// Last approval/policy payload, or null.
    pub approval: Option<ApprovalPolicy>,
}

/// User setting resolved when a new session receives its initial permission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSettings {
    /// Preset pinned into a newly created session.
    pub default_preset: String,
}

/// Folds the last selected preset from the durable log.
#[must_use]
pub fn effective_permission_preset(events: &[SessionEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|event| event.event_type == "permission/preset")
        .and_then(|event| event.data.get("preset").and_then(Value::as_str))
        .map(str::to_owned)
}

/// One-event knob transition for the projection unit.
#[must_use]
pub fn apply_knob_event(state: &KnobState, event: &SessionEvent) -> ProjectionTransition {
    let next = match event.event_type.as_str() {
        "permission/preset" => KnobState {
            preset: event
                .data
                .get("preset")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..state.clone()
        },
        "sandbox/mode" => KnobState {
            sandbox: event
                .data
                .get("mode")
                .and_then(Value::as_str)
                .and_then(|mode| serde_json::from_value(json!(mode)).ok()),
            ..state.clone()
        },
        "approval/policy" => KnobState {
            approval: event
                .data
                .get("policy")
                .and_then(Value::as_str)
                .and_then(|policy| serde_json::from_value(json!(policy)).ok()),
            ..state.clone()
        },
        _ => return ProjectionTransition::Unchanged,
    };
    if next == *state {
        ProjectionTransition::Unchanged
    } else {
        ProjectionTransition::changed(next).unwrap_or(ProjectionTransition::Unchanged)
    }
}

fn fold_knobs(events: &[SessionEvent]) -> KnobState {
    let mut state = KnobState::default();
    for event in events {
        if let ProjectionTransition::Changed(next) = apply_knob_event(&state, event)
            && let Ok(next) = serde_json::from_value(next)
        {
            state = next;
        }
    }
    state
}

fn derive_preset(
    presets: &IndexMap<String, PresetSpec>,
    state: &KnobState,
    sandbox_default: Option<SandboxMode>,
    approval_default: Option<ApprovalPolicy>,
) -> String {
    let sandbox = state.sandbox.or(sandbox_default);
    let approval = state.approval.or(approval_default);
    let matches =
        |spec: &PresetSpec| Some(spec.sandbox) == sandbox && Some(spec.approval) == approval;
    if let Some(preset) = &state.preset
        && let Some(spec) = presets.get(preset)
        && matches(spec)
    {
        return preset.clone();
    }
    presets
        .iter()
        .find_map(|(name, spec)| matches(spec).then(|| name.clone()))
        .unwrap_or_else(|| CUSTOM_PRESET.to_owned())
}

/// Owns the deployment's permission presets and their write path.
pub struct PermissionPresetService {
    context: Context,
    presets: IndexMap<String, PresetSpec>,
    default_settings_source: seekdeep_settings::SettingsSectionSource,
}

impl PermissionPresetService {
    /// Builds, validates, and publishes the service.
    ///
    /// # Errors
    ///
    /// Returns reserved-name, unconfined-executor, invalid-default, or
    /// duplicate-service failures.
    pub fn new(context: &Context, config: &Config) -> anyhow::Result<Arc<Self>> {
        let presets = config.presets.clone();
        anyhow::ensure!(
            !presets.contains_key(CUSTOM_PRESET),
            "permission: \"{CUSTOM_PRESET}\" is reserved for the derived not-a-preset state and cannot name a table entry"
        );
        let shell = context
            .get(SHELL)
            .ok_or_else(|| anyhow::anyhow!("permission-presets requires shell"))?;
        anyhow::ensure!(
            shell.sandbox_mode().is_some(),
            "permission: the mounted bash executor does not confine (no sandboxMode) — presets bundle a sandbox mode, so composing this plugin over an unconfined executor is a misconfiguration"
        );
        let approval = context
            .get(APPROVAL)
            .ok_or_else(|| anyhow::anyhow!("permission-presets requires approval"))?;
        let inferred_default = derive_preset(
            &presets,
            &KnobState::default(),
            shell.sandbox_mode(),
            Some(approval.policy()),
        );
        let default_preset = config
            .default_preset
            .clone()
            .unwrap_or(inferred_default.clone());
        anyhow::ensure!(
            default_preset != CUSTOM_PRESET,
            "permission: composed sandbox and approval defaults match no preset; configure defaultPreset explicitly"
        );
        anyhow::ensure!(
            presets.contains_key(&default_preset),
            "permission: unknown preset \"{default_preset}\" (known: {})",
            presets.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        let ns = settings_namespace(PERMISSION_SETTINGS_NAMESPACE)?;
        let choices = Schema::union(presets.keys().cloned().map(Schema::constant)).required();
        let installed = install_settings_section(
            context,
            &ns,
            Schema::object([("defaultPreset", choices)]),
            json!({"defaultPreset": default_preset}),
            None,
            Arc::new(|| Ok(())),
        )?;
        let service = Arc::new(Self {
            context: context.clone(),
            presets,
            default_settings_source: installed.source,
        });
        context.provide(PERMISSION_PRESETS, service.clone())?;
        Ok(service)
    }

    /// The advertised preset names, in declaration order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.presets.keys().cloned().collect()
    }

    /// The preset currently selected as the default for future sessions.
    #[must_use]
    pub fn default_preset(&self) -> String {
        let value = self.default_settings_source.get();
        serde_json::from_value::<PermissionSettings>(value).map_or_else(
            |_| "workspace-write".to_owned(),
            |settings| settings.default_preset,
        )
    }

    /// Resolves the preset matching the effective knob values.
    #[must_use]
    pub fn current(&self, events: &[SessionEvent]) -> String {
        self.derive(&fold_knobs(events))
    }

    fn derive(&self, state: &KnobState) -> String {
        let shell_default = self
            .context
            .get(SHELL)
            .and_then(|shell| shell.sandbox_mode());
        let approval_default = self.context.get(APPROVAL).map(|approval| approval.policy());
        derive_preset(&self.presets, state, shell_default, approval_default)
    }

    /// Builds the whole select value for one folded knob state.
    #[must_use]
    pub fn select_for(&self, state: &KnobState) -> PermissionSelect {
        let current_value = self.derive(state);
        let mut options: Vec<PresetOption> = self
            .names()
            .iter()
            .map(|name| self.option_of(name))
            .collect();
        if current_value == CUSTOM_PRESET {
            options.push(self.option_of(CUSTOM_PRESET));
        }
        PermissionSelect {
            options,
            current_value,
        }
    }

    /// Resolves a preset's knob bundle.
    ///
    /// # Errors
    ///
    /// Returns when name is not in the table.
    pub fn resolve(&self, name: &str) -> anyhow::Result<PresetSpec> {
        self.presets.get(name).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "permission: unknown preset \"{name}\" (known: {})",
                self.names().join(", ")
            )
        })
    }

    /// Builds the client option for a table entry or custom.
    ///
    /// # Panics
    ///
    /// Panics when name is neither a table key nor custom.
    pub fn option_of(&self, name: &str) -> PresetOption {
        if name == CUSTOM_PRESET {
            return PresetOption {
                value: CUSTOM_PRESET.to_owned(),
                name: "Custom".to_owned(),
                description: Some(
                    "Current sandbox and approval settings do not match a preset.".to_owned(),
                ),
            };
        }
        let spec = self
            .presets
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("permission: unknown preset \"{name}\""));
        PresetOption {
            value: name.to_owned(),
            name: spec.name.clone().unwrap_or_else(|| name.to_owned()),
            description: spec.description,
        }
    }

    /// Records a changed preset, then updates each changed knob through its own setter.
    ///
    /// # Errors
    ///
    /// Returns unknown-preset or setter failures.
    pub fn set(&self, session: &Arc<Session>, name: &str) -> anyhow::Result<()> {
        let policy_writer =
            |policy: ApprovalPolicy| set_approval_policy(session, policy).map(|_| ());
        self.apply(session, name, &policy_writer)
    }

    fn apply(
        &self,
        session: &Arc<Session>,
        name: &str,
        set_approval: &dyn Fn(ApprovalPolicy) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let spec = self.resolve(name)?;
        if self.current(&session.events()) != name {
            session.append(
                "permission/preset",
                json!({"preset": name}),
                seekdeep_core::session::AppendOptions::default(),
            )?;
        }
        let events = session.events();
        let shell_default = self
            .context
            .get(SHELL)
            .and_then(|shell| shell.sandbox_mode());
        if Some(spec.sandbox) != effective_sandbox_mode(&events).or(shell_default) {
            set_sandbox_mode(session, spec.sandbox)?;
        }
        let approval_default = self.context.get(APPROVAL).map(|approval| approval.policy());
        if Some(spec.approval) != effective_approval_policy(&events).or(approval_default) {
            set_approval(spec.approval)?;
        }
        Ok(())
    }

    /// Fills every missing permission fact before a session is published.
    ///
    /// # Errors
    ///
    /// Returns unknown-preset or setter failures.
    pub fn pin_initial_permission(&self, session: &Arc<Session>) -> anyhow::Result<()> {
        let events = session.events();
        let selected = effective_permission_preset(&events);
        let sandbox = effective_sandbox_mode(&events);
        let approval = effective_approval_policy(&events);
        let seeded = events
            .iter()
            .any(|event| event.event_type == "session/end-seed");
        if selected.is_none() && sandbox.is_none() && approval.is_none() && !seeded {
            let name = self.default_preset();
            let spec = self.resolve(&name)?;
            session.append(
                "permission/preset",
                json!({"preset": name}),
                seekdeep_core::session::AppendOptions::default(),
            )?;
            set_sandbox_mode(session, spec.sandbox)?;
            set_approval_policy(session, spec.approval)?;
            return Ok(());
        }
        let state = KnobState {
            preset: selected.clone(),
            sandbox,
            approval,
        };
        let effective = self.derive(&state);
        if selected.is_none() && effective != CUSTOM_PRESET {
            session.append(
                "permission/preset",
                json!({"preset": effective}),
                seekdeep_core::session::AppendOptions::default(),
            )?;
        }
        if sandbox.is_none()
            && let Some(mode) = self
                .context
                .get(SHELL)
                .and_then(|shell| shell.sandbox_mode())
        {
            set_sandbox_mode(session, mode)?;
        }
        if approval.is_none() {
            let policy = self
                .context
                .get(APPROVAL)
                .map_or(ApprovalPolicy::Ask, |approval| approval.policy());
            set_approval_policy(session, policy)?;
        }
        Ok(())
    }

    fn register_children(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        if let Some(projections) = context.get(SESSION_PROJECTIONS) {
            let service = self.clone();
            projections.register(
                context,
                ProjectionDefinition::new(
                    "permissions",
                    1,
                    || Ok(serde_json::to_value(KnobState::default())?),
                    move |state, event| {
                        let state: KnobState = serde_json::from_value(state.clone())?;
                        Ok(apply_knob_event(&state, event))
                    },
                    move |state| {
                        let state: KnobState = serde_json::from_value(state.clone())?;
                        Ok(serde_json::to_value(service.select_for(&state))?)
                    },
                ),
            )?;
        }
        if let Some(commands) = context.get(COMMANDS) {
            let service = self.clone();
            commands.register(
                context,
                CommandDefinition::new(
                    "permission",
                    "Switch the permission preset (sandbox mode + approval policy)",
                    Arc::new(move |invocation: CommandInvocation| {
                        let service = service.clone();
                        Box::pin(async move {
                            let name = invocation.raw_input.trim();
                            let result = if name.is_empty() {
                                CommandResult::success(Some(format!(
                                    "current preset {} (available: {})",
                                    service.current(&invocation.agent.session().events()),
                                    service.names().join(", ")
                                )))
                            } else if !service.names().contains(&name.to_owned()) {
                                CommandResult::error(format!(
                                    "unknown preset \"{name}\" (available: {})",
                                    service.names().join(", ")
                                ))
                            } else {
                                let switched = service
                                    .context
                                    .get(APPROVAL)
                                    .ok_or_else(|| {
                                        anyhow::anyhow!("permission-presets requires approval")
                                    })
                                    .and_then(|approval| {
                                        service.apply(invocation.agent.session(), name, &|policy| {
                                            approval.set_policy(invocation.agent.as_ref(), policy)
                                        })
                                    });
                                match switched {
                                    Ok(()) => {
                                        CommandResult::success(Some(format!("preset {name}")))
                                    }
                                    Err(error) => CommandResult::error(error.to_string()),
                                }
                            };
                            Ok(result)
                        })
                    }),
                )
                .with_input("<preset>"),
            )?;
        }
        Ok(())
    }
}

/// Installs the permission-preset service and its optional children.
///
/// # Errors
///
/// Returns service construction, listener, or child-registration failures.
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let service = PermissionPresetService::new(context, config)?;
    service.register_children(context)?;

    let session_service = service.clone();
    context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            let Some(session) = args.get::<Session>(0) else {
                return Ok(EventReply::Undefined);
            };
            if let Err(error) = session_service.pin_initial_permission(&session) {
                tracing::warn!("permission-presets: could not pin initial permission: {error}");
            }
            Ok(EventReply::Undefined)
        },
        EventOptions::default(),
    )?;

    if let Some(store) = context.get(SESSIONS) {
        for session in store.list() {
            if let Err(error) = service.pin_initial_permission(&session) {
                tracing::warn!("permission-presets: could not pin initial permission: {error}");
            }
        }
    }
    Ok(())
}

/// Builds the loader-compatible permission-preset plugin.
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
