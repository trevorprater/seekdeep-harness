//! Bundled official `seekdeep-badge` skill provider.

use std::{path::PathBuf, sync::Arc};

use seekdeep_cordis::{Context, Plugin};
use seekdeep_skill::{
    BUNDLED_SKILL_RANK, SKILLS, SkillCandidate, SkillDefinition, SkillInvocationPolicy,
    SkillLookupOptions, SkillProvider, SkillProviderObservation, SkillResourceBase, SkillSource,
    SkillSummary,
};
use serde_json::json;

/// Cordis plugin name.
pub const NAME: &str = "skill-badge";
/// Skill registry service required by the provider.
pub const INJECT: &[&str] = &["skills"];
/// Stable provider and skill name after the product rename.
pub const PROVIDER_NAME: &str = "seekdeep-badge";
/// Model and user routing description.
pub const DESCRIPTION: &str = "Add the official “powered by seekdeep” badge to documents, pull requests, merge requests, and other content produced with SeekDeep Harness. Use whenever creating a pull request or merge request. Also use when the user asks for a seekdeep badge, powered-by-seekdeep attribution, or a reusable seekdeep badge asset or snippet.";

const SKILL_BODY: &str =
    include_str!("../../../packages/skill/skill-badge/assets/seekdeep-badge.md");

#[derive(Debug)]
struct BadgeProvider {
    resource_base: SkillResourceBase,
}

impl BadgeProvider {
    fn summary(&self) -> SkillSummary {
        SkillSummary {
            name: PROVIDER_NAME.to_owned(),
            description: DESCRIPTION.to_owned(),
            when_to_use: None,
            invocation: SkillInvocationPolicy {
                model_invocable: true,
                user_invocable: true,
            },
            source: SkillSource("bundled".to_owned()),
            provider: PROVIDER_NAME.to_owned(),
            resource_base: Some(self.resource_base.clone()),
        }
    }

    fn candidate(&self) -> SkillCandidate {
        let summary = self.summary();
        SkillCandidate {
            name: summary.name,
            description: summary.description,
            when_to_use: summary.when_to_use,
            invocation: summary.invocation,
            source: summary.source,
            provider: summary.provider,
            resource_base: summary.resource_base,
            rank: BUNDLED_SKILL_RANK,
            locator: json!({"kind": "bundled", "name": PROVIDER_NAME}),
            path: None,
            metadata: None,
        }
    }
}

#[async_trait::async_trait]
impl SkillProvider for BadgeProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    async fn list(
        &self,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<SkillProviderObservation> {
        Ok(SkillProviderObservation {
            candidates: vec![self.candidate()],
            complete: true,
        })
    }

    async fn get(
        &self,
        _candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        Ok(Some(SkillDefinition {
            summary: self.summary(),
            content: SKILL_BODY.to_owned(),
            path: None,
            metadata: None,
        }))
    }
}

/// Absolute resource directory advertised by the bundled provider.
#[must_use]
pub fn resource_directory() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/skill/skill-badge/assets");
    std::fs::canonicalize(&path)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Registers the bundled badge provider.
///
/// # Errors
///
/// Returns missing-service, duplicate-provider, or inactive-owner failures.
pub fn apply(context: &Context) -> anyhow::Result<()> {
    let skills = context
        .get(SKILLS)
        .ok_or_else(|| anyhow::anyhow!("skill-badge requires skills"))?;
    skills.register_provider(
        context,
        Arc::new(BadgeProvider {
            resource_base: SkillResourceBase::Directory {
                path: resource_directory(),
            },
        }),
    )?;
    Ok(())
}

/// Loader-facing Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move { apply(&context) })
    })
}

/// Embedded source PNG bytes used by artifact-integrity tests.
pub const BADGE_PNG: &[u8] =
    include_bytes!("../../../packages/skill/skill-badge/assets/seekdeep-badge.png");
