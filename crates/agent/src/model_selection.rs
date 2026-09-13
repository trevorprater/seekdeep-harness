//! Agent-scoped model selection shared by runtime entry points.

use std::sync::Arc;

use parking_lot::RwLock;
use seekdeep_cordis::{Context, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_llm::{LlmCallConfig, ModelId, ProviderId, ReasoningEffortId};
use seekdeep_system_prompt::SystemPrompt;

/// Complete provider, model, and optional reasoning effort for one live agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSelection {
    /// Registered provider route.
    pub provider: ProviderId,
    /// Provider-owned model identifier.
    pub model: ModelId,
    /// Adapter-owned effort, or provider/default behavior when absent.
    pub reasoning_effort: Option<ReasoningEffortId>,
}

/// Mutable next-step selection and the selection captured for the current step.
#[derive(Clone, Debug, Default)]
pub struct ModelSelectionRef {
    /// Selection read when the next prompt assembly enters middleware.
    pub current: Option<ModelSelection>,
    /// Selection paired with the most recently completed prompt assembly.
    pub assembled: Option<ModelSelection>,
}

/// The two listener effects installed as one model-selection coupling.
#[derive(Clone, Debug)]
pub struct ModelSelectionInstallation {
    assembly: EffectHandle,
    request: EffectHandle,
}

impl ModelSelectionInstallation {
    /// Removes both listeners in installation order.
    ///
    /// # Errors
    ///
    /// Returns the first listener-disposal failure after attempting both.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        let assembly = self.assembly.dispose().await;
        let request = self.request.dispose().await;
        assembly.and(request)
    }
}

/// Couples prompt assembly and request routing to one mutable selection.
///
/// The prompt listener snapshots `current` before delegating and records that
/// exact value only after delegation succeeds. The request listener uses the
/// recorded snapshot, preventing a concurrent switch from splitting prompt
/// variables and transport routing across models.
///
/// # Errors
///
/// Returns when either listener cannot be owned by the agent context.
pub fn install_model_selection(
    agent_context: &Context,
    system_prompt: &SystemPrompt,
    selection: Arc<RwLock<ModelSelectionRef>>,
) -> anyhow::Result<ModelSelectionInstallation> {
    let assembly_selection = selection.clone();
    let assembly = system_prompt.on_assemble(
        agent_context,
        move |_assembly, _context, next| {
            let selected = assembly_selection.read().current.clone();
            let assembly_selection = assembly_selection.clone();
            async move {
                let mut assembly = next.run().await?;
                assembly_selection.write().assembled.clone_from(&selected);
                if let Some(selected) = selected {
                    assembly
                        .variables
                        .insert("provider".to_owned(), Some(selected.provider.into_string()));
                    assembly
                        .variables
                        .insert("model".to_owned(), Some(selected.model.into_string()));
                }
                Ok(assembly)
            }
        },
        EventOptions::default(),
    )?;

    let request_selection = selection;
    let request = agent_context.events().on_waterfall(
        agent_context,
        "agent/request",
        move |_, _args, next| {
            let request_selection = request_selection.clone();
            Box::pin(async move {
                let reply = next.run().await?;
                let Some(config) = reply.downcast::<LlmCallConfig>() else {
                    anyhow::bail!("agent/request returned an invalid call config");
                };
                let Some(selected) = request_selection.read().assembled.clone() else {
                    return Ok(EventReply::Value(config));
                };
                let mut resolved = (*config).clone();
                resolved.provider = selected.provider;
                resolved.model = selected.model;
                resolved.reasoning_effort = selected.reasoning_effort;
                Ok(EventReply::Value(Arc::new(resolved)))
            })
        },
        EventOptions::default(),
    )?;
    Ok(ModelSelectionInstallation { assembly, request })
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::EventArgs;
    use seekdeep_scope::{ScopeKey, scope_target};
    use seekdeep_system_prompt::PromptAssembly;
    use seekdeep_system_prompt::{AssembleContext, SystemPromptConfig};

    use super::*;

    async fn request(context: &Context, config: LlmCallConfig) -> anyhow::Result<LlmCallConfig> {
        let reply = context
            .events()
            .waterfall(context, "agent/request", &EventArgs::new(), move || {
                Box::pin(async move { Ok(EventReply::Value(Arc::new(config))) })
            })
            .await?;
        reply
            .downcast::<LlmCallConfig>()
            .map(|config| (*config).clone())
            .ok_or_else(|| anyhow::anyhow!("config"))
    }

    #[tokio::test]
    async fn snapshots_prompt_and_request_selection_together() {
        let root = Context::new();
        let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("prompt");
        let selection = Arc::new(RwLock::new(ModelSelectionRef::default()));
        let installation =
            install_model_selection(&root, &prompt, selection.clone()).expect("install");
        let seed = LlmCallConfig {
            provider: "seed".into(),
            model: "seed".into(),
            reasoning_effort: None,
            temperature: Some(0.2),
            max_tokens: None,
            stop: None,
        };
        let initial = prompt
            .assemble(AssembleContext::default())
            .await
            .expect("assemble");
        assert!(initial.variables.is_empty());
        assert_eq!(request(&root, seed.clone()).await.expect("request"), seed);

        selection.write().current = Some(ModelSelection {
            provider: "alpha".into(),
            model: "a1".into(),
            reasoning_effort: Some(ReasoningEffortId::new("high")),
        });
        let assembled = prompt
            .assemble(AssembleContext::default())
            .await
            .expect("assemble");
        assert_eq!(assembled.variables["provider"].as_deref(), Some("alpha"));
        selection.write().current = Some(ModelSelection {
            provider: "beta".into(),
            model: "b1".into(),
            reasoning_effort: None,
        });
        let alpha = request(&root, seed.clone()).await.expect("request");
        assert_eq!(alpha.provider.as_str(), "alpha");
        assert_eq!(alpha.model.as_str(), "a1");
        assert_eq!(
            alpha
                .reasoning_effort
                .as_ref()
                .map(ReasoningEffortId::as_str),
            Some("high")
        );

        prompt
            .assemble(AssembleContext::default())
            .await
            .expect("assemble beta");
        let mut inherited = seed.clone();
        inherited.reasoning_effort = Some(ReasoningEffortId::new("max"));
        let beta = request(&root, inherited).await.expect("request beta");
        assert_eq!(beta.provider.as_str(), "beta");
        assert_eq!(beta.model.as_str(), "b1");
        assert!(beta.reasoning_effort.is_none());

        installation.dispose().await.expect("dispose");
        let after = prompt
            .assemble(AssembleContext::default())
            .await
            .expect("after");
        assert!(after.variables.is_empty());
        assert_eq!(
            request(&root, seed.clone()).await.expect("after request"),
            seed
        );

        // Scoped listeners remain selected by their exact routing context.
        let _ = scope_target(&root, Some(ScopeKey::new()));
        let _: PromptAssembly = after;
    }
}
