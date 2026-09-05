//! Ordered system sections, dynamic context, tool schemas, and prompt variables.

use std::{
    collections::HashSet,
    future::Future,
    panic::AssertUnwindSafe,
    sync::{Arc, OnceLock},
};

use futures::FutureExt;
use indexmap::IndexMap;
use regex::Regex;
use seekdeep_cordis::{
    Context, CordisError, EventArgs, EventOptions, EventReply, Plugin, ServiceKey, events::Next,
    fiber::EffectHandle,
};
use seekdeep_core::session::Session;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{AbortSignal, ContextSnapshotSection, ToolSchema};
use seekdeep_scope::{
    ScopeKey, scope_target, scoped_event_args,
    store::{AnonymousEntries, LayerEffectOptions, NamedEntries, ScopeLayer, ScopedLayers},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Deployment persona's shadowable section name.
pub const PERSONA_SECTION: &str = "deployment:persona";
/// Prompt order of the persona slot.
pub const PERSONA_ORDER: f64 = 0.0;
/// Reserved tool-order marker where unlisted tools are inserted.
pub const TOOL_ORDER_REST: &str = "<unlisted-tools>";
/// Typed Cordis slot corresponding to `ctx.systemPrompt`.
pub const SYSTEM_PROMPT: ServiceKey<SystemPrompt> = ServiceKey::new("systemPrompt");
/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "system-prompt";
/// System prompt service has no service prerequisites.
pub const PLUGIN_INJECT: &[&str] = &[];

/// Merge-extensible context for one prompt assembly.
#[derive(Clone, Debug, Default)]
pub struct AssembleContext {
    /// Scope whose providers and listeners participate.
    pub scope: Option<ScopeKey>,
    /// Explicit signal for the request that owns this assembly.
    pub signal: Option<AbortSignal>,
    /// Exact durable agent session when assembly belongs to a live agent.
    pub agent_session: Option<Arc<Session>>,
    /// Plugin-defined lossless fields.
    pub fields: Map<String, Value>,
}

/// Per-assembly text provider.
pub type PromptTextProvider =
    Arc<dyn Fn(&AssembleContext) -> anyhow::Result<String> + Send + Sync + 'static>;

/// Static or dynamically resolved prompt text.
#[derive(Clone)]
pub enum PromptText {
    /// Exact static text.
    Static(String),
    /// Provider evaluated on each assembly.
    Dynamic(PromptTextProvider),
}

impl std::fmt::Debug for PromptText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static(text) => formatter.debug_tuple("Static").field(text).finish(),
            Self::Dynamic(_) => formatter.write_str("Dynamic(..)"),
        }
    }
}

impl PromptText {
    fn resolve(&self, context: &AssembleContext) -> anyhow::Result<String> {
        match self {
            Self::Static(text) => Ok(text.clone()),
            Self::Dynamic(provider) => provider(context),
        }
    }
}

impl From<String> for PromptText {
    fn from(value: String) -> Self {
        Self::Static(value)
    }
}

impl From<&str> for PromptText {
    fn from(value: &str) -> Self {
        Self::Static(value.to_owned())
    }
}

/// One ordered registry input section.
#[derive(Clone, Debug)]
pub struct PromptSection {
    /// Name unique within one layer.
    pub name: String,
    /// Ascending concatenation order.
    pub order: f64,
    /// Static or per-assembly text.
    pub text: PromptText,
    /// Whether this is the complete authoritative system prompt.
    pub complete: bool,
}

impl PromptSection {
    /// Builds a non-complete section.
    #[must_use]
    pub fn new(name: impl Into<String>, order: f64, text: impl Into<PromptText>) -> Self {
        Self {
            name: name.into(),
            order,
            text: text.into(),
            complete: false,
        }
    }

    /// Marks this contribution as the complete prompt.
    #[must_use]
    pub fn complete(mut self) -> Self {
        self.complete = true;
        self
    }
}

/// Ordered dynamic runtime-context registry input.
#[derive(Clone, Debug)]
pub struct PromptContext {
    /// Name unique within one layer.
    pub name: String,
    /// Ascending join order.
    pub order: f64,
    /// Static or per-assembly text.
    pub text: PromptText,
}

impl PromptContext {
    /// Builds a runtime-context contribution.
    #[must_use]
    pub fn new(name: impl Into<String>, order: f64, text: impl Into<PromptText>) -> Self {
        Self {
            name: name.into(),
            order,
            text: text.into(),
        }
    }
}

/// One section after its text provider resolves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledSection {
    /// Source section name.
    pub name: String,
    /// Resolved, uninterpolated text.
    pub text: String,
}

/// One dynamic context after its text provider resolves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledContext {
    /// Source context name.
    pub name: String,
    /// Resolved, uninterpolated text.
    pub text: String,
}

/// Tool schemas visible in one provider's assembly view.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolProviderResult {
    /// Schemas contributed to this assembly.
    pub schemas: Vec<ToolSchema>,
    /// Pre-restriction universe, defaulting to schema names.
    pub known_names: Option<Vec<String>>,
}

/// Complete mutable assembly passed through the expert waterfall.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromptAssembly {
    /// Ordered prompt sections.
    pub sections: Vec<AssembledSection>,
    /// Ordered dynamic context contributions.
    pub contexts: Vec<AssembledContext>,
    /// Canonically ordered model tool schemas.
    pub tools: Vec<ToolSchema>,
    /// Resolved prompt variables.
    pub variables: IndexMap<String, Option<String>>,
}

struct BaseAssembly {
    assembly: PromptAssembly,
    complete_section: Option<AssembledSection>,
    runtime_context_suppressed: bool,
}

/// Deployment-authored prompt configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SystemPromptConfig {
    /// Include `SeekDeep`'s fixed identity opener.
    pub include_harness_identity: bool,
    /// Include dynamic runtime-context snapshots.
    pub include_runtime_context: bool,
    /// Deployment-wide order-zero persona.
    pub persona: String,
    /// Explicit model tool order containing [`TOOL_ORDER_REST`].
    pub tool_order: Option<Vec<String>>,
}

impl Default for SystemPromptConfig {
    fn default() -> Self {
        Self {
            include_harness_identity: true,
            include_runtime_context: true,
            persona: String::new(),
            tool_order: None,
        }
    }
}

/// Per-assembly tool-schema provider.
pub type ToolProvider =
    Arc<dyn Fn(&AssembleContext) -> anyhow::Result<ToolProviderResult> + Send + Sync + 'static>;
/// Per-assembly variable provider.
pub type VariableProvider =
    Arc<dyn Fn(&AssembleContext) -> anyhow::Result<Option<String>> + Send + Sync + 'static>;

struct PromptLayer {
    sections: NamedEntries<PromptSection>,
    contexts: NamedEntries<PromptContext>,
    runtime_context_suppressors: AnonymousEntries<bool>,
    tool_providers: AnonymousEntries<ToolProvider>,
    variables: NamedEntries<VariableProvider>,
}

impl PromptLayer {
    fn new(scope: Option<ScopeKey>) -> Self {
        Self {
            sections: NamedEntries::new(move |name| duplicate_error("section", name, scope)),
            contexts: NamedEntries::new(move |name| duplicate_error("context", name, scope)),
            runtime_context_suppressors: AnonymousEntries::default(),
            tool_providers: AnonymousEntries::default(),
            variables: NamedEntries::new(move |name| duplicate_error("variable", name, scope)),
        }
    }
}

impl ScopeLayer for PromptLayer {
    fn is_empty(&self) -> bool {
        self.sections.is_empty()
            && self.contexts.is_empty()
            && self.runtime_context_suppressors.is_empty()
            && self.tool_providers.is_empty()
            && self.variables.is_empty()
    }
}

fn duplicate_error(kind: &str, name: &str, scope: Option<ScopeKey>) -> anyhow::Error {
    if scope.is_some() {
        anyhow::anyhow!("prompt {kind} {name:?} is already registered in this scope")
    } else {
        anyhow::anyhow!(
            "prompt {kind} {name:?} is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)"
        )
    }
}

/// Typed continuation for the expert assembly waterfall.
pub struct AssembleNext {
    next: Next,
    context: AssembleContext,
}

impl AssembleNext {
    /// Delegates to the remaining assembly middleware.
    ///
    /// # Errors
    ///
    /// Returns a downstream failure or invalid reply type.
    pub async fn run(self) -> anyhow::Result<PromptAssembly> {
        self.next
            .run()
            .await?
            .downcast::<PromptAssembly>()
            .map(|assembly| (*assembly).clone())
            .ok_or_else(|| anyhow::anyhow!("system-prompt/assemble returned an invalid assembly"))
    }

    /// Delegates after replacing the mutable assembly seen downstream.
    ///
    /// # Errors
    ///
    /// Returns a downstream failure or invalid reply type.
    pub async fn run_with(self, assembly: PromptAssembly) -> anyhow::Result<PromptAssembly> {
        self.next
            .run_with(EventArgs::from_values(vec![
                Arc::new(assembly),
                Arc::new(self.context),
            ]))
            .await?
            .downcast::<PromptAssembly>()
            .map(|assembly| (*assembly).clone())
            .ok_or_else(|| anyhow::anyhow!("system-prompt/assemble returned an invalid assembly"))
    }
}

/// Registry service assembled before every model request.
pub struct SystemPrompt {
    context: Context,
    layers: ScopedLayers<PromptLayer>,
    tool_order: Option<Vec<String>>,
}

impl std::fmt::Debug for SystemPrompt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SystemPrompt")
            .field("tool_order", &self.tool_order)
            .finish_non_exhaustive()
    }
}

impl SystemPrompt {
    /// Constructs a registry and installs configured deployment contributions.
    ///
    /// # Errors
    ///
    /// Returns invalid tool order or initial-registration failures.
    pub fn new(context: &Context, config: SystemPromptConfig) -> anyhow::Result<Arc<Self>> {
        let tool_order = validate_tool_order(config.tool_order)?;
        let change_context = context.clone();
        let layers = ScopedLayers::try_new(
            |scope| Ok(PromptLayer::new(scope)),
            move || {
                change_context.events().emit(
                    &change_context,
                    "system-prompt/change",
                    &EventArgs::new(),
                )
            },
        )?;
        let service = Arc::new(Self {
            context: context.clone(),
            layers,
            tool_order,
        });
        if config.include_harness_identity {
            service.section(
                context,
                PromptSection::new(
                    "harness:identity",
                    -100.0,
                    "You are an AI agent powered by SeekDeep Harness.",
                ),
            )?;
        }
        service.section(
            context,
            PromptSection::new(PERSONA_SECTION, PERSONA_ORDER, config.persona),
        )?;
        if !config.include_runtime_context {
            service.suppress_runtime_context(context)?;
        }
        Ok(service)
    }

    /// Provides this registry on `ctx.systemPrompt` for the mounting fiber.
    ///
    /// # Errors
    ///
    /// Returns standard duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(SYSTEM_PROMPT, self.clone())
    }

    /// Registers typed assembly middleware.
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn on_assemble<F, Fut>(
        &self,
        context: &Context,
        middleware: F,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError>
    where
        F: Fn(PromptAssembly, AssembleContext, AssembleNext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<PromptAssembly>> + Send + 'static,
    {
        self.context.events().on_waterfall(
            context,
            "system-prompt/assemble",
            move |_, args, next| {
                let Some(assembly) = args.get::<PromptAssembly>(0) else {
                    return Box::pin(async {
                        Err(anyhow::anyhow!(
                            "system-prompt/assemble is missing its assembly"
                        ))
                    });
                };
                let Some(assemble_context) = args.get::<AssembleContext>(1) else {
                    return Box::pin(async {
                        Err(anyhow::anyhow!(
                            "system-prompt/assemble is missing its context"
                        ))
                    });
                };
                let future = middleware(
                    (*assembly).clone(),
                    (*assemble_context).clone(),
                    AssembleNext {
                        next,
                        context: (*assemble_context).clone(),
                    },
                );
                Box::pin(async move {
                    let assembly = AssertUnwindSafe(future)
                        .catch_unwind()
                        .await
                        .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))??;
                    Ok(EventReply::Value(Arc::new(assembly)))
                })
            },
            options,
        )
    }

    /// Registers an ordered global or scoped prompt section.
    ///
    /// # Errors
    ///
    /// Returns for non-finite order, duplicate name, notification failure, or
    /// inactive ownership.
    pub fn section(
        &self,
        context: &Context,
        section: PromptSection,
    ) -> anyhow::Result<EffectHandle> {
        anyhow::ensure!(
            section.order.is_finite(),
            "prompt section {:?} order must be a finite number",
            section.name
        );
        let name = section.name.clone();
        self.layers.effect(
            context,
            move |layer| layer.sections.insert(name, section),
            LayerEffectOptions::new("systemPrompt.section()"),
        )
    }

    /// Registers ordered dynamic runtime context.
    ///
    /// # Errors
    ///
    /// Returns for non-finite order, duplicate name, notification failure, or
    /// inactive ownership.
    pub fn prompt_context(
        &self,
        context: &Context,
        prompt_context: PromptContext,
    ) -> anyhow::Result<EffectHandle> {
        anyhow::ensure!(
            prompt_context.order.is_finite(),
            "prompt context {:?} order must be a finite number",
            prompt_context.name
        );
        let name = prompt_context.name.clone();
        self.layers.effect(
            context,
            move |layer| layer.contexts.insert(name, prompt_context),
            LayerEffectOptions::new("systemPrompt.context()"),
        )
    }

    /// Suppresses all dynamic context for the selected scope.
    ///
    /// # Errors
    ///
    /// Returns notification or inactive-ownership failures.
    pub fn suppress_runtime_context(&self, context: &Context) -> anyhow::Result<EffectHandle> {
        self.layers.effect(
            context,
            |layer| Ok(layer.runtime_context_suppressors.append(true)),
            LayerEffectOptions::new("systemPrompt.suppressRuntimeContext()"),
        )
    }

    /// Registers one tool-schema provider.
    ///
    /// # Errors
    ///
    /// Returns notification or inactive-ownership failures.
    pub fn tools(&self, context: &Context, provider: ToolProvider) -> anyhow::Result<EffectHandle> {
        self.layers.effect(
            context,
            move |layer| Ok(layer.tool_providers.append(provider)),
            LayerEffectOptions::new("systemPrompt.tools()"),
        )
    }

    /// Registers one prompt variable provider.
    ///
    /// # Errors
    ///
    /// Returns for invalid/duplicate name, notification failure, or inactive ownership.
    pub fn variable(
        &self,
        context: &Context,
        name: impl Into<String>,
        provider: VariableProvider,
    ) -> anyhow::Result<EffectHandle> {
        let name = name.into();
        anyhow::ensure!(
            variable_name().is_match(&name),
            "invalid prompt variable name {name:?} (must match /^[a-z][a-z0-9_]*$/)"
        );
        self.layers.effect(
            context,
            move |layer| layer.variables.insert(name, provider),
            LayerEffectOptions::new("systemPrompt.variable()"),
        )
    }

    /// Assembles the global and selected scope-chain contributions.
    ///
    /// # Errors
    ///
    /// Returns provider, ordering, complete-section, or waterfall failures.
    pub async fn assemble(
        &self,
        assemble_context: AssembleContext,
    ) -> anyhow::Result<PromptAssembly> {
        let scope = assemble_context.scope;
        let BaseAssembly {
            assembly,
            complete_section,
            runtime_context_suppressed,
        } = self.build_base_assembly(&assemble_context)?;
        let args = EventArgs::from_values(vec![
            Arc::new(assembly.clone()),
            Arc::new(assemble_context.clone()),
        ]);
        let args = match scope {
            Some(scope) => scoped_event_args(scope, args),
            None => args,
        };
        let reply = self
            .context
            .events()
            .waterfall_with_args(
                &scope_target(&self.context, scope),
                "system-prompt/assemble",
                &args,
                move |args| {
                    Box::pin(async move {
                        let assembly = args.get::<PromptAssembly>(0).ok_or_else(|| {
                            anyhow::anyhow!("system-prompt/assemble lost its assembly")
                        })?;
                        Ok(EventReply::Value(assembly))
                    })
                },
            )
            .await?;
        let mut transformed = reply
            .downcast::<PromptAssembly>()
            .map(|assembly| (*assembly).clone())
            .ok_or_else(|| {
                anyhow::anyhow!("system-prompt/assemble returned an invalid assembly")
            })?;
        if let Some(complete) = complete_section {
            transformed.sections = vec![complete];
        }
        if runtime_context_suppressed {
            transformed.contexts.clear();
        }
        Ok(transformed)
    }

    fn build_base_assembly(
        &self,
        assemble_context: &AssembleContext,
    ) -> anyhow::Result<BaseAssembly> {
        let scope = assemble_context.scope;
        let scope_layers = self.layers.chain_layers(scope);
        let runtime_context_suppressed = !self.layers.global.runtime_context_suppressors.is_empty()
            || scope_layers
                .iter()
                .any(|layer| !layer.runtime_context_suppressors.is_empty());

        let mut variables = IndexMap::new();
        for (name, provider) in self.layers.global.variables.entries() {
            variables.insert(name, provider(assemble_context)?);
        }
        for layer in &scope_layers {
            for (name, provider) in layer.variables.entries() {
                variables.insert(name, provider(assemble_context)?);
            }
        }

        let section_by_name = self.layers.merge(scope, |layer| &layer.sections);
        let context_by_name = self.layers.merge(scope, |layer| &layer.contexts);
        let providers = self
            .layers
            .global
            .tool_providers
            .values()
            .chain(
                scope_layers
                    .iter()
                    .flat_map(|layer| layer.tool_providers.values()),
            )
            .collect::<Vec<_>>();
        let mut collected = Vec::new();
        let mut known_names = HashSet::new();
        for provider in providers {
            let result = provider(assemble_context)?;
            let accepted_known = result.known_names.unwrap_or_else(|| {
                result
                    .schemas
                    .iter()
                    .map(|schema| schema.name.clone())
                    .collect()
            });
            collected.extend(result.schemas.into_iter().map(detach_tool_schema));
            known_names.extend(accepted_known);
        }

        let mut section_definitions = section_by_name.into_values().collect::<Vec<_>>();
        section_definitions.sort_by(|left, right| left.order.total_cmp(&right.order));
        let complete_names = section_definitions
            .iter()
            .filter(|section| section.complete)
            .map(|section| section.name.clone())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            complete_names.len() <= 1,
            "multiple complete prompt sections are active: {}",
            complete_names
                .iter()
                .map(|name| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut complete_section = None;
        let sections = section_definitions
            .into_iter()
            .map(|section| {
                let assembled = AssembledSection {
                    name: section.name,
                    text: section.text.resolve(assemble_context)?,
                };
                if section.complete {
                    complete_section = Some(assembled.clone());
                }
                Ok(assembled)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let contexts = if runtime_context_suppressed {
            Vec::new()
        } else {
            let mut definitions = context_by_name.into_values().collect::<Vec<_>>();
            definitions.sort_by(|left, right| left.order.total_cmp(&right.order));
            definitions
                .into_iter()
                .map(|entry| {
                    Ok(AssembledContext {
                        name: entry.name,
                        text: entry.text.resolve(assemble_context)?,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        };
        Ok(BaseAssembly {
            assembly: PromptAssembly {
                sections,
                contexts,
                tools: order_tools(collected, self.tool_order.as_deref(), &known_names)?,
                variables,
            },
            complete_section,
            runtime_context_suppressed,
        })
    }
}

/// Constructs and lifecycle-mounts `ctx.systemPrompt`.
///
/// # Errors
///
/// Returns configuration, initial-registration, or service-publication failures.
pub fn install(context: &Context, config: SystemPromptConfig) -> anyhow::Result<Arc<SystemPrompt>> {
    let prompt = SystemPrompt::new(context, config)?;
    prompt.provide(context)?;
    Ok(prompt)
}

/// Builds the Loader-compatible system-prompt plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(
        PLUGIN_NAME,
        PLUGIN_INJECT.iter().copied(),
        |context, config| {
            Box::pin(async move {
                install(&context, serde_json::from_value(config)?)?;
                Ok(())
            })
        },
    )
}

/// Interpolates variables, drops empty sections, and joins with blank lines.
///
/// # Errors
///
/// Returns malformed, unknown, or undefined variable diagnostics.
pub fn render_prompt(assembly: &PromptAssembly) -> anyhow::Result<String> {
    Ok(assembly
        .sections
        .iter()
        .map(|section| interpolate(&section.name, &section.text, &assembly.variables, "section"))
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"))
}

/// Renders named nonempty dynamic context sections.
///
/// # Errors
///
/// Returns malformed, unknown, or undefined variable diagnostics.
pub fn render_context_sections(
    assembly: &PromptAssembly,
) -> anyhow::Result<Vec<ContextSnapshotSection>> {
    Ok(assembly
        .contexts
        .iter()
        .map(|context| {
            Ok(ContextSnapshotSection {
                name: context.name.clone(),
                text: interpolate(&context.name, &context.text, &assembly.variables, "context")?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .filter(|section| !section.text.is_empty())
        .collect())
}

/// Joins already-rendered context sections into the superseding snapshot.
#[must_use]
pub fn join_context_sections(sections: &[ContextSnapshotSection]) -> String {
    let body = sections
        .iter()
        .map(|section| section.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if body.is_empty() {
        String::new()
    } else {
        format!(
            "Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\n{body}"
        )
    }
}

/// Renders the complete current dynamic-context snapshot.
///
/// # Errors
///
/// Returns malformed, unknown, or undefined variable diagnostics.
pub fn render_context_snapshot(assembly: &PromptAssembly) -> anyhow::Result<String> {
    Ok(join_context_sections(&render_context_sections(assembly)?))
}

/// Registers validation around the authoritative assembly waterfall result.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        "seekdeep-system-prompt",
        InvariantInstaller::new(
            std::iter::empty::<String>(),
            |context, failure| async move {
                context.events().on_waterfall(
                    &context,
                    "system-prompt/assemble",
                    move |_, _, next| {
                        let failure = failure.clone();
                        Box::pin(async move {
                            let reply = next.run().await?;
                            let assembly = reply.downcast::<PromptAssembly>().ok_or_else(|| {
                                failure.fail("authoritative assembly is not a prompt assembly")
                            })?;
                            validate_assembly(&assembly, &failure)?;
                            Ok(reply)
                        })
                    },
                    EventOptions {
                        prepend: true,
                        global: true,
                    },
                )?;
                Ok(())
            },
        ),
    )
}

fn validate_assembly(
    assembly: &PromptAssembly,
    failure: &seekdeep_invariants::InvariantFailure,
) -> anyhow::Result<()> {
    let mut section_names = HashSet::new();
    for section in &assembly.sections {
        if section.name.is_empty() {
            return Err(failure
                .fail("assembled section names must be non-empty")
                .into());
        }
        if !section_names.insert(section.name.as_str()) {
            return Err(failure
                .fail(format!(
                    "assembled section name {:?} is duplicated",
                    section.name
                ))
                .into());
        }
    }

    let mut context_names = HashSet::new();
    for context in &assembly.contexts {
        if context.name.is_empty() {
            return Err(failure
                .fail("assembled context names must be non-empty")
                .into());
        }
        if !context_names.insert(context.name.as_str()) {
            return Err(failure
                .fail(format!(
                    "assembled context name {:?} is duplicated",
                    context.name
                ))
                .into());
        }
    }

    if assembly.tools.iter().any(|tool| tool.name.is_empty()) {
        return Err(failure
            .fail("assembled tool names must be non-empty")
            .into());
    }
    for name in assembly.variables.keys() {
        if !variable_name().is_match(name) {
            return Err(failure
                .fail(format!("assembled variable name {name:?} is invalid"))
                .into());
        }
    }
    Ok(())
}

fn detach_tool_schema(schema: ToolSchema) -> ToolSchema {
    ToolSchema {
        name: schema.name,
        description: schema.description,
        parameters: schema.parameters.clone(),
    }
}

fn validate_tool_order(order: Option<Vec<String>>) -> anyhow::Result<Option<Vec<String>>> {
    let Some(order) = order else {
        return Ok(None);
    };
    let mut seen = HashSet::new();
    for name in &order {
        anyhow::ensure!(
            seen.insert(name.clone()),
            "toolOrder lists {name:?} more than once"
        );
    }
    anyhow::ensure!(
        seen.contains(TOOL_ORDER_REST),
        "toolOrder must contain the {TOOL_ORDER_REST:?} rest entry (where unlisted tools are inserted)"
    );
    Ok(Some(order))
}

fn order_tools(
    mut tools: Vec<ToolSchema>,
    order: Option<&[String]>,
    known_names: &HashSet<String>,
) -> anyhow::Result<Vec<ToolSchema>> {
    anyhow::ensure!(
        tools.iter().all(|tool| tool.name != TOOL_ORDER_REST),
        "tool provider returned reserved tool name {TOOL_ORDER_REST:?} (reserved for toolOrder's rest entry)"
    );
    let Some(order) = order else {
        tools.sort_by(|left, right| js_code_unit_cmp(&left.name, &right.name));
        return Ok(tools);
    };
    let unknown = order
        .iter()
        .filter(|name| name.as_str() != TOOL_ORDER_REST && !known_names.contains(*name))
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        let mut known = known_names.iter().cloned().collect::<Vec<_>>();
        known.sort_by(|left, right| js_code_unit_cmp(left, right));
        anyhow::bail!(
            "toolOrder lists unregistered tool{} {}; known tools: {}",
            if unknown.len() == 1 { "" } else { "s" },
            unknown
                .iter()
                .map(|name| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", "),
            if known.is_empty() {
                "(none)".to_owned()
            } else {
                known.join(", ")
            }
        );
    }
    let listed = order.iter().cloned().collect::<HashSet<_>>();
    let mut rest = tools
        .iter()
        .filter(|tool| !listed.contains(&tool.name))
        .cloned()
        .collect::<Vec<_>>();
    rest.sort_by(|left, right| js_code_unit_cmp(&left.name, &right.name));
    let mut ordered = Vec::new();
    for name in order {
        if name == TOOL_ORDER_REST {
            ordered.extend(rest.clone());
        } else {
            ordered.extend(tools.iter().filter(|tool| &tool.name == name).cloned());
        }
    }
    Ok(ordered)
}

fn js_code_unit_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn interpolate(
    input_name: &str,
    text: &str,
    variables: &IndexMap<String, Option<String>>,
    kind: &str,
) -> anyhow::Result<String> {
    let mut result = String::new();
    let mut cursor = 0;
    while let Some(relative_open) = text[cursor..].find("{{") {
        let open = cursor + relative_open;
        result.push_str(&text[cursor..open]);
        let after_open = open + 2;
        let Some(relative_close) = text[after_open..].find("}}") else {
            result.push_str("{{");
            cursor = after_open;
            continue;
        };
        let close = after_open + relative_close;
        let name = &text[after_open..close];
        if name.contains(['{', '}']) || !variable_name().is_match(name) {
            anyhow::bail!(
                "malformed prompt variable reference {{\u{7b}{name}\u{7d}}} in {kind} {input_name:?} (variable names match /^[a-z][a-z0-9_]*$/)"
            );
        }
        let value = variables.get(name).ok_or_else(|| {
            let known = if variables.is_empty() {
                "(none)".to_owned()
            } else {
                variables.keys().cloned().collect::<Vec<_>>().join(", ")
            };
            anyhow::anyhow!(
                "unknown prompt variable \"{{{{{name}}}}}\" in {kind} {input_name:?}; registered variables: {known}"
            )
        })?;
        let value = value.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "prompt variable \"{{{{{name}}}}}\" has no value for this assembly ({kind} {input_name:?})"
            )
        })?;
        result.push_str(value);
        cursor = close + 2;
    }
    result.push_str(&text[cursor..]);
    Ok(result)
}

fn variable_name() -> &'static Regex {
    static VARIABLE_NAME: OnceLock<Regex> = OnceLock::new();
    VARIABLE_NAME.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]*$").expect("constant regex"))
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload.downcast_ref::<String>().map_or_else(
        || {
            payload
                .downcast_ref::<&'static str>()
                .map_or_else(|| "panic".to_owned(), |message| (*message).to_owned())
        },
        Clone::clone,
    )
}

#[cfg(test)]
mod tests {
    use seekdeep_scope::create_scope;
    use serde_json::json;

    use super::*;

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_owned(),
            description: format!("{name} tool"),
            parameters: Map::from_iter([("type".to_owned(), json!("object"))]),
        }
    }

    #[tokio::test]
    async fn assembles_scoped_shadows_variables_context_and_tools() {
        let root = Context::new();
        let prompt = SystemPrompt::new(
            &root,
            SystemPromptConfig {
                persona: "Global {{name}}".to_owned(),
                tool_order: Some(vec!["z".to_owned(), TOOL_ORDER_REST.to_owned()]),
                ..SystemPromptConfig::default()
            },
        )
        .expect("prompt");
        prompt
            .variable(&root, "name", Arc::new(|_| Ok(Some("world".to_owned()))))
            .expect("variable");
        prompt
            .prompt_context(&root, PromptContext::new("cwd", 0.0, "/tmp"))
            .expect("context");
        prompt
            .tools(
                &root,
                Arc::new(|_| {
                    Ok(ToolProviderResult {
                        schemas: vec![tool("a"), tool("z")],
                        known_names: None,
                    })
                }),
            )
            .expect("tools");
        let key = ScopeKey::new();
        let scope = create_scope(&root, key, None).expect("scope");
        prompt
            .section(
                &scope.context,
                PromptSection::new(PERSONA_SECTION, PERSONA_ORDER, "Scoped {{name}}"),
            )
            .expect("scoped persona");
        let assembly = prompt
            .assemble(AssembleContext {
                scope: Some(key),
                ..AssembleContext::default()
            })
            .await
            .expect("assembly");
        assert_eq!(
            render_prompt(&assembly).expect("render"),
            "You are an AI agent powered by SeekDeep Harness.\n\nScoped world"
        );
        assert_eq!(
            assembly
                .tools
                .iter()
                .map(|schema| schema.name.as_str())
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        assert!(
            render_context_snapshot(&assembly)
                .expect("snapshot")
                .contains("/tmp")
        );
        scope.dispose().await.expect("dispose");
    }

    #[tokio::test]
    async fn complete_section_and_suppression_survive_waterfall_edits() {
        let root = Context::new();
        let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).expect("prompt");
        prompt
            .section(
                &root,
                PromptSection::new("complete", 5.0, "only").complete(),
            )
            .expect("complete");
        prompt
            .prompt_context(&root, PromptContext::new("context", 0.0, "secret"))
            .expect("context");
        prompt.suppress_runtime_context(&root).expect("suppress");
        prompt
            .on_assemble(
                &root,
                |mut assembly, _, _| async move {
                    assembly.sections.push(AssembledSection {
                        name: "injected".to_owned(),
                        text: "injected".to_owned(),
                    });
                    assembly.contexts.push(AssembledContext {
                        name: "injected".to_owned(),
                        text: "leak".to_owned(),
                    });
                    Ok(assembly)
                },
                EventOptions::default(),
            )
            .expect("middleware");
        let assembly = prompt
            .assemble(AssembleContext::default())
            .await
            .expect("assembly");
        assert_eq!(
            assembly.sections,
            [AssembledSection {
                name: "complete".to_owned(),
                text: "only".to_owned()
            }]
        );
        assert!(assembly.contexts.is_empty());
    }

    #[test]
    fn interpolation_is_strict_but_leaves_unclosed_openers_literal() {
        let assembly = PromptAssembly {
            sections: vec![AssembledSection {
                name: "x".to_owned(),
                text: "hello {{name}} and {{ literal".to_owned(),
            }],
            variables: IndexMap::from([("name".to_owned(), Some("seekdeep".to_owned()))]),
            ..PromptAssembly::default()
        };
        assert_eq!(
            render_prompt(&assembly).expect("render"),
            "hello seekdeep and {{ literal"
        );
        let mut unknown = assembly;
        unknown.sections[0].text = "{{missing}}".to_owned();
        assert!(render_prompt(&unknown).is_err());
    }
}
