//! The source's `vi.spyOn(ctx.subprocess, 'spawn')`: a subprocess service seat that records every
//! spawn request and handle while the real local runtime does the work.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    SubprocessHandleRef, SubprocessLookupEnvironment, SubprocessRuntime, SubprocessService,
    SubprocessSpawnSpec, SubprocessTerminalHandleRef, SubprocessTerminalSpawnSpec,
};
use seekdeep_subprocess_local::LocalSubprocessRuntime;

#[derive(Debug)]
pub(crate) struct SpyingSubprocessRuntime {
    inner: Arc<LocalSubprocessRuntime>,
    spawns: Mutex<Vec<SubprocessSpawnSpec>>,
    handles: Mutex<Vec<SubprocessHandleRef>>,
}

impl SpyingSubprocessRuntime {
    /// Installs the real local runtime under `owner`, which owns its teardown, and seats this spy
    /// as the subprocess service of `context`.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub(crate) fn install(owner: &Context, context: &Context) -> anyhow::Result<Arc<Self>> {
        let inner = LocalSubprocessRuntime::install(owner)?;
        let spy = Arc::new(Self {
            inner,
            spawns: Mutex::new(Vec::new()),
            handles: Mutex::new(Vec::new()),
        });
        let provider: Arc<dyn SubprocessRuntime> = spy.clone();
        SubprocessService::new(provider).provide(context)?;
        Ok(spy)
    }

    /// Every spawn request in order.
    pub(crate) fn spawns(&self) -> Vec<SubprocessSpawnSpec> {
        self.spawns.lock().clone()
    }

    /// Every spawned handle in order.
    pub(crate) fn handles(&self) -> Vec<SubprocessHandleRef> {
        self.handles.lock().clone()
    }

    /// The source's `expectQuiescent`: at least one child was spawned and every one of them exited
    /// with a closed outcome.
    pub(crate) async fn expect_quiescent(&self) {
        let handles = self.handles();
        assert!(!handles.is_empty(), "no child process was spawned");
        for handle in handles {
            assert!(handle.wait_for_exit(None).await.unwrap());
            let outcome = handle.done().await.unwrap();
            assert!(outcome.exit_code.is_some() || outcome.signal.is_some());
        }
    }
}

#[async_trait]
impl SubprocessRuntime for SpyingSubprocessRuntime {
    async fn resolve_executable(
        &self,
        command: &str,
        env: Option<&SubprocessLookupEnvironment>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<String> {
        self.inner.resolve_executable(command, env, signal).await
    }

    fn spawn(&self, spec: SubprocessSpawnSpec) -> anyhow::Result<SubprocessHandleRef> {
        self.spawns.lock().push(spec.clone());
        let handle = self.inner.spawn(spec)?;
        self.handles.lock().push(Arc::clone(&handle));
        Ok(handle)
    }

    async fn spawn_terminal(
        &self,
        spec: SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<SubprocessTerminalHandleRef> {
        self.inner.spawn_terminal(spec).await
    }
}
