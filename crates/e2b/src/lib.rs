//! Shared lifecycle ownership of one E2B sandbox.

mod types;

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use futures::{FutureExt as _, future::Shared};
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use serde::{Deserialize, Serialize};

pub use types::*;

/// Cordis service slot for the shared sandbox owner.
pub const E2B: ServiceKey<E2bService> = ServiceKey::new("e2b");
/// Cordis service slot for the SDK factory binding.
pub const E2B_FACTORY: ServiceKey<E2bFactoryService> = ServiceKey::new("e2bFactory");
/// Cordis plugin name.
pub const NAME: &str = "e2b";
/// Required services.
pub const INJECT: &[&str] = &["e2bFactory"];

static NEXT_CONTROL_HOME: AtomicU64 = AtomicU64::new(1);

/// Quotes one opaque argument for E2B's unavoidable login-shell layer.
#[must_use]
pub fn quote_e2b_shell_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Returns a fresh control environment whose HOME cannot be overridden.
#[must_use]
pub fn e2b_control_envs(overrides: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut environment = overrides.clone();
    environment.insert(
        "HOME".to_owned(),
        format!(
            "/.seekdeep-e2b-control-{}",
            NEXT_CONTROL_HOME.fetch_add(1, Ordering::Relaxed)
        ),
    );
    environment
}

/// Shared E2B owner configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct E2bConfig {
    /// API key; omission reads `E2B_API_KEY`.
    pub api_key: Option<String>,
    /// Shared remote working directory.
    pub cwd: String,
    /// Sandbox lifetime in milliseconds.
    pub timeout_ms: f64,
}

impl Default for E2bConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            cwd: "/home/user/workspace".to_owned(),
            timeout_ms: 300_000.0,
        }
    }
}

/// Concrete SDK factory service binding.
pub struct E2bFactoryService(Arc<dyn E2bSandboxFactory>);

impl std::fmt::Debug for E2bFactoryService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("E2bFactoryService")
            .field(&"dyn E2bSandboxFactory")
            .finish()
    }
}

impl E2bFactoryService {
    /// Wraps one SDK factory.
    #[must_use]
    pub fn new(factory: Arc<dyn E2bSandboxFactory>) -> Arc<Self> {
        Arc::new(Self(factory))
    }

    /// Provides the factory for the caller's lifetime.
    ///
    /// # Errors
    ///
    /// Returns duplicate-Service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(E2B_FACTORY, self.clone())?)
    }
}

type ReadySandbox =
    Shared<futures::future::BoxFuture<'static, Result<Arc<dyn E2bSandbox>, Arc<str>>>>;

/// Shared lazily consumed SDK handle and remote execution-world metadata.
pub struct E2bService {
    cwd: String,
    runtime_root: String,
    get_sandbox: Arc<dyn Fn() -> E2bSandboxFuture + Send + Sync>,
}

impl std::fmt::Debug for E2bService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("E2bService")
            .field("cwd", &self.cwd)
            .field("runtime_root", &self.runtime_root)
            .finish_non_exhaustive()
    }
}

impl E2bService {
    /// Creates a manually supplied service, primarily for composed adapters and tests.
    #[must_use]
    pub fn new(
        cwd: impl Into<String>,
        get_sandbox: Arc<dyn Fn() -> E2bSandboxFuture + Send + Sync>,
    ) -> Arc<Self> {
        let cwd = cwd.into();
        Arc::new(Self {
            runtime_root: posix_join(&cwd, ".seekdeep-e2b"),
            cwd,
            get_sandbox,
        })
    }

    /// Shared remote working directory.
    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// Adapter-private remote runtime directory.
    #[must_use]
    pub fn runtime_root(&self) -> &str {
        &self.runtime_root
    }

    /// Returns the shared live sandbox.
    ///
    /// # Errors
    ///
    /// Returns creation, setup, or disposal-race failures.
    pub async fn get_sandbox(&self) -> anyhow::Result<Arc<dyn E2bSandbox>> {
        (self.get_sandbox)().await
    }
}

/// Installs one eagerly opening, lifecycle-owned E2B sandbox.
///
/// # Errors
///
/// Returns configuration, factory, setup, or Cordis registration failures.
pub fn install(context: &Context, config: E2bConfig) -> anyhow::Result<Arc<E2bService>> {
    let factory = context
        .get(E2B_FACTORY)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-e2b requires e2bFactory"))?;
    let api_key = config
        .api_key
        .or_else(|| std::env::var("E2B_API_KEY").ok())
        .unwrap_or_default();
    anyhow::ensure!(
        !api_key.is_empty(),
        "seekdeep-e2b: configure apiKey or set E2B_API_KEY"
    );
    anyhow::ensure!(
        config.cwd.starts_with('/'),
        "seekdeep-e2b: cwd must be an absolute Linux path: {}",
        config.cwd
    );
    anyhow::ensure!(
        config.timeout_ms.is_finite() && config.timeout_ms > 0.0,
        "seekdeep-e2b: timeoutMs must be a positive finite number"
    );
    let timeout_ms = config.timeout_ms;
    let cwd = config.cwd;
    let runtime_root = posix_join(&cwd, ".seekdeep-e2b");
    let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
    let open_factory = factory.0.clone();
    let open_cwd = cwd.clone();
    let open_runtime_root = runtime_root.clone();
    tokio::spawn(async move {
        let result = open(
            open_factory,
            E2bCreateOptions {
                api_key,
                timeout_ms,
                secure: true,
                kill_on_timeout: true,
            },
            &open_cwd,
            &open_runtime_root,
        )
        .await
        .map_err(|error| Arc::<str>::from(format!("{error:#}")));
        let _ = ready_sender.send(result);
    });
    let ready: ReadySandbox = async move {
        ready_receiver
            .await
            .map_err(|_| Arc::<str>::from("E2B sandbox setup task ended without a result"))?
    }
    .boxed()
    .shared();
    let disposed = Arc::new(AtomicBool::new(false));
    let get_ready = ready.clone();
    let get_disposed = disposed.clone();
    let service = Arc::new(E2bService {
        cwd,
        runtime_root,
        get_sandbox: Arc::new(move || {
            let ready = get_ready.clone();
            let disposed = get_disposed.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    !disposed.load(Ordering::Acquire),
                    "E2B sandbox service is disposing"
                );
                let sandbox = ready
                    .await
                    .map_err(|message| anyhow::anyhow!(message.to_string()))?;
                anyhow::ensure!(
                    !disposed.load(Ordering::Acquire),
                    "E2B sandbox service is disposing"
                );
                Ok(sandbox)
            })
        }),
    });
    context.provide(E2B, service.clone())?;
    let teardown_ready = ready;
    context.own(EffectHandle::new("e2b sandbox teardown", move || {
        let disposed = disposed.clone();
        let ready = teardown_ready.clone();
        Box::pin(async move {
            disposed.store(true, Ordering::Release);
            let Ok(sandbox) = ready.await else {
                return Ok(());
            };
            if let Err(error) = sandbox.kill().await
                && error.downcast_ref::<E2bSandboxNotFound>().is_none()
            {
                tracing::error!(%error, "E2B sandbox teardown failed");
            }
            Ok(())
        })
    }))?;
    Ok(service)
}

async fn open(
    factory: Arc<dyn E2bSandboxFactory>,
    options: E2bCreateOptions,
    cwd: &str,
    runtime_root: &str,
) -> anyhow::Result<Arc<dyn E2bSandbox>> {
    let sandbox = factory.create(options).await?;
    let setup = async {
        sandbox.files().make_dir(cwd, None).await?;
        sandbox.files().make_dir(runtime_root, None).await?;
        let info = sandbox.files().get_info(runtime_root, None).await?;
        anyhow::ensure!(
            info.kind == E2bFileType::Directory && info.symlink_target.is_none(),
            "seekdeep-e2b: runtime root must be a real directory: {runtime_root}"
        );
        sandbox
            .commands()
            .run(
                &format!("chmod 700 -- {}", quote_e2b_shell_arg(runtime_root)),
                e2b_control_envs(&BTreeMap::new()),
                None,
            )
            .await?;
        Ok(())
    }
    .await;
    if let Err(error) = setup {
        let _ = sandbox.kill().await;
        return Err(error);
    }
    Ok(sandbox)
}

fn posix_join(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else {
        format!("{}/{child}", parent.trim_end_matches('/'))
    }
}

/// Builds the loader-compatible E2B owner plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: E2bConfig = serde_json::from_value(config)?;
            install(&context, config)?;
            Ok(())
        })
    })
}
