//! Session-keyed skill catalog, single-flight, lexicon, and pick policy.

use std::collections::BTreeMap;

use seekdeep_identity::SessionId;
use serde::{Deserialize, Serialize};

/// One browser-safe skill catalog row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillCatalogEntry {
    /// Stable slash name.
    pub name: String,
    /// User-facing secondary copy.
    pub description: String,
    /// Optional invocation guidance retained from the Host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    /// Whether the model-side catalog also exposes this skill.
    #[serde(default)]
    pub model_invocable: bool,
}

/// One slash-menu candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillCandidate {
    /// Slash name.
    pub name: String,
    /// Description with an optional user-only marker.
    pub description: String,
}

/// Exact fetch generation owned by one Session cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SkillCatalogGeneration(u64);

impl SkillCatalogGeneration {
    /// Returns the opaque monotonic value for boundary maps.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Cache action selected for one catalog consumer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillCatalogDecision {
    /// Addressed child sessions cannot read the parent Agent catalog.
    Addressed,
    /// A settled catalog is immediately available.
    Settled(Vec<SkillCatalogEntry>),
    /// Join the exact in-flight generation.
    Join(SkillCatalogGeneration),
    /// Start the exact new generation.
    Start(SkillCatalogGeneration),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CatalogState {
    generation: SkillCatalogGeneration,
    settled: Option<Vec<SkillCatalogEntry>>,
}

/// Portable decision state for the browser catalog cache.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillCatalogCache {
    next_generation: u64,
    entries: BTreeMap<SessionId, CatalogState>,
}

impl SkillCatalogCache {
    /// Selects addressed, settled, join, or start behavior for one Session.
    ///
    /// # Panics
    ///
    /// Panics after exhausting every `u64` generation rather than reusing an ABA-prone token.
    pub fn begin(&mut self, session_id: &SessionId, addressed: bool) -> SkillCatalogDecision {
        if addressed {
            return SkillCatalogDecision::Addressed;
        }
        if let Some(entry) = self.entries.get(session_id) {
            return entry.settled.clone().map_or(
                SkillCatalogDecision::Join(entry.generation),
                SkillCatalogDecision::Settled,
            );
        }
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("skill catalog generation exhausted");
        let generation = SkillCatalogGeneration(self.next_generation);
        self.entries.insert(
            session_id.clone(),
            CatalogState {
                generation,
                settled: None,
            },
        );
        SkillCatalogDecision::Start(generation)
    }

    /// Commits one success only when its exact generation still owns the key.
    pub fn settle_success(
        &mut self,
        session_id: &SessionId,
        generation: SkillCatalogGeneration,
        skills: Vec<SkillCatalogEntry>,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(session_id) else {
            return false;
        };
        if entry.generation != generation {
            return false;
        }
        entry.settled = Some(skills);
        true
    }

    /// Evicts one failed fetch only when its exact generation still owns the key.
    pub fn settle_failure(
        &mut self,
        session_id: &SessionId,
        generation: SkillCatalogGeneration,
    ) -> bool {
        if self
            .entries
            .get(session_id)
            .is_some_and(|entry| entry.generation == generation)
        {
            self.entries.remove(session_id);
            true
        } else {
            false
        }
    }

    /// Invalidates one exact Session cache and returns its generation for abort ownership.
    pub fn invalidate(&mut self, session_id: &SessionId) -> Option<SkillCatalogGeneration> {
        self.entries
            .remove(session_id)
            .map(|entry| entry.generation)
    }

    /// Invalidates every cache and returns exact Session/generation abort ownership.
    pub fn clear(&mut self) -> Vec<(SessionId, SkillCatalogGeneration)> {
        std::mem::take(&mut self.entries)
            .into_iter()
            .map(|(session_id, entry)| (session_id, entry.generation))
            .collect()
    }

    /// Returns the synchronous settled skill names for one Session.
    #[must_use]
    pub fn lexicon(&self, session_id: &SessionId) -> Option<Vec<String>> {
        self.entries
            .get(session_id)?
            .settled
            .as_ref()
            .map(|skills| skills.iter().map(|skill| skill.name.clone()).collect())
    }

    /// Filters a settled catalog by the source's case-sensitive prefix rule.
    #[must_use]
    pub fn candidates(
        skills: &[SkillCatalogEntry],
        query: &str,
        user_only: &str,
    ) -> Vec<SkillCandidate> {
        skills
            .iter()
            .filter(|skill| skill.name.starts_with(query))
            .map(|skill| SkillCandidate {
                name: skill.name.clone(),
                description: if skill.model_invocable {
                    skill.description.clone()
                } else {
                    format!("{user_only} · {}", skill.description)
                },
            })
            .collect()
    }

    /// Returns the literal slash reference inserted into the draft.
    #[must_use]
    pub fn picked_text(name: &str) -> String {
        format!("/{name} ")
    }
}
