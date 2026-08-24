#![allow(dead_code)]

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentCancelCause, AgentControlError, AgentController, AgentOptions, CancelOptions,
    Inbox, InboxTarget, MaintenanceReservation, NoopInboxNotifications,
};
use seekdeep_cordis::{Context, PluginFiber};
use seekdeep_core::{
    session::{Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{AbortSignal, ContentBlock, UserMessage};
use seekdeep_permission_presets::{PERMISSION_PRESETS, PermissionPresetService};
use seekdeep_sandbox::SandboxMode;
use seekdeep_scope::ScopeKey;
use seekdeep_settings::{SettingsDocument, SettingsNamespace, SettingsService, SettingsStorage};
use seekdeep_shell::{
    ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcessHandle, ShellRunResult,
    ShellService,
};
use seekdeep_user_approval::{ApprovalConfig, ApprovalPolicy, ApprovalService};
use serde_json::{Map, Value, json};

#[derive(Debug)]
struct StubShell(Option<SandboxMode>);

#[async_trait]
impl ShellExecutor for StubShell {
    fn sandbox_mode(&self) -> Option<SandboxMode> {
        self.0
    }

    fn resolve(&self, _request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
        anyhow::bail!("permission tests do not execute shell commands")
    }

    async fn run(&self, _spec: ShellExecSpec) -> anyhow::Result<ShellRunResult> {
        anyhow::bail!("permission tests do not execute shell commands")
    }

    fn start(&self, _spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        anyhow::bail!("permission tests do not execute shell commands")
    }
}

#[derive(Debug, Default)]
pub(crate) struct MemorySettings {
    document: Mutex<SettingsDocument>,
}

#[async_trait]
impl SettingsStorage for MemorySettings {
    fn writable(&self) -> bool {
        true
    }

    fn document_path(&self) -> Option<&Path> {
        None
    }

    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        Ok(self.document.lock().clone())
    }

    async fn persist(
        &self,
        namespace: &SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        self.document.lock().insert(
            namespace.as_str().to_owned(),
            Value::Object(section.clone()),
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SentMessage {
    pub(crate) text: String,
    pub(crate) target: InboxTarget,
    pub(crate) wakeup: bool,
}

#[derive(Default)]
pub(crate) struct RecordingController {
    pub(crate) sent: Mutex<Vec<SentMessage>>,
}

impl AgentController for RecordingController {
    fn send(
        &self,
        message: UserMessage,
        target: InboxTarget,
        wakeup: bool,
    ) -> Result<(), AgentControlError> {
        let text = match message.content() {
            [ContentBlock::Text { text }] => text.clone(),
            content => format!("{content:?}"),
        };
        self.sent.lock().push(SentMessage {
            text,
            target,
            wakeup,
        });
        Ok(())
    }

    fn cancel(
        &self,
        _cause: AgentCancelCause,
        _options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        Ok(())
    }

    fn when_idle(&self) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError> {
        Ok(MaintenanceReservation::new(
            AbortSignal::default(),
            Arc::new(|| {}),
        ))
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MountOptions {
    pub(crate) shell_mode: Option<SandboxMode>,
    pub(crate) approval_policy: ApprovalPolicy,
    pub(crate) with_settings: bool,
    pub(crate) with_projections: bool,
    pub(crate) with_commands: bool,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            shell_mode: Some(SandboxMode::WorkspaceWrite),
            approval_policy: ApprovalPolicy::Ask,
            with_settings: false,
            with_projections: false,
            with_commands: false,
        }
    }
}

pub(crate) struct Harness {
    pub(crate) context: Context,
    pub(crate) sessions: Arc<SessionStore>,
    pub(crate) service: Arc<PermissionPresetService>,
    pub(crate) settings: Option<Arc<SettingsService>>,
    pub(crate) plugin: Arc<PluginFiber>,
}

pub(crate) struct BaseHarness {
    pub(crate) context: Context,
    pub(crate) sessions: Arc<SessionStore>,
    pub(crate) settings: Option<Arc<SettingsService>>,
}

pub(crate) async fn base(options: MountOptions) -> anyhow::Result<BaseHarness> {
    let context = Context::new();
    let sessions = SessionStore::install(&context)?;
    let settings = if options.with_settings {
        Some(SettingsService::install(&context, Arc::new(MemorySettings::default())).await?)
    } else {
        None
    };
    if options.with_projections {
        seekdeep_session_projection::SessionProjectionRegistry::install(&context)?;
    }
    if options.with_commands {
        seekdeep_commands::install(&context)?;
    }
    ShellService::new(Arc::new(StubShell(options.shell_mode))).provide(&context)?;
    ApprovalService::new(
        context.clone(),
        ApprovalConfig {
            policy: options.approval_policy,
        },
    )
    .provide(&context)?;
    Ok(BaseHarness {
        context,
        sessions,
        settings,
    })
}

pub(crate) async fn mount_permission(base: BaseHarness, config: Value) -> anyhow::Result<Harness> {
    let BaseHarness {
        context,
        sessions,
        settings,
    } = base;
    let plugin = context.plugin(seekdeep_permission_presets::plugin(), config)?;
    plugin.await_settled().await?;
    let service = context
        .get(PERMISSION_PRESETS)
        .ok_or_else(|| anyhow::anyhow!("permission service did not activate"))?;
    Ok(Harness {
        context,
        sessions,
        service,
        settings,
        plugin,
    })
}

pub(crate) async fn mount(config: Value, options: MountOptions) -> anyhow::Result<Harness> {
    mount_permission(base(options).await?, config).await
}

pub(crate) fn fresh_session(id: &str) -> Arc<Session> {
    Session::create(&SessionId::new(id), None, None).unwrap()
}

pub(crate) fn create_session(
    harness: &Harness,
    id: &str,
    seed: Option<Vec<seekdeep_core::session::SessionEvent>>,
) -> Arc<Session> {
    harness
        .sessions
        .create(
            &harness.context,
            Some(SessionId::new(id)),
            CreateSessionOptions {
                seed,
                ..CreateSessionOptions::default()
            },
        )
        .unwrap()
}

pub(crate) fn agent(session: Arc<Session>) -> (Arc<Agent>, Arc<RecordingController>) {
    let inbox = Arc::new(
        Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("agent inbox"),
    );
    let agent = Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        Context::new(),
        ScopeKey::new(),
    ));
    let controller = Arc::new(RecordingController::default());
    agent
        .install_controller(controller.clone())
        .expect("install controller");
    (agent, controller)
}

pub(crate) fn event_pairs(session: &Session) -> Vec<(String, Value)> {
    session
        .events()
        .into_iter()
        .map(|event| (event.event_type, event.data))
        .collect()
}

pub(crate) fn default_config() -> Value {
    json!({})
}
