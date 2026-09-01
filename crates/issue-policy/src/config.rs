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
