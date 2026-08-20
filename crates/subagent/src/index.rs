//! Service definition for the subagent capability seam.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_tools::assert_object_json_schema;

use crate::depth::assert_subagent_max_depth;
use crate::descriptor::{SubagentDescriptorInput, snapshot_subagent_descriptor};
use crate::error::SubagentError;
use crate::lifecycle::{emit_subagent_lifecycle, observe_run};
use crate::types::{
    ResolvedSubagentStartRequest, SubagentProvider, SubagentRun, SubagentStartRequest,
};

/// Typed Cordis slot for the subagent runtime.
pub const SUBAGENTS: ServiceKey<SubagentRuntime> = ServiceKey::new("subagents");

/// Cordis plugin name.
pub const NAME: &str = "subagent";
/// Services required by the subagent runtime.
pub const INJECT: &[&str] = &[];

/// Named provider registry with one-shot runs.
pub struct SubagentRuntime {
    context: Context,
    providers: Mutex<HashMap<String, Arc<dyn SubagentProvider>>>,
}

impl SubagentRuntime {
    /// Constructs an unprovided runtime.
    #[must_use]
    pub fn new(context: &Context) -> Arc<Self> {
        Arc::new(Self {
            context: context.clone(),
            providers: Mutex::new(HashMap::new()),
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
        Ok(runtime)
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
        {
            let mut providers = self.providers.lock();
            if providers.contains_key(&name) {
                return Err(SubagentError::new(
                    format!("a subagent provider named \"{name}\" is already registered"),
                    "DUPLICATE_PROVIDER",
                )
                .into());
            }
            providers.insert(name.clone(), provider.clone());
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
        emit_subagent_lifecycle(
            &self.context,
            "subagent/provider-added",
            seekdeep_cordis::EventArgs::one(provider),
            None,
        );
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

    fn expect_provider(&self, name: &str) -> anyhow::Result<Arc<dyn SubagentProvider>> {
        self.providers.lock().get(name).cloned().ok_or_else(|| {
            SubagentError::new(
                format!("no subagent provider registered for \"{name}\""),
                "NO_PROVIDER",
            )
            .into()
        })
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
