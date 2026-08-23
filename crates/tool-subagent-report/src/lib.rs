//! Child-scoped `report` tool and model-visible delivery guidance.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_llm::ContentBlock;
use seekdeep_subagent::{SUBAGENTS, SubagentReportDelivery, SubagentReportOptions};
use seekdeep_system_prompt::{PromptSection, SYSTEM_PROMPT};
use seekdeep_tools::{DefineToolOptions, DefineToolOutput, TOOLS, define_tool};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Loader plugin name.
pub const NAME: &str = "tool-subagent-report";
/// Host services required before registering the child contribution.
pub const INJECT: &[&str] = &["subagents", "tools", "systemPrompt"];
const REPORT_SECTION_ORDER: f64 = 117.0;
const GUIDANCE: &str = "Deliver your result with the report tool before you finish: call it once with a self-contained answer. The agent that started you shares your workspace but does not automatically receive your transcript, tool output, or reasoning, so a closing remark such as \"done\" leaves it nothing it can use. Report earlier as well whenever a partial finding changes what that agent should do next; reporting never ends your turn.";
const DESCRIPTION: &str = "Report selected content to the agent that started you. Call this once before you finish, with a self-contained final result, and earlier for progress or findings that change what that agent does next. That agent shares your workspace but does not automatically receive your transcript, tool output, or reasoning, so finishing your work is not itself a result. Reporting does not end your turn or finish your work, and only your direct parent receives it. A failed call may still have arrived, so do not blindly repeat it.";

/// Parent scheduling policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportDelivery {
    /// Add context without waking an idle parent.
    Quiet,
    /// Queue one ordinary parent turn.
    #[default]
    Wakeup,
}

impl From<ReportDelivery> for SubagentReportDelivery {
    fn from(value: ReportDelivery) -> Self {
        match value {
            ReportDelivery::Quiet => Self::Quiet,
            ReportDelivery::Wakeup => Self::Wakeup,
        }
    }
}

/// Plugin configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Accepted reports wake the parent by default.
    pub report_delivery: ReportDelivery,
}

#[derive(Debug, Deserialize)]
struct ReportArgs {
    output: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportValue {
    message_id: seekdeep_llm::MessageId,
}

/// Installs the tool and guidance into one unpublished continuable child scope.
///
/// # Errors
///
/// Returns missing-service, duplicate-registration, or schema failures after
/// rolling back any earlier registration.
///
/// # Panics
///
/// The returned synchronous revoker panics after attempting both removals when
/// either scoped effect reports a cleanup failure.
pub fn install_report_tool(
    child_context: &Context,
    service_context: &Context,
    delivery: SubagentReportDelivery,
) -> anyhow::Result<Box<dyn Fn() + Send + Sync>> {
    let prompt = child_context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent-report requires systemPrompt"))?;
    let section = prompt.section(
        child_context,
        PromptSection::new("tool:report", REPORT_SECTION_ORDER, GUIDANCE),
    )?;
    let tools = child_context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent-report requires tools"))?;
    let subagents = service_context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent-report requires subagents"))?;
    let definition = define_tool(DefineToolOptions::new(
        "report",
        DESCRIPTION,
        json!({
            "output": {
                "type": "string",
                "required": true,
                "description": "Actionable content for your parent; summarize conclusions and reference relevant shared paths."
            }
        }),
        DefineToolOutput::new(
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "messageId": { "type": "string", "required": true }
                }
            }),
            Arc::new(|_args: &ReportArgs, value: &ReportValue| {
                Ok(vec![ContentBlock::Text {
                    text: format!(
                        "report accepted by the agent that started you as message {}",
                        value.message_id
                    ),
                }])
            }),
        ),
        Arc::new(move |args: ReportArgs, run| {
            let subagents = subagents.clone();
            Box::pin(async move {
                let agent = run
                    .execution()
                    .agent
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("report requires a live child Agent"))?;
                let message_id = subagents.report_from(
                    &agent,
                    vec![ContentBlock::Text { text: args.output }],
                    SubagentReportOptions {
                        delivery,
                        signal: run.signal(),
                    },
                )?;
                Ok(ReportValue { message_id })
            })
        }),
    ))?;
    let tool = match tools.register(child_context, definition) {
        Ok(tool) => tool,
        Err(error) => {
            let rollback = futures::executor::block_on(section.dispose());
            if let Err(rollback) = rollback {
                return Err(anyhow::anyhow!(
                    "{error:#}: prompt rollback failed: {rollback:#}"
                ));
            }
            return Err(error);
        }
    };
    Ok(Box::new(move || {
        let tool_result = futures::executor::block_on(tool.dispose());
        let section_result = futures::executor::block_on(section.dispose());
        if let Err(error) = tool_result.and(section_result) {
            panic!("failed to revoke report tool and prompt registrations: {error:#}");
        }
    }))
}

/// Registers the contribution for every later continuable child Activation.
///
/// # Errors
///
/// Returns missing-service or lifecycle registration failures.
///
/// # Panics
///
/// Panics inside child materialization when scoped tool or guidance
/// installation fails; the unpublished Agent transaction catches that failure
/// and rolls back the child.
pub fn install(context: &Context, config: Config) -> anyhow::Result<EffectHandle> {
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent-report requires subagents"))?;
    let service_context = context.clone();
    let delivery = SubagentReportDelivery::from(config.report_delivery);
    subagents.register_continuable_setup(Arc::new(move |child_context| {
        install_report_tool(child_context, &service_context, delivery)
            .unwrap_or_else(|error| panic!("failed to install report tool: {error:#}"))
    }))
}

/// Builds the Loader-compatible plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config = serde_json::from_value::<Config>(config)?;
            install(&context, config)?;
            Ok(())
        })
    })
}
