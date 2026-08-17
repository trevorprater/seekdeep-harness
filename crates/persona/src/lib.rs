//! Scope-only persona composition for one agent.

use std::sync::Arc;

use seekdeep_cordis::{Context, Fiber, fiber::EffectHandle};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_system_prompt::{PromptSection, SystemPrompt};
use serde::{Deserialize, Serialize};

/// Re-export of the registry-owned persona ordering position.
pub use seekdeep_system_prompt::PERSONA_ORDER;
/// Re-export of the registry-owned, shadowable persona slot name.
pub use seekdeep_system_prompt::PERSONA_SECTION;

/// Per-agent persona composition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaConfig {
    /// Persona prose contributed to the deployment persona slot.
    pub text: String,
    /// Whether this section is the complete authoritative system prompt.
    #[serde(default)]
    pub complete: bool,
    /// Whether dynamic runtime-context snapshots remain visible in this scope.
    #[serde(default = "default_true")]
    pub include_runtime_context: bool,
}

impl PersonaConfig {
    /// Creates a persona with source-schema defaults.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            complete: false,
            include_runtime_context: true,
        }
    }

    /// Marks the persona as the complete system prompt.
    #[must_use]
    pub fn complete(mut self) -> Self {
        self.complete = true;
        self
    }

    /// Suppresses dynamic runtime context in the mounting scope.
    #[must_use]
    pub fn suppress_runtime_context(mut self) -> Self {
        self.include_runtime_context = false;
        self
    }
}

const fn default_true() -> bool {
    true
}

/// Installs a persona in the mounting context's scope.
///
/// The system-prompt service always owns the global persona slot, so mounting
/// this on an unscoped context fails loudly. A scoped registration shadows the
/// deployment persona only along that scope chain. Installation is
/// transactional: a partial registration is rolled back before an error is
/// returned.
///
/// # Errors
///
/// Returns for a duplicate persona slot, an inactive context, notification
/// failures, or cleanup failure while rolling back a partial installation.
pub async fn install(
    context: &Context,
    system_prompt: &Arc<SystemPrompt>,
    config: PersonaConfig,
) -> anyhow::Result<EffectHandle> {
    let fiber = Fiber::active_child("persona");
    let child = context.with_fiber(fiber.clone());
    let mut section = PromptSection::new(PERSONA_SECTION, PERSONA_ORDER, config.text);
    if config.complete {
        section = section.complete();
    }

    let install_result = (|| {
        system_prompt.section(&child, section)?;
        if !config.include_runtime_context {
            system_prompt.suppress_runtime_context(&child)?;
        }
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(error) = install_result {
        return match fiber.dispose().await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow::anyhow!("{error:#}: cleanup failed: {cleanup:#}")),
        };
    }

    let cleanup_fiber = fiber.clone();
    let effect = EffectHandle::new("persona", move || {
        Box::pin(async move { cleanup_fiber.dispose().await })
    });
    if let Err(error) = context.own(effect.clone()) {
        return match fiber.dispose().await {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!("{error}: cleanup failed: {cleanup:#}")),
        };
    }
    Ok(effect)
}

/// Registers the persona package's explained empty invariant companion.
///
/// Identity-slot ownership, complete-prompt enforcement, shadowing, and
/// disposal are all enforced by the system-prompt registry itself.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-persona", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_scope::{ScopeKey, create_scope};
    use seekdeep_system_prompt::{
        AssembleContext, AssembledSection, PromptContext, SystemPromptConfig, render_prompt,
    };

    use super::*;

    fn harness(deployment_persona: &str) -> (Context, Arc<SystemPrompt>) {
        let context = Context::new();
        let prompt = SystemPrompt::new(
            &context,
            SystemPromptConfig {
                persona: deployment_persona.to_owned(),
                ..SystemPromptConfig::default()
            },
        )
        .expect("system prompt");
        (context, prompt)
    }

    async fn persona_text(prompt: &SystemPrompt, scope: Option<ScopeKey>) -> Option<String> {
        prompt
            .assemble(AssembleContext {
                scope,
                ..AssembleContext::default()
            })
            .await
            .expect("assemble")
            .sections
            .into_iter()
            .find(|section| section.name == PERSONA_SECTION)
            .map(|section| section.text)
    }

    #[tokio::test]
    async fn unscoped_mount_collides_with_registry_default() {
        let (context, prompt) = harness("deployment identity");
        let error = install(
            &context,
            &prompt,
            PersonaConfig::new("composition identity"),
        )
        .await
        .expect_err("global collision");
        assert!(
            format!("{error:#}")
                .contains("prompt section \"deployment:persona\" is already registered")
        );
    }

    #[tokio::test]
    async fn scoped_personas_shadow_independently_and_empty_text_occupies_slot() {
        let (context, prompt) = harness("deployment identity");
        let first_key = ScopeKey::new();
        let second_key = ScopeKey::new();
        let empty_key = ScopeKey::new();
        let first = create_scope(&context, first_key, None).expect("first scope");
        let second = create_scope(&context, second_key, None).expect("second scope");
        let empty = create_scope(&context, empty_key, None).expect("empty scope");

        install(
            &first.context,
            &prompt,
            PersonaConfig::new("first identity"),
        )
        .await
        .expect("first persona");
        install(
            &second.context,
            &prompt,
            PersonaConfig::new("second identity"),
        )
        .await
        .expect("second persona");
        install(&empty.context, &prompt, PersonaConfig::new(""))
            .await
            .expect("empty persona");

        assert_eq!(
            persona_text(&prompt, Some(first_key)).await.as_deref(),
            Some("first identity")
        );
        assert_eq!(
            persona_text(&prompt, Some(second_key)).await.as_deref(),
            Some("second identity")
        );
        assert_eq!(
            persona_text(&prompt, Some(empty_key)).await.as_deref(),
            Some("")
        );
        assert_eq!(
            persona_text(&prompt, None).await.as_deref(),
            Some("deployment identity")
        );
    }

    #[tokio::test]
    async fn disposing_installation_restores_shadowed_default() {
        let (context, prompt) = harness("deployment identity");
        let key = ScopeKey::new();
        let scope = create_scope(&context, key, None).expect("scope");
        let installation = install(
            &scope.context,
            &prompt,
            PersonaConfig::new("preset identity"),
        )
        .await
        .expect("persona");
        assert_eq!(
            persona_text(&prompt, Some(key)).await.as_deref(),
            Some("preset identity")
        );

        installation.dispose().await.expect("dispose persona");
        assert_eq!(
            persona_text(&prompt, Some(key)).await.as_deref(),
            Some("deployment identity")
        );
    }

    #[tokio::test]
    async fn variables_interpolate_at_render_time() {
        let (context, prompt) = harness("");
        let key = ScopeKey::new();
        let scope = create_scope(&context, key, None).expect("scope");
        prompt
            .variable(
                &context,
                "model",
                Arc::new(|_| Ok(Some("seekdeep-v4-pro".to_owned()))),
            )
            .expect("variable");
        install(
            &scope.context,
            &prompt,
            PersonaConfig::new("You run on {{model}}."),
        )
        .await
        .expect("persona");

        let assembly = prompt
            .assemble(AssembleContext {
                scope: Some(key),
                ..AssembleContext::default()
            })
            .await
            .expect("assemble");
        assert_eq!(
            assembly
                .sections
                .iter()
                .find(|section| section.name == PERSONA_SECTION)
                .map(|section| section.text.as_str()),
            Some("You run on {{model}}.")
        );
        assert!(
            render_prompt(&assembly)
                .expect("render")
                .contains("You run on seekdeep-v4-pro.")
        );
    }

    #[tokio::test]
    async fn complete_persona_wins_after_late_waterfall_contributions() {
        let (context, prompt) = harness("deployment identity");
        let key = ScopeKey::new();
        let scope = create_scope(&context, key, None).expect("scope");
        prompt
            .section(
                &context,
                PromptSection::new("global:extra", 100.0, "global guidance"),
            )
            .expect("global section");
        install(
            &scope.context,
            &prompt,
            PersonaConfig::new("Only this.").complete(),
        )
        .await
        .expect("persona");
        prompt
            .on_assemble(
                &scope.context,
                |mut assembly, _, next| async move {
                    assembly.sections.push(AssembledSection {
                        name: "late:extra".to_owned(),
                        text: "late guidance".to_owned(),
                    });
                    next.run_with(assembly).await
                },
                seekdeep_cordis::EventOptions {
                    prepend: true,
                    ..seekdeep_cordis::EventOptions::default()
                },
            )
            .expect("middleware");

        let assembly = prompt
            .assemble(AssembleContext {
                scope: Some(key),
                ..AssembleContext::default()
            })
            .await
            .expect("assemble");
        assert_eq!(
            assembly.sections,
            [AssembledSection {
                name: PERSONA_SECTION.to_owned(),
                text: "Only this.".to_owned(),
            }]
        );
        assert_eq!(render_prompt(&assembly).expect("render"), "Only this.");
    }

    #[tokio::test]
    async fn runtime_context_suppression_is_scoped_and_reversible() {
        let (context, prompt) = harness("deployment identity");
        let key = ScopeKey::new();
        let scope = create_scope(&context, key, None).expect("scope");
        prompt
            .prompt_context(&context, PromptContext::new("policy", 1.0, "global policy"))
            .expect("context");
        let installation = install(
            &scope.context,
            &prompt,
            PersonaConfig::new("Only this.").suppress_runtime_context(),
        )
        .await
        .expect("persona");

        assert!(
            prompt
                .assemble(AssembleContext {
                    scope: Some(key),
                    ..AssembleContext::default()
                })
                .await
                .expect("scoped")
                .contexts
                .is_empty()
        );
        assert_eq!(
            prompt
                .assemble(AssembleContext::default())
                .await
                .expect("global")
                .contexts
                .len(),
            1
        );

        installation.dispose().await.expect("dispose persona");
        assert_eq!(
            prompt
                .assemble(AssembleContext {
                    scope: Some(key),
                    ..AssembleContext::default()
                })
                .await
                .expect("restored")
                .contexts
                .len(),
            1
        );
    }

    #[test]
    fn config_deserialization_applies_schema_defaults() {
        let config: PersonaConfig =
            serde_json::from_str(r#"{"text":"Scoped identity."}"#).expect("config");
        assert!(!config.complete);
        assert!(config.include_runtime_context);
        assert!(serde_json::from_str::<PersonaConfig>(r"{}").is_err());
    }

    #[tokio::test]
    async fn invariant_companion_reserves_and_releases_renamed_package() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &seekdeep_invariants::InvariantConfig::default())
                .expect("registry");
        let registration = register_invariant(&registry).expect("register invariant");
        registration.await_ready().await.expect("invariant ready");
        assert!(registry.is_registered("seekdeep-persona"));

        registration.dispose().await.expect("dispose invariant");
        assert!(!registry.is_registered("seekdeep-persona"));
    }
}
