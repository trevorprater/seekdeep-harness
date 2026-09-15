//! Issue-management repository and Project configuration.

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

/// Repository and GitHub Project identities owned by Issue policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuePolicyConfig {
    /// GitHub organization login.
    pub organization: String,
    /// GitHub repository name.
    pub repository: String,
    /// Organization `ProjectV2` number.
    pub project_number: u64,
    /// Required `ProjectV2` title.
    pub project_title: String,
    /// Automation actor allowed to regress `In review` after requested changes.
    pub lifecycle_actor: String,
    /// Issue field carrying P0-P3.
    pub priority_field: String,
    /// Whether an Owner line may precede assignment permission.
    #[serde(default)]
    pub allow_unassigned_owner: bool,
    /// Ordered Project status progression.
    pub statuses: Vec<String>,
}

impl IssuePolicyConfig {
    /// Load the target repository's compiled configuration.
    ///
    /// # Errors
    ///
    /// Returns malformed JSON or a missing lifecycle/status invariant.
    pub fn bundled() -> Result<Self> {
        Self::from_json(include_str!(
            "../../../.github/issue-management/config.json"
        ))
    }

    /// Addresses the repository an Actions run is hosted in.
    ///
    /// The bundled identity names the published home of the policy; a workflow run in a
    /// fork or another clone still validates and updates its own pull requests and Issues,
    /// so `GITHUB_REPOSITORY` (`owner/name`, set by Actions) overrides the organization and
    /// repository while the Project identities stay bundled.
    ///
    /// # Errors
    ///
    /// Returns a slug that is not `owner/name`.
    pub fn for_actions_repository(mut self, slug: Option<&str>) -> Result<Self> {
        let Some(slug) = slug.map(str::trim).filter(|slug| !slug.is_empty()) else {
            return Ok(self);
        };
        let (organization, repository) = slug
            .split_once('/')
            .filter(|(owner, name)| !owner.is_empty() && !name.is_empty() && !name.contains('/'))
            .ok_or_else(|| anyhow::anyhow!("GITHUB_REPOSITORY must be owner/name, got {slug:?}"))?;
        organization.clone_into(&mut self.organization);
        repository.clone_into(&mut self.repository);
        Ok(self)
    }

    /// Parse and validate one configuration document.
    ///
    /// # Errors
    ///
    /// Returns malformed JSON or a missing lifecycle/status invariant.
    pub fn from_json(source: &str) -> Result<Self> {
        let config: Self = serde_json::from_str(source).context("invalid Issue policy config")?;
        config.validate()?;
        Ok(config)
    }

    /// Active statuses in their permitted forward-only order.
    #[must_use]
    pub fn active_statuses(&self) -> Vec<&str> {
        self.statuses
            .iter()
            .map(String::as_str)
            .filter(|status| !matches!(*status, "Done" | "No action"))
            .collect()
    }

    fn validate(&self) -> Result<()> {
        let active = self.active_statuses();
        for status in ["In progress", "In review"] {
            ensure!(active.contains(&status), "config.statuses 缺少 {status}");
        }
        ensure!(
            !self.lifecycle_actor.is_empty(),
            "config.lifecycleActor 未设置"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::IssuePolicyConfig;

    #[test]
    fn an_actions_repository_slug_overrides_the_bundled_identity_only() {
        let bundled = IssuePolicyConfig::bundled().unwrap();
        let hosted = bundled
            .clone()
            .for_actions_repository(Some("someone/seekdeep-harness-fork"))
            .unwrap();
        assert_eq!(hosted.organization, "someone");
        assert_eq!(hosted.repository, "seekdeep-harness-fork");
        assert_eq!(hosted.project_number, bundled.project_number);
        assert_eq!(hosted.project_title, bundled.project_title);
        assert_eq!(
            bundled.clone().for_actions_repository(None).unwrap(),
            bundled
        );
        assert_eq!(
            bundled.clone().for_actions_repository(Some(" ")).unwrap(),
            bundled
        );
        for slug in ["nameonly", "/name", "owner/", "owner/name/extra"] {
            assert!(bundled.clone().for_actions_repository(Some(slug)).is_err());
        }
    }
}
