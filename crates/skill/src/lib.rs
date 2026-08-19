//! Agent skill registry foundation: the shared skill types and the
//! model-visible rendering plus name/invocation validation.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Standard precedence rank for packaged skill providers and local bundled roots.
pub const BUNDLED_SKILL_RANK: f64 = 600.0;

/// Returns whether a string is a valid kebab-case skill name.
#[must_use]
pub fn is_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Origin bucket for a skill contribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SkillSource(pub String);

impl std::fmt::Display for SkillSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Optional provider-specific base used by loaded skill bodies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SkillResourceBase {
    /// Local directory base.
    Directory {
        /// Absolute or workspace-relative path.
        path: String,
    },
    /// Remote URL base.
    Url {
        /// Base URL.
        url: String,
    },
    /// Opaque resource description.
    Opaque {
        /// Human-readable description.
        description: String,
    },
}

/// Invocation controls shared by skill discovery consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInvocationPolicy {
    /// Whether model-facing catalogs and loaders include this skill.
    pub model_invocable: bool,
    /// Whether human-facing command catalogs and loaders include this skill.
    pub user_invocable: bool,
}

/// Invocation-neutral skill metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    /// Kebab-case identifier.
    pub name: String,
    /// Short routing description.
    pub description: String,
    /// Optional fuller when-to-use guidance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    /// Invocation controls.
    pub invocation: SkillInvocationPolicy,
    /// Origin bucket.
    pub source: SkillSource,
    /// Provider label.
    pub provider: String,
    /// Optional resource base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_base: Option<SkillResourceBase>,
}

/// Complete parsed skill definition including the loaded body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDefinition {
    /// Shared summary fields.
    #[serde(flatten)]
    pub summary: SkillSummary,
    /// Markdown instruction body.
    pub content: String,
    /// Absolute file path when the skill came from disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Parsed optional frontmatter metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Whether a skill may be advertised to and loaded by a model.
#[must_use]
pub fn is_model_invocable(skill: &SkillSummary) -> bool {
    skill.invocation.model_invocable
}

/// Whether a skill may be advertised to and loaded by a human-facing command.
#[must_use]
pub fn is_user_invocable(skill: &SkillSummary) -> bool {
    skill.invocation.user_invocable
}

/// Renders one loaded skill for the model as a canonical `skill_content` block.
#[must_use]
pub fn render_skill_content(skill: &SkillDefinition) -> String {
    let summary = &skill.summary;
    let resource_hint = render_resource_hint(summary);
    let mut lines = vec![format!(
        "<skill_content name=\"{}\">",
        escape_attr(&summary.name)
    )];
    lines.push("<skill_resources>".to_owned());
    lines.extend(resource_hint);
    lines.push("</skill_resources>".to_owned());
    lines.push(String::new());
    lines.push("<skill_instructions>".to_owned());
    lines.push(skill.content.clone());
    lines.push("</skill_instructions>".to_owned());
    lines.push("</skill_content>".to_owned());
    lines.join(
        "
",
    )
}

fn render_resource_hint(summary: &SkillSummary) -> Vec<String> {
    match &summary.resource_base {
        None => vec![
            format!(
                "Resources for this skill are managed by provider \"{}\".",
                escape_text(&summary.provider),
            ),
            "Load referenced resources only as needed.".to_owned(),
        ],
        Some(SkillResourceBase::Directory { path }) => vec![
            format!("Base directory for this skill: {}", escape_text(path)),
            "Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.".to_owned(),
        ],
        Some(SkillResourceBase::Url { url }) => vec![
            format!("Base URL for this skill: {}", escape_text(url)),
            "Resolve relative URLs mentioned by this skill against the base URL before using them. Load referenced resources only as needed.".to_owned(),
        ],
        Some(SkillResourceBase::Opaque { description }) => vec![
            format!("Resources for this skill: {}", escape_text(description)),
            "Load referenced resources only as needed.".to_owned(),
        ],
    }
}

/// Escapes model-facing attribute text so it cannot open framing tags.
#[must_use]
pub fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// Escapes model-facing prose embedded inside skill markup.
#[must_use]
pub fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One catalog candidate plus its precedence order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCandidate {
    /// Kebab-case identifier.
    pub name: String,
    /// Short routing description.
    pub description: String,
    /// Optional fuller when-to-use guidance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    /// Invocation controls.
    pub invocation: SkillInvocationPolicy,
    /// Origin bucket.
    pub source: SkillSource,
    /// Provider label.
    pub provider: String,
    /// Optional resource base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_base: Option<SkillResourceBase>,
    /// Precedence rank within one layer.
    pub rank: f64,
    /// Provider-specific opaque discovery locator.
    pub locator: Value,
    /// Absolute file path when the skill came from disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Parsed optional frontmatter metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Runtime skill contribution accepted by the registry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRegistration {
    /// Shared summary fields.
    #[serde(flatten)]
    pub summary: SkillSummary,
    /// Markdown instruction body.
    pub content: String,
    /// Absolute file path when the skill came from disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Parsed optional frontmatter metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Caller context for cwd-sensitive and abortable provider work.
#[derive(Clone, Debug, Default)]
pub struct SkillLookupOptions {
    /// Workspace selector.
    pub cwd: Option<String>,
    /// Cancellation for discovery or loading.
    pub signal: Option<seekdeep_llm::AbortSignal>,
}

/// Registry read options including the viewing scope.
#[derive(Clone, Debug, Default)]
pub struct SkillViewOptions {
    /// Lookup context.
    pub lookup: SkillLookupOptions,
    /// Viewing scope, or None for the global layer alone.
    pub scope: Option<seekdeep_scope::ScopeKey>,
}

/// Provider candidates plus discovery completeness.
#[derive(Clone, Debug, Default)]
pub struct SkillProviderObservation {
    /// Available candidates.
    pub candidates: Vec<SkillCandidate>,
    /// Whether discovery completed and these candidates may be cached.
    pub complete: bool,
}

/// One catalog observation plus discovery completeness.
#[derive(Clone, Debug)]
pub struct SkillCatalogSnapshot {
    /// Sorted invocation-neutral summaries.
    pub skills: Vec<SkillSummary>,
    /// Whether every provider completed without a concurrent revision.
    pub complete: bool,
}

/// Provider interface for one source of skills.
#[async_trait::async_trait]
pub trait SkillProvider: Send + Sync + 'static {
    /// Unique provider name.
    fn name(&self) -> &str;

    /// Lists available candidates for the current lookup context.
    async fn list(&self, options: &SkillLookupOptions) -> anyhow::Result<SkillProviderObservation>;

    /// Loads a complete skill body for a previously listed candidate.
    async fn get(
        &self,
        candidate: &SkillCandidate,
        options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>>;
}

/// The reserved provider name for runtime skill registrations.
pub const RUNTIME_PROVIDER: &str = "runtime";
/// Default maximum completed catalogs kept in memory.
pub const DEFAULT_COLLECT_CACHE_ENTRIES: usize = 128;

/// One provider registration retained by its layer.
#[derive(Clone)]
struct ProviderEntry {
    provider: Arc<dyn SkillProvider>,
    /// Service-wide monotonic registration order, the within-layer rank tiebreak.
    order: usize,
}

struct SkillLayer {
    providers: seekdeep_scope::store::NamedEntries<ProviderEntry>,
    runtime: seekdeep_scope::store::NamedEntries<SkillDefinition>,
}

impl SkillLayer {
    fn new(scope: Option<seekdeep_scope::ScopeKey>) -> Self {
        let scoped = scope.is_some();
        Self {
            providers: seekdeep_scope::store::NamedEntries::new(move |name| {
                if scoped {
                    anyhow::anyhow!(
                        "a skill provider named {name:?} is already registered in this scope"
                    )
                } else {
                    anyhow::anyhow!("a skill provider named {name:?} is already registered")
                }
            }),
            runtime: seekdeep_scope::store::NamedEntries::new(|name| {
                anyhow::anyhow!("a runtime skill named {name:?} is already registered")
            }),
        }
    }
}

impl seekdeep_scope::store::ScopeLayer for SkillLayer {
    fn is_empty(&self) -> bool {
        self.providers.is_empty() && self.runtime.is_empty()
    }
}

/// Runtime-skill provider whose `get()` returns the injected definition.
struct RuntimeSkillProvider;

#[async_trait::async_trait]
impl SkillProvider for RuntimeSkillProvider {
    fn name(&self) -> &str {
        RUNTIME_PROVIDER
    }

    async fn list(
        &self,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<SkillProviderObservation> {
        Ok(SkillProviderObservation::default())
    }

    async fn get(
        &self,
        candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        Ok(serde_json::from_value::<SkillDefinition>(candidate.locator.clone()).ok())
    }
}

/// Agent skill provider registry.
pub struct SkillRegistry {
    layers: seekdeep_scope::store::ScopedLayers<SkillLayer>,
    collect_cache: parking_lot::Mutex<std::collections::HashMap<String, Arc<SkillCatalogSnapshot>>>,
    revision: Arc<parking_lot::Mutex<usize>>,
    next_provider_order: parking_lot::Mutex<usize>,
    next_scope_id: parking_lot::Mutex<usize>,
    scope_ids: parking_lot::Mutex<std::collections::HashMap<seekdeep_scope::ScopeKey, usize>>,
    collect_cache_max_entries: usize,
}

impl std::fmt::Debug for SkillRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SkillRegistry")
            .field("collect_cache_max_entries", &self.collect_cache_max_entries)
            .finish_non_exhaustive()
    }
}

impl SkillRegistry {
    /// Constructs the registry with the configured cache capacity.
    #[must_use]
    pub fn new(collect_cache_max_entries: usize) -> Arc<Self> {
        let revision = Arc::new(parking_lot::Mutex::new(0usize));
        let layers = seekdeep_scope::store::ScopedLayers::new(SkillLayer::new, {
            let revision = revision.clone();
            move || {
                *revision.lock() += 1;
            }
        });
        Arc::new(Self {
            layers,
            collect_cache: parking_lot::Mutex::new(std::collections::HashMap::new()),
            revision,
            next_provider_order: parking_lot::Mutex::new(0),
            next_scope_id: parking_lot::Mutex::new(1),
            scope_ids: parking_lot::Mutex::new(std::collections::HashMap::new()),
            collect_cache_max_entries,
        })
    }

    /// Registers a borrowed same-process provider into the calling scope's layer.
    ///
    /// # Errors
    ///
    /// Returns duplicate-name or inactive-context failures.
    pub fn register_provider(
        self: &Arc<Self>,
        context: &Context,
        provider: Arc<dyn SkillProvider>,
    ) -> anyhow::Result<seekdeep_cordis::fiber::EffectHandle> {
        let name = provider.name().to_owned();
        anyhow::ensure!(
            name != RUNTIME_PROVIDER,
            "{RUNTIME_PROVIDER:?} is reserved for runtime skill registrations"
        );
        let order = {
            let mut next = self.next_provider_order.lock();
            let order = *next;
            *next += 1;
            order
        };
        self.layers.effect(
            context,
            move |layer| {
                let undo = layer
                    .providers
                    .insert(name, ProviderEntry { provider, order })?;
                Ok(undo)
            },
            seekdeep_scope::store::LayerEffectOptions::new("skills.registerProvider()"),
        )
    }

    /// Registers a borrowed runtime skill into the calling scope's layer.
    ///
    /// # Errors
    ///
    /// Returns invalid-skill or inactive-context failures.
    pub fn register(
        self: &Arc<Self>,
        context: &Context,
        skill: SkillDefinition,
    ) -> anyhow::Result<seekdeep_cordis::fiber::EffectHandle> {
        anyhow::ensure!(is_skill_name(&skill.summary.name), "invalid skill name");
        let name = skill.summary.name.clone();
        let scope = seekdeep_scope::scope_of(context);
        let already_registered = match scope {
            Some(scope) => self
                .layers
                .peek(Some(scope))
                .is_some_and(|layer| layer.runtime.contains_key(&name)),
            None => self.layers.global.runtime.contains_key(&name),
        };
        if already_registered {
            tracing::warn!("runtime skill {name:?} ignored because it is already registered");
            return Ok(seekdeep_cordis::fiber::EffectHandle::synchronous(
                "skills.register() (no-op)",
                || Ok(()),
            ));
        }
        self.layers.effect(
            context,
            move |layer| layer.runtime.insert(name, skill),
            seekdeep_scope::store::LayerEffectOptions::new("skills.register()"),
        )
    }

    /// Lists sorted invocation-neutral summaries.
    ///
    /// # Errors
    ///
    /// Returns provider discovery failures.
    pub async fn list(&self, options: &SkillViewOptions) -> anyhow::Result<Vec<SkillSummary>> {
        Ok(self.snapshot(options).await?.skills)
    }

    /// Observes the current catalog and its discovery completeness.
    ///
    /// # Errors
    ///
    /// Returns provider discovery failures.
    pub async fn snapshot(
        &self,
        options: &SkillViewOptions,
    ) -> anyhow::Result<SkillCatalogSnapshot> {
        self.collect(options).await
    }

    /// Loads and validates the winning candidate for one name.
    ///
    /// # Errors
    ///
    /// Returns provider discovery or loading failures.
    pub async fn get(
        &self,
        name: &str,
        options: &SkillViewOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        if !is_skill_name(name) {
            return Ok(None);
        }
        let snapshot = self.collect(options).await?;
        let entry = snapshot
            .skills
            .iter()
            .find(|summary| summary.name == name)
            .cloned();
        // Reload through the winning provider.
        if entry.is_none() {
            return Ok(None);
        }
        // The snapshot lost the provider; re-collect the candidate directly.
        let candidate = self.collect_candidate(name, options).await?;
        let Some((provider, candidate)) = candidate else {
            return Ok(None);
        };
        let definition = provider.get(&candidate, &options.lookup).await?;
        let Some(definition) = definition else {
            return Ok(None);
        };
        anyhow::ensure!(
            definition.summary.name == candidate.name,
            "loaded skill name mismatch"
        );
        Ok(Some(definition))
    }

    async fn collect_candidate(
        &self,
        name: &str,
        options: &SkillViewOptions,
    ) -> anyhow::Result<Option<(Arc<dyn SkillProvider>, SkillCandidate)>> {
        let layers = self.layer_chain(options.scope);
        for layer in &layers {
            let (candidates, _cacheable) = self.collect_layer(layer, options).await?;
            for candidate in candidates {
                if candidate.name == name {
                    let provider = Self::provider_for(layer, &candidate);
                    return Ok(Some((provider, candidate)));
                }
            }
        }
        Ok(None)
    }

    fn provider_for(layer: &SkillLayer, candidate: &SkillCandidate) -> Arc<dyn SkillProvider> {
        if candidate.provider == RUNTIME_PROVIDER {
            return Arc::new(RuntimeSkillProvider);
        }
        layer
            .providers
            .entries()
            .find_map(|(_, entry)| {
                if entry.provider.name() == candidate.provider {
                    Some(entry.provider.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| Arc::new(RuntimeSkillProvider))
    }

    fn layer_chain(&self, scope: Option<seekdeep_scope::ScopeKey>) -> Vec<Arc<SkillLayer>> {
        let mut layers = vec![self.layers.global.clone()];
        layers.extend(self.layers.chain_layers(scope));
        layers
    }

    async fn collect(&self, options: &SkillViewOptions) -> anyhow::Result<SkillCatalogSnapshot> {
        let revision = *self.revision.lock();
        let key = self.collect_cache_key(options.lookup.cwd.as_deref(), options.scope, revision);
        if let Some(cached) = self.collect_cache.lock().get(&key).cloned() {
            return Ok((*cached).clone());
        }
        let snapshot = self.collect_fresh(options).await?;
        if *self.revision.lock() == revision && snapshot.complete {
            self.collect_cache
                .lock()
                .insert(key, Arc::new(snapshot.clone()));
        }
        Ok(snapshot)
    }

    async fn collect_fresh(
        &self,
        options: &SkillViewOptions,
    ) -> anyhow::Result<SkillCatalogSnapshot> {
        let layers = self.layer_chain(options.scope);
        let mut merged: std::collections::HashMap<String, SkillCandidate> =
            std::collections::HashMap::new();
        let mut complete = true;
        for layer in &layers {
            let (collected, cacheable) = self.collect_layer(layer, options).await?;
            if !cacheable {
                complete = false;
            }
            for candidate in collected {
                merged.insert(candidate.name.clone(), candidate);
            }
        }
        let mut skills: Vec<SkillSummary> = merged.values().map(to_summary).collect();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(SkillCatalogSnapshot { skills, complete })
    }

    async fn collect_layer(
        &self,
        layer: &SkillLayer,
        options: &SkillViewOptions,
    ) -> anyhow::Result<(Vec<SkillCandidate>, bool)> {
        // Indexed candidates: (candidate, provider_order, local_order). The rank
        // lives on the candidate; provider order breaks rank ties across
        // providers, and local order breaks ties within one provider.
        let mut indexed: Vec<(SkillCandidate, i64, usize)> = Vec::new();
        let mut cacheable = true;
        let mut runtime: Vec<SkillDefinition> = layer.runtime.values().collect();
        runtime.sort_by(|a, b| a.summary.name.cmp(&b.summary.name));
        for (local_order, skill) in runtime.into_iter().enumerate() {
            indexed.push((runtime_candidate(&skill), -1, local_order));
        }
        for (_, entry) in layer.providers.entries() {
            let provider_order = i64::try_from(entry.order).unwrap_or(i64::MAX);
            let observation = match entry.provider.list(&options.lookup).await {
                Ok(observation) => observation,
                Err(error) => {
                    cacheable = false;
                    tracing::warn!(
                        "skill provider {:?} skipped: {error:#}",
                        entry.provider.name()
                    );
                    continue;
                }
            };
            if !observation.complete {
                cacheable = false;
            }
            for (local_order, candidate) in observation.candidates.into_iter().enumerate() {
                indexed.push((candidate, provider_order, local_order));
            }
        }
        // Lower ranks win within one layer; runtime entries (rank 250) lose to
        // project and user ranks, and equal ranks keep runtime-first insertion order.
        indexed.sort_by(|a, b| {
            a.0.rank
                .partial_cmp(&b.0.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
        });
        // Dedupe by name within one layer: first (highest-priority) wins.
        let mut seen = std::collections::HashSet::new();
        let candidates = indexed
            .into_iter()
            .filter_map(|(candidate, _, _)| {
                if seen.insert(candidate.name.clone()) {
                    Some(candidate)
                } else {
                    None
                }
            })
            .collect();
        Ok((candidates, cacheable))
    }

    fn collect_cache_key(
        &self,
        cwd: Option<&str>,
        scope: Option<seekdeep_scope::ScopeKey>,
        revision: usize,
    ) -> String {
        let scope_ids: Vec<usize> = seekdeep_scope::scope_chain_of(scope)
            .into_iter()
            .map(|key| self.scope_id(key))
            .collect();
        format!("{cwd:?}|{scope_ids:?}|{revision}")
    }

    fn scope_id(&self, key: seekdeep_scope::ScopeKey) -> usize {
        let mut scope_ids = self.scope_ids.lock();
        if let Some(id) = scope_ids.get(&key) {
            return *id;
        }
        let id = {
            let mut next = self.next_scope_id.lock();
            let id = *next;
            *next += 1;
            id
        };
        scope_ids.insert(key, id);
        id
    }
}

fn runtime_candidate(skill: &SkillDefinition) -> SkillCandidate {
    SkillCandidate {
        name: skill.summary.name.clone(),
        description: skill.summary.description.clone(),
        when_to_use: skill.summary.when_to_use.clone(),
        invocation: skill.summary.invocation,
        source: skill.summary.source.clone(),
        provider: skill.summary.provider.clone(),
        resource_base: skill.summary.resource_base.clone(),
        rank: 250.0,
        locator: serde_json::to_value(skill).unwrap_or(Value::Null),
        path: skill.path.clone(),
        metadata: skill.metadata.clone(),
    }
}

fn to_summary(candidate: &SkillCandidate) -> SkillSummary {
    SkillSummary {
        name: candidate.name.clone(),
        description: candidate.description.clone(),
        when_to_use: candidate.when_to_use.clone(),
        invocation: candidate.invocation,
        source: candidate.source.clone(),
        provider: candidate.provider.clone(),
        resource_base: candidate.resource_base.clone(),
    }
}

/// Registers the package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-skill", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::Context;
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    fn skill() -> SkillDefinition {
        SkillDefinition {
            summary: SkillSummary {
                name: "dsh-badge".to_owned(),
                description: "Add a badge".to_owned(),
                when_to_use: None,
                invocation: SkillInvocationPolicy {
                    model_invocable: true,
                    user_invocable: true,
                },
                source: SkillSource("bundled".to_owned()),
                provider: "dsh-badge".to_owned(),
                resource_base: Some(SkillResourceBase::Directory {
                    path: "/skills/badge".to_owned(),
                }),
            },
            content: "render a badge".to_owned(),
            path: None,
            metadata: None,
        }
    }

    #[test]
    fn skill_name_grammar_accepts_kebab_case_only() {
        assert!(is_skill_name("dsh-badge"));
        assert!(is_skill_name("skill-filesystem"));
        assert!(is_skill_name("a1-b2"));
        assert!(!is_skill_name(""));
        assert!(!is_skill_name("Badge"));
        assert!(!is_skill_name("-badge"));
        assert!(!is_skill_name("badge-"));
        assert!(!is_skill_name("badge--x"));
    }

    #[test]
    fn invocation_policy_is_resolved_independently() {
        let skill = skill();
        assert!(is_model_invocable(&skill.summary));
        assert!(is_user_invocable(&skill.summary));
        let mut summary = skill.summary.clone();
        summary.invocation.user_invocable = false;
        assert!(!is_user_invocable(&summary));
    }

    #[test]
    fn render_skill_content_embeds_escaped_attributes_and_verbatim_body() {
        let rendered = render_skill_content(&skill());
        assert!(rendered.contains(r#"<skill_content name="dsh-badge">"#));
        assert!(rendered.contains("<skill_instructions>"));
        assert!(rendered.contains("render a badge"));
        assert!(rendered.contains("</skill_content>"));

        let mut evil = skill();
        evil.summary.name = "a\"<&".to_owned();
        let rendered = render_skill_content(&evil);
        assert!(rendered.contains("a&quot;&lt;&amp;"));
    }

    #[test]
    fn escaping_is_total_for_prose_and_attributes() {
        assert_eq!(escape_text("a&b<c>d"), "a&amp;b&lt;c&gt;d");
        assert_eq!(escape_attr("a\"b<c&d"), "a&quot;b&lt;c&amp;d");
    }

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
        let registration = register_invariant(&registry).expect("register");
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.expect("dispose");
        register_invariant(&registry).expect("replacement");
    }
}
