//! Scripted providers the pinned specs register beside the in-process ones.

#![allow(dead_code, reason = "shared by several spec ports")]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use seekdeep_subagent::{
    ContinuableCreateRequest, ContinuableCreateSpec, ResolvedSubagentStartRequest,
    SubagentCapabilities, SubagentProvider, SubagentRun,
};

/// A provider whose one-shot start must never run; with `continuable` it
/// contributes an empty continuable-creation spec, without it the runtime's
/// capability check rejects a continuable start first.
pub(crate) struct ScriptedProvider {
    name: String,
    capabilities: SubagentCapabilities,
    continuable: bool,
    starts: AtomicUsize,
}

impl ScriptedProvider {
    pub(crate) fn one_shot_only(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            capabilities: SubagentCapabilities::default(),
            continuable: false,
            starts: AtomicUsize::new(0),
        })
    }

    pub(crate) fn continuable(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            capabilities: SubagentCapabilities::default(),
            continuable: true,
            starts: AtomicUsize::new(0),
        })
    }

    pub(crate) fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SubagentProvider for ScriptedProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    fn inherits_parent_context(&self) -> bool {
        false
    }

    fn supports_continuable(&self) -> bool {
        self.continuable
    }

    async fn start(
        &self,
        _request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("one-shot start is not used")
    }

    async fn prepare_continuable(
        &self,
        _request: ContinuableCreateRequest,
    ) -> anyhow::Result<ContinuableCreateSpec> {
        anyhow::ensure!(self.continuable, "continuable preparation unsupported");
        Ok(ContinuableCreateSpec::default())
    }
}
