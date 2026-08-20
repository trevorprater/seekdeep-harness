//! Shared in-process child composition.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, CreateAgentMeta};
use seekdeep_cordis::Context;
use seekdeep_core::session::{AppendOptions, Session, SessionId, SessionOrigin};
use seekdeep_sandbox::SandboxMode;
use seekdeep_sandbox_policy::SANDBOX_POLICY;
use seekdeep_system_prompt::{PromptContext, PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{TOOLS, ToolRestriction};
use seekdeep_user_approval::APPROVAL;

use crate::depth::delegation_depth_of;

/// Thrown when starting a child would exceed the requested depth cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentDepthError {
    /// The attempted depth.
    pub attempted_depth: u64,
    /// The cap that was exceeded.
    pub max_depth: u64,
}

impl std::fmt::Display for SubagentDepthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "subagent depth {} exceeds maxDepth {}",
            self.attempted_depth, self.max_depth
        )
    }
}

impl std::error::Error for SubagentDepthError {}

/// Resolves the child's delegation depth and enforces an optional cap.
///
/// # Errors
///
/// Returns when the resolved depth exceeds the cap or leaves the safe-integer range.
pub fn resolve_child_depth(parent: &Agent, max_depth: Option<u64>) -> anyhow::Result<u64> {
    let child_depth = delegation_depth_of(parent) + 1;
    if let Some(max_depth) = max_depth
        && child_depth > max_depth
    {
        return Err(SubagentDepthError {
            attempted_depth: child_depth,
            max_depth,
        }
        .into());
    }
    Ok(child_depth)
}

/// Resolves the child's `AgentOptions`, inheriting the parent route unless overridden.
#[must_use]
pub fn resolve_child_agent_options(
    parent: &Agent,
    requested: Option<AgentOptions>,
    child_depth: u64,
) -> AgentOptions {
    let mut options = requested.unwrap_or_default();
    if options.provider.is_none() {
        options.provider.clone_from(&parent.options().provider);
    }
    if options.model.is_none() {
        options.model.clone_from(&parent.options().model);
    }
    if options.max_tokens.is_none() {
        options.max_tokens = parent.options().max_tokens;
    }
    options.subagent_depth = Some(child_depth);
    options
}

/// Builds the child session's durable creation metadata.
#[must_use]
pub fn child_session_meta(
    parent: &Agent,
    child_depth: u64,
    lineage_seed_length: u64,
) -> CreateAgentMeta {
    let parent_header = parent.session().header();
    CreateAgentMeta {
        cwd: parent_header.cwd.clone(),
        parent_session: Some(parent_header.id.clone()),
        seed_length: (lineage_seed_length > 0).then_some(lineage_seed_length),
        origin: Some(SessionOrigin::Subagent),
        delegation_depth: Some(child_depth),
        // The parent's live preset composition is read opportunistically;
        // the agent-presets package is not yet ported.
        agent_preset: None,
    }
}

/// Model-facing delegation-scope statement for every in-process child.
pub const SUBAGENT_DELEGATION_CONTEXT: &str = "You are a delegated subagent: your permission scope was fixed when you were started and cannot be widened from inside this session — operations that require approval are rejected automatically. When the task needs access beyond that scope, do not retry the denied operation; state the limitation in your reply so the delegating agent can handle it.";

/// The scoped composition a child agent's creation window applies.
#[derive(Clone, Debug, Default)]
pub struct ChildComposition {
    /// Per-child persona shadowing the deployment persona.
    pub persona: Option<String>,
    /// Per-child tool scoping.
    pub tool_filter: Option<ToolRestriction>,
}

/// Composes one child inside its creation window.
///
/// # Errors
///
/// Returns prompt-registration or restriction failures.
pub fn apply_child_composition(
    child_ctx: &Context,
    composition: &ChildComposition,
) -> anyhow::Result<()> {
    let prompt = child_ctx
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("subagent child requires systemPrompt"))?;
    prompt.prompt_context(
        child_ctx,
        PromptContext::new(
            "subagent:delegation",
            120.0,
            PromptText::Static(SUBAGENT_DELEGATION_CONTEXT.to_owned()),
        ),
    )?;
    if let Some(persona) = &composition.persona {
        prompt.section(
            child_ctx,
            PromptSection::new(
                "deployment:persona",
                0.0,
                PromptText::Static(persona.clone()),
            ),
        )?;
    }
    if let Some(tool_filter) = &composition.tool_filter {
        let tools = child_ctx
            .get(TOOLS)
            .ok_or_else(|| anyhow::anyhow!("subagent child requires tools"))?;
        tools.restrict(child_ctx, tool_filter.clone())?;
    }
    Ok(())
}

/// Policy seeded onto a child session's log at the delegation boundary.
#[derive(Clone, Debug, Default)]
pub struct DelegatedPolicyOverrides {
    /// The parent session's explicit sandbox-mode override.
    pub sandbox_mode: Option<SandboxMode>,
    /// The approval pin (always "never" when approval is composed).
    pub approval_policy: Option<String>,
}

/// Captures the policy to seed into one delegation.
#[must_use]
pub fn capture_delegated_policy_overrides(parent: &Agent) -> DelegatedPolicyOverrides {
    let sandbox_mode = parent
        .context()
        .get(SANDBOX_POLICY)
        .and_then(|policy| policy.override_of(parent.session()));
    let approval_policy = if parent.context().get(APPROVAL).is_some() {
        Some("never".to_owned())
    } else {
        None
    };
    DelegatedPolicyOverrides {
        sandbox_mode,
        approval_policy,
    }
}

/// Appends the captured delegation policy onto the child's own log.
///
/// # Errors
///
/// Returns append failures.
pub fn append_delegated_policy_overrides(
    child_session: &Session,
    overrides: &DelegatedPolicyOverrides,
) -> anyhow::Result<()> {
    if let Some(mode) = overrides.sandbox_mode {
        child_session.append(
            "sandbox/mode",
            serde_json::json!({"mode": mode, "source": "delegation"}),
            AppendOptions::default(),
        )?;
    }
    if let Some(policy) = &overrides.approval_policy {
        child_session.append(
            "approval/policy",
            serde_json::json!({"policy": policy, "source": "delegation"}),
            AppendOptions::default(),
        )?;
    }
    Ok(())
}

/// Identity and lineage inputs shared by every in-process child creation.
pub struct ChildCreateInputs {
    /// The child's reserved session id.
    pub session_id: SessionId,
    /// The delegating parent agent.
    pub parent: Arc<Agent>,
    /// The resolved delegation depth.
    pub child_depth: u64,
    /// How many leading seed events came from the parent's log.
    pub lineage_seed_length: u64,
}
