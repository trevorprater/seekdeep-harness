//! Service definition for the subagent capability seam.

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use parking_lot::Mutex;
use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::SessionId;
use seekdeep_llm::{AbortSignal, ContentBlock, MessageId};
use seekdeep_tools::assert_object_json_schema;

use crate::activation_setup_registry::{
    ContinuableSetupContribution, SubagentActivationSetupRegistry,
};
use crate::continuation::{
    ContinuableStart, ContinuableStartSpec, ContinuationHost, SubagentContinuationManager,
    SubagentFollowupOptions, SubagentInterruptAuthority, SubagentReportOptions,
};
use crate::depth::assert_subagent_max_depth;
use crate::descriptor::{SubagentDescriptorInput, snapshot_subagent_descriptor};
use crate::error::SubagentError;
use crate::lifecycle::{
    ActivationObserver, create_activation_observer, emit_subagent_lifecycle, observe_run,
};
use crate::list_children::{
    SubagentDescendantListEntry, SubagentListEntry, list_children as list_subagent_children,
    list_descendants as list_subagent_descendants,
};
use crate::types::{
    ContinuableCreateRequest, ContinuableCreateSpec, ResolvedSubagentStartRequest,
    SubagentProvider, SubagentRun, SubagentStartRequest,
};

/// Typed Cordis slot for the subagent runtime.
pub const SUBAGENTS: ServiceKey<SubagentRuntime> = ServiceKey::new("subagents");

/// Cordis plugin name.
pub const NAME: &str = "subagent";
/// Services required by the subagent runtime.
pub const INJECT: &[&str] = &[];

/// Named provider registry with one-shot runs, durable discovery, and
/// continuable-child operations.
pub struct SubagentRuntime {
    context: Context,
    providers: Mutex<HashMap<String, Arc<dyn SubagentProvider>>>,
    continuations: Mutex<Option<Arc<SubagentContinuationManager>>>,
    setup_registry: Arc<SubagentActivationSetupRegistry>,
}

impl SubagentRuntime {
    /// Constructs an unprovided runtime.
    #[must_use]
    pub fn new(context: &Context) -> Arc<Self> {
        Arc::new(Self {
            context: context.clone(),
            providers: Mutex::new(HashMap::new()),
            continuations: Mutex::new(None),
            setup_registry: SubagentActivationSetupRegistry::new(),
        })
    }

    /// Publishes this runtime on the subagents slot.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(SUBAGENTS, self.clone())
    }

    /// Builds and publishes the subagent runtime.
    ///
    /// # Errors
    ///
    /// Returns registration or provide failures.
    pub fn install(context: &Context) -> anyhow::Result<Arc<Self>> {
        let runtime = Self::new(context);
        runtime.provide(context)?;
        runtime.mount_continuations(context)?;
        Ok(runtime)
    }

    fn mount_continuations(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let weak = Arc::downgrade(self);
        let setup_registry = Arc::clone(&self.setup_registry);
        let plugin = Plugin::new(
            "subagent-continuations",
            ["agents"],
            move |child_ctx, _config| {
                let weak = weak.clone();
                let setup_registry = Arc::clone(&setup_registry);
                Box::pin(async move {
                    let host: Arc<dyn ContinuationHost> = Arc::new(HostBridge(weak.clone()));
                    let manager =
                        SubagentContinuationManager::new(&child_ctx, host, setup_registry)?;
                    if let Some(runtime) = weak.upgrade() {
                        *runtime.continuations.lock() = Some(Arc::clone(&manager));
                        let weak2 = weak.clone();
                        let manager2 = Arc::clone(&manager);
                        child_ctx.own(EffectHandle::synchronous(
                            "subagents.continuationBinding()",
                            move || {
                                if let Some(runtime) = weak2.upgrade() {
                                    let mut slot = runtime.continuations.lock();
                                    if slot.as_ref().is_some_and(|m| Arc::ptr_eq(m, &manager2)) {
                                        *slot = None;
                                    }
                                }
                                Ok(())
                            },
                        ))?;
                    }
                    Ok(())
                })
            },
        );
        context.plugin(plugin, serde_json::Value::Null)?;
        Ok(())
    }

    /// Registers a provider under its name.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-provider or inactive-owner failure.
    pub fn register_provider(
        self: &Arc<Self>,
        provider: Arc<dyn SubagentProvider>,
    ) -> anyhow::Result<EffectHandle> {
        let name = provider.name().to_owned();
        let event_args = seekdeep_cordis::EventArgs::one(provider.clone());
        let emission = self.context.events().prepare_emit(
            &self.context,
            "subagent/provider-added",
            &event_args,
        )?;
        {
            let mut providers = self.providers.lock();
            if providers.contains_key(&name) {
                return Err(SubagentError::new(
                    format!("a subagent provider named \"{name}\" is already registered"),
                    "DUPLICATE_PROVIDER",
                )
                .into());
            }
            providers.insert(name.clone(), provider);
        }
        if let Err(error) = emission.emit() {
            self.providers.lock().remove(&name);
            return Err(error);
        }
        let runtime = Arc::clone(self);
        let effect = EffectHandle::new("subagents.registerProvider()", move || {
            let runtime = Arc::clone(&runtime);
            let name = name.clone();
            Box::pin(async move {
                runtime.providers.lock().remove(&name);
                runtime.emit_provider_removed(&name);
                Ok(())
            })
        });
        Ok(effect)
    }

    /// Looks up a provider by name.
    #[must_use]
    pub fn get_provider(&self, name: &str) -> Option<Arc<dyn SubagentProvider>> {
        self.providers.lock().get(name).cloned()
    }

    /// Lists registered provider names in insertion order.
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        self.providers.lock().keys().cloned().collect()
    }

    /// Establishes a published child on the named provider.
    ///
    /// # Errors
    ///
    /// Returns no-provider, unsupported-capability, invalid-schema, or
    /// provider-start failures.
    pub async fn start(
        &self,
        name: &str,
        request: SubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        let provider = self.expect_provider(name)?;
        Self::assert_capabilities(&provider, &request)?;
        assert_subagent_max_depth(request.max_depth);
        if let Some(schema) = &request.output_schema {
            assert_object_json_schema(schema.clone())?;
        }
        let descriptor = snapshot_subagent_descriptor(&SubagentDescriptorInput::OneShot {
            provider: name.to_owned(),
            label: request.label.clone(),
        })?;
        let parent = Arc::clone(&request.parent);
        let resolved = ResolvedSubagentStartRequest {
            request,
            descriptor,
        };
        let run = provider.start(resolved).await?;
        Ok(observe_run(&self.context, name, &parent, run))
    }

    /// Establish one durable continuable child and deliver its initial prompt.
    ///
    /// # Errors
    ///
    /// Returns when continuation services are unavailable or materialization fails.
    pub async fn start_continuable(
        &self,
        spec: ContinuableStartSpec,
    ) -> anyhow::Result<ContinuableStart> {
        self.require_continuations()?.start_continuable(spec).await
    }

    /// Deliver one later message to a continuable child as its next FIFO turn.
    ///
    /// # Errors
    ///
    /// Returns when continuation services are unavailable, parent authority is
    /// rejected, or the message was not admitted.
    pub async fn followup(
        &self,
        parent: &Arc<Agent>,
        child_id: &SessionId,
        content: Vec<ContentBlock>,
        options: SubagentFollowupOptions,
    ) -> anyhow::Result<MessageId> {
        self.require_continuations()?
            .followup(parent, child_id, content, options)
            .await
    }

    /// Interrupt one live continuable child's current turn. An absent target is
    /// an accepted no-op.
    ///
    /// # Errors
    ///
    /// Returns the UNAUTHORIZED code when the authority does not own the live target.
    #[allow(clippy::needless_pass_by_value)]
    pub fn interrupt(
        &self,
        target_session_id: SessionId,
        authority: SubagentInterruptAuthority,
    ) -> anyhow::Result<()> {
        match self.continuations.lock().as_ref() {
            Some(manager) => manager.interrupt(&target_session_id, authority),
            None => Ok(()),
        }
    }

    /// Deliver selected content from one live continuable child to its durable
    /// direct parent.
    ///
    /// # Errors
    ///
    /// Returns when continuation services are unavailable, sender authorization
    /// fails, or the direct parent is not live.
    pub fn report_from(
        &self,
        child: &Arc<Agent>,
        content: Vec<ContentBlock>,
        options: SubagentReportOptions,
    ) -> anyhow::Result<MessageId> {
        self.require_continuations()?
            .report_from(child, content, options)
    }

    /// Compose one deployment capability into every continuable child's
    /// unpublished creation context.
    ///
    /// # Errors
    ///
    /// Returns effect-ownership failures.
    pub fn register_continuable_setup(
        &self,
        contribution: ContinuableSetupContribution,
    ) -> anyhow::Result<EffectHandle> {
        let effect = self.setup_registry.register(contribution);
        self.context.own(effect.clone())?;
        Ok(effect)
    }

    /// Close continuable admission below exact live parent Agents and dispose
    /// only their visible descendant Activations child-first.
    ///
    /// # Errors
    ///
    /// Returns an aggregate error after all branches settle when any failed.
    pub async fn drain_continuable_descendants(
        &self,
        parents: &[Arc<Agent>],
    ) -> anyhow::Result<()> {
        let manager = self.continuations.lock().clone();
        let Some(manager) = manager else {
            return Ok(());
        };
        manager.drain_descendants(parents).await
    }

    /// Enumerate the parent's direct session-backed subagents.
    ///
    /// # Errors
    ///
    /// Returns under the same conditions as the listing function.
    pub async fn list_children(
        &self,
        parent_session_id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SubagentListEntry>> {
        list_subagent_children(&self.context, parent_session_id, signal.as_ref()).await
    }

    /// Enumerate the root's complete session-backed subagent tree.
    ///
    /// # Errors
    ///
    /// Returns under the same conditions as the listing function.
    pub async fn list_descendants(
        &self,
        root_session_id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SubagentDescendantListEntry>> {
        list_subagent_descendants(&self.context, root_session_id, signal.as_ref()).await
    }

    fn expect_provider(&self, name: &str) -> anyhow::Result<Arc<dyn SubagentProvider>> {
        self.providers.lock().get(name).cloned().ok_or_else(|| {
            SubagentError::new(
                format!("no subagent provider registered for \"{name}\""),
                "NO_PROVIDER",
            )
            .into()
        })
    }

    fn require_continuations(&self) -> anyhow::Result<Arc<SubagentContinuationManager>> {
        self.continuations.lock().clone().ok_or_else(|| {
            SubagentError::new(
                "continuable subagents require the agents service",
                "CONTINUATION_UNAVAILABLE",
            )
            .into()
        })
    }

    async fn prepare_continuable(
        &self,
        name: &str,
        request: ContinuableCreateRequest,
    ) -> anyhow::Result<ContinuableCreateSpec> {
        let provider = self.expect_provider(name)?;
        provider.prepare_continuable(request).await
    }

    fn observe_activation(
        &self,
        provider: &str,
        child_id: &SessionId,
        parent: &Arc<Agent>,
    ) -> ActivationObserver {
        create_activation_observer(&self.context, provider, child_id, parent)
    }

    fn assert_capabilities(
        provider: &Arc<dyn SubagentProvider>,
        request: &SubagentStartRequest,
    ) -> anyhow::Result<()> {
        let capabilities = provider.capabilities();
        let needs: [(&str, bool, bool); 4] = [
            (
                "outputSchema",
                request.output_schema.is_some(),
                capabilities.output_schema,
            ),
            (
                "depthLimit",
                request.max_depth.is_some(),
                capabilities.depth_limit,
            ),
            (
                "toolFilter",
                request.tool_filter.is_some(),
                capabilities.tool_filter,
            ),
            ("persona", request.persona.is_some(), capabilities.persona),
        ];
        for (cap, needed, supported) in needs {
            if needed && !supported {
                return Err(SubagentError::new(
                    format!(
                        "subagent provider \"{}\" does not support the \"{cap}\" capability",
                        provider.name()
                    ),
                    "UNSUPPORTED_CAPABILITY",
                )
                .into());
            }
        }
        Ok(())
    }

    fn emit_provider_removed(&self, name: &str) {
        emit_subagent_lifecycle(
            &self.context,
            "subagent/provider-removed",
            seekdeep_cordis::EventArgs::one(name.to_owned()),
            None,
        );
    }
}

struct HostBridge(Weak<SubagentRuntime>);

impl ContinuationHost for HostBridge {
    fn prepare_continuable(
        &self,
        name: &str,
        request: ContinuableCreateRequest,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<ContinuableCreateSpec>> {
        let weak = self.0.clone();
        let name = name.to_owned();
        Box::pin(async move {
            let runtime = weak
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("subagent runtime disposed"))?;
            runtime.prepare_continuable(&name, request).await
        })
    }

    fn observe_activation(
        &self,
        provider: &str,
        child_id: &SessionId,
        parent: &Arc<Agent>,
    ) -> ActivationObserver {
        let runtime = self.0.upgrade().expect("runtime live during activation");
        runtime.observe_activation(provider, child_id, parent)
    }
}

/// Builds the loader-compatible subagent plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _config| {
        Box::pin(async move {
            SubagentRuntime::install(&context)?;
            Ok(())
        })
    })
}
