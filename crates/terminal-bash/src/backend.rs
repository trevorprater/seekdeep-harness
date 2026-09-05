//! Shared-policy Bash terminal backend and sandbox-mode lifetime fence.

use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, OnceLock, Weak},
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::Agent;
use seekdeep_cordis::{
    Context, DispatchMode, EventArgs, EventOptions, EventReply, Plugin, fiber::EffectHandle,
};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::{SANDBOX, SandboxMode, SandboxPolicy};
use seekdeep_sandbox_policy::{SANDBOX_POLICY, SandboxPolicyRequest, SandboxPolicyService};
use seekdeep_subprocess::{SUBPROCESS, SubprocessTerminalHandleRef, SubprocessTerminalSpawnSpec};
use seekdeep_terminal::{
    TERMINALS, TerminalBackend, TerminalBackendCleanupError, TerminalBackendSession,
    TerminalBackendSessionRef, TerminalBackendSpawnSpec, TerminalFailure, TerminalResult,
    TerminalSessionService, abort_failure,
};

use crate::{
    LocalPtySession,
    config::{ResolvedTerminalBashConfig, TerminalBashConfig},
    sanitize::CONTROLLED_PROMPT,
};

/// Cordis plugin name.
pub const NAME: &str = "terminal-bash";
/// Required services in source order.
pub const INJECT: &[&str] = &["terminals", "sandboxPolicy", "subprocess"];

type SpawnTerminalFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<SubprocessTerminalHandleRef>> + Send + 'static>>;
type SpawnTerminal =
    Arc<dyn Fn(SubprocessTerminalSpawnSpec) -> SpawnTerminalFuture + Send + Sync + 'static>;
type CreateSession = Arc<
    dyn Fn(SubprocessTerminalHandleRef, ResolvedTerminalBashConfig) -> BashPtySessionRef
        + Send
        + Sync
        + 'static,
>;

/// Startup-capable session seam used by the backend's rollback boundary.
#[async_trait]
pub trait BashPtySession: TerminalBackendSession {
    /// Captures the startup MOTD through the ordinary readiness protocol.
    async fn initialize(&self, signal: Option<AbortSignal>) -> TerminalResult<()>;
}

#[async_trait]
impl BashPtySession for LocalPtySession {
    async fn initialize(&self, signal: Option<AbortSignal>) -> TerminalResult<()> {
        LocalPtySession::initialize(self, signal).await
    }
}

/// Shared startup-capable backend session.
pub type BashPtySessionRef = Arc<dyn BashPtySession>;

#[derive(Clone)]
struct FenceServices {
    terminals: Arc<TerminalSessionService>,
    sandbox_policy: Arc<SandboxPolicyService>,
}

struct SandboxModeFenceState {
    services: Mutex<FenceServices>,
}

fn fences() -> &'static Mutex<HashMap<usize, Weak<SandboxModeFenceState>>> {
    static FENCES: OnceLock<Mutex<HashMap<usize, Weak<SandboxModeFenceState>>>> = OnceLock::new();
    FENCES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn owner_key(owner: &Arc<Agent>) -> usize {
    Arc::as_ptr(owner) as usize
}

fn ensure_sandbox_mode_fence(
    owner: &Arc<Agent>,
    terminals: Arc<TerminalSessionService>,
    sandbox_policy: Arc<SandboxPolicyService>,
) -> TerminalResult<()> {
    let key = owner_key(owner);
    if let Some(existing) = fences().lock().get(&key).and_then(Weak::upgrade) {
        *existing.services.lock() = FenceServices {
            terminals,
            sandbox_policy,
        };
        return Ok(());
    }
    let state = Arc::new(SandboxModeFenceState {
        services: Mutex::new(FenceServices {
            terminals,
            sandbox_policy,
        }),
    });
    fences().lock().insert(key, Arc::downgrade(&state));

    let weak_owner = Arc::downgrade(owner);
    owner
        .context()
        .events()
        .on_sync(
            owner.context(),
            "internal/dispatch",
            move |_, args| {
                fence_internal_dispatch(&weak_owner, &state, &args)?;
                Ok(EventReply::Undefined)
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .map_err(|error| TerminalFailure::message(error.to_string()))?;
    Ok(())
}

fn fence_internal_dispatch(
    weak_owner: &Weak<Agent>,
    state: &SandboxModeFenceState,
    args: &EventArgs,
) -> anyhow::Result<()> {
    args.get::<DispatchMode>(0)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks a dispatch mode"))?;
    let event_name = args
        .get::<String>(1)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
    if event_name.as_str() != "session/event" {
        return Ok(());
    }
    let carried = args
        .get::<EventArgs>(2)
        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
    let session = carried
        .get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its session"))?;
    let event = carried
        .get::<SessionEvent>(1)
        .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
    let Some(owner) = weak_owner.upgrade() else {
        return Ok(());
    };
    if !Arc::ptr_eq(&session, owner.session()) || event.event_type != "sandbox/mode" {
        return Ok(());
    }
    let services = state.services.lock().clone();
    let current_mode = services
        .sandbox_policy
        .override_of(&session)
        .unwrap_or(services.sandbox_policy.default_mode);
    let proposed_mode = event
        .data
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("undefined");
    if proposed_mode == current_mode.as_str() || !services.terminals.has_owner_activity(&owner) {
        return Ok(());
    }
    anyhow::bail!(
        "cannot change sandbox mode from \"{current_mode}\" to \"{proposed_mode}\" while persistent terminal sessions are open or being created; wait for creation to settle and close them first"
    )
}

#[allow(clippy::cast_precision_loss)]
fn safe_integer_as_f64(value: u64) -> f64 {
    // ResolvedTerminalBashConfig originates from a validated JavaScript safe
    // integer, so this round-trip is exact even though arbitrary u64 values
    // would not be.
    value as f64
}

/// Exact deliberate environment layered after the subprocess provider's scrubbed base.
#[must_use]
pub fn child_environment(spec: &TerminalBackendSpawnSpec) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("TERM".to_owned(), "dumb".to_owned()),
        ("PAGER".to_owned(), "cat".to_owned()),
        ("GIT_PAGER".to_owned(), "cat".to_owned()),
        ("PS1".to_owned(), CONTROLLED_PROMPT.to_owned()),
        (
            "PROMPT_COMMAND".to_owned(),
            "printf \"\\033]133;D;%s\\007\" \"$?\"".to_owned(),
        ),
        (
            "BASH_SILENCE_DEPRECATION_WARNING".to_owned(),
            "1".to_owned(),
        ),
        ("SEEKDEEP_SHELL".to_owned(), "1".to_owned()),
        (
            "SEEKDEEP_SESSION_ID".to_owned(),
            spec.owner.id().as_str().to_owned(),
        ),
        (
            "SEEKDEEP_PTY_SESSION_ID".to_owned(),
            spec.session_id.as_str().to_owned(),
        ),
    ])
}

/// Local shell backend registered under a configured terminal type.
pub struct BashTerminalBackend {
    context: Context,
    config: ResolvedTerminalBashConfig,
    spawn_terminal: SpawnTerminal,
    create_session: CreateSession,
}

impl std::fmt::Debug for BashTerminalBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BashTerminalBackend")
            .field("backend_type", &self.config.backend_type)
            .finish_non_exhaustive()
    }
}

impl BashTerminalBackend {
    /// Creates the production backend over the current subprocess service.
    #[must_use]
    pub fn new(context: Context, config: ResolvedTerminalBashConfig) -> Arc<Self> {
        let lookup = context.clone();
        Self::with_factories(
            context,
            config,
            move |spec| {
                let lookup = lookup.clone();
                async move {
                    let subprocess = lookup.get(SUBPROCESS).ok_or_else(|| {
                        anyhow::anyhow!("terminal-bash: ctx.subprocess is unavailable")
                    })?;
                    subprocess.spawn_terminal(spec).await
                }
            },
            |terminal, config| {
                let session: BashPtySessionRef = LocalPtySession::new(terminal, config);
                session
            },
        )
    }

    /// Constructs the backend over deterministic test seams.
    #[doc(hidden)]
    #[must_use]
    pub fn with_factories<F, Fut, G>(
        context: Context,
        config: ResolvedTerminalBashConfig,
        spawn_terminal: F,
        create_session: G,
    ) -> Arc<Self>
    where
        F: Fn(SubprocessTerminalSpawnSpec) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<SubprocessTerminalHandleRef>> + Send + 'static,
        G: Fn(SubprocessTerminalHandleRef, ResolvedTerminalBashConfig) -> BashPtySessionRef
            + Send
            + Sync
            + 'static,
    {
        Arc::new(Self {
            context,
            config,
            spawn_terminal: Arc::new(move |spec| Box::pin(spawn_terminal(spec))),
            create_session: Arc::new(create_session),
        })
    }

    fn spawn_argv(
        &self,
        policy: seekdeep_sandbox::SandboxExecutionPolicy,
    ) -> TerminalResult<Vec<String>> {
        let argv = std::iter::once(self.config.shell_path.clone())
            .chain(self.config.shell_args.iter().cloned())
            .collect::<Vec<_>>();
        if policy.mode == SandboxMode::DangerFullAccess {
            return Ok(argv);
        }
        let sandbox = self.context.get(SANDBOX).ok_or_else(|| {
            TerminalFailure::message(format!(
                "terminal-bash: sandbox mode \"{}\" requires a ctx.sandbox provider in the execution world",
                policy.mode
            ))
        })?;
        let policy = SandboxPolicy::try_from(policy)
            .map_err(|error| TerminalFailure::message(error.to_string()))?;
        sandbox
            .confine(&argv, &policy)
            .map(|confined| confined.argv)
            .map_err(TerminalFailure::from_anyhow)
    }

    async fn initialize_session(
        session: BashPtySessionRef,
        signal: Option<AbortSignal>,
    ) -> TerminalResult<()> {
        let Some(signal) = signal else {
            return session.initialize(None).await;
        };
        if signal.is_aborted() {
            return Err(abort_failure(&signal));
        }
        let initialized_session = session;
        let initialized_signal = signal.clone();
        let mut initialization = Box::pin(async move {
            initialized_session
                .initialize(Some(initialized_signal))
                .await
        });
        // JavaScript invokes `session.initialize(signal)` before constructing
        // `Promise.race`, so its synchronous prefix reserves the startup send
        // before an output callback can consume the first prompt. Poll once in
        // this task before detaching the still-pending future to preserve that
        // ordering while still allowing cancellation to win promptly.
        if let std::task::Poll::Ready(result) = futures::poll!(&mut initialization) {
            return result;
        }
        let mut initializing = tokio::spawn(initialization);
        tokio::select! {
            biased;
            result = &mut initializing => result.map_err(|error| TerminalFailure::message(error.to_string()))?,
            () = signal.cancelled() => Err(abort_failure(&signal)),
        }
    }
}

#[async_trait]
impl TerminalBackend for BashTerminalBackend {
    fn backend_type(&self) -> &str {
        &self.config.backend_type
    }

    async fn spawn(
        &self,
        spec: TerminalBackendSpawnSpec,
    ) -> TerminalResult<TerminalBackendSessionRef> {
        if let Some(signal) = &spec.signal
            && signal.is_aborted()
        {
            return Err(abort_failure(signal));
        }
        let terminals = self.context.get(TERMINALS).ok_or_else(|| {
            TerminalFailure::message("terminal-bash: ctx.terminals is unavailable")
        })?;
        let sandbox_policy = self.context.get(SANDBOX_POLICY).ok_or_else(|| {
            TerminalFailure::message("terminal-bash: ctx.sandboxPolicy is unavailable")
        })?;
        ensure_sandbox_mode_fence(&spec.owner, terminals, sandbox_policy.clone())?;
        let policy = sandbox_policy
            .resolve(SandboxPolicyRequest {
                session: Some(spec.owner.session()),
                mode: None,
            })
            .map_err(TerminalFailure::from_anyhow)?;
        let argv = self.spawn_argv(policy.clone())?;
        if argv.is_empty() {
            return Err(TerminalFailure::message(
                "terminal-bash: sandbox returned empty argv",
            ));
        }
        let terminal = (self.spawn_terminal)(SubprocessTerminalSpawnSpec {
            argv,
            cwd: spec
                .cwd
                .as_ref()
                .map_or(policy.workspace_root, PathBuf::from),
            env: Some(child_environment(&spec)),
            rows: self.config.rows,
            cols: self.config.cols,
            grace_ms: safe_integer_as_f64(self.config.dispose_grace_ms),
            signal: spec.signal.clone(),
        })
        .await
        .map_err(TerminalFailure::from_anyhow)?;
        let session = (self.create_session)(terminal, self.config.clone());
        if let Err(spawn_error) =
            Self::initialize_session(session.clone(), spec.signal.clone()).await
        {
            if let Err(cleanup_error) = session.close("PTY startup failed").await {
                return Err(TerminalFailure::new(TerminalBackendCleanupError::new(
                    spawn_error,
                    cleanup_error,
                )));
            }
            return Err(spawn_error);
        }
        let published: TerminalBackendSessionRef = session;
        Ok(published)
    }
}

/// Validates config and registers one reversible backend contribution.
///
/// # Errors
///
/// Returns configuration, missing dependency, duplicate type, or ownership failures.
pub fn apply(context: &Context, config: &TerminalBashConfig) -> anyhow::Result<EffectHandle> {
    let resolved = config.resolve()?;
    let terminals = context
        .get(TERMINALS)
        .ok_or_else(|| anyhow::anyhow!("terminal-bash requires terminals"))?;
    let backend = BashTerminalBackend::new(context.clone(), resolved);
    let backend: Arc<dyn TerminalBackend> = backend;
    terminals
        .register_backend(context, &backend)
        .map_err(anyhow::Error::from)
}

fn validate_plugin_config(value: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let config: TerminalBashConfig = serde_json::from_value(value.clone())?;
    config.resolve()?;
    Ok(value.clone())
}

/// Builds the Loader-compatible dependency-managed plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: TerminalBashConfig = serde_json::from_value(config)?;
            apply(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(validate_plugin_config)
}
