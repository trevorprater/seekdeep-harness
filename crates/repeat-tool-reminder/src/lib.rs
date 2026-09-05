//! Advisory per-agent detection of repeated identical tool calls.

use std::{cmp::Ordering, sync::Arc};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentEvent};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use seekdeep_tools::{PostToolDecision, ToolExecution};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

/// Cordis plugin name.
pub const NAME: &str = "repeat-tool-reminder";
/// Raw event listeners can install before the agent or tool services exist.
pub const INJECT: &[&str] = &[];

const GENTLE_REMINDER: &str = "You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call.";

fn default_thresholds() -> Vec<f64> {
    vec![3.0, 5.0, 8.0]
}

const fn default_preview_chars() -> f64 {
    500.0
}

/// Repeat-call detection and reminder policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RepeatToolReminderConfig {
    /// Consecutive-repeat counts that trigger a reminder.
    pub thresholds: Vec<f64>,
    /// Tool-name wildcard patterns to track; empty tracks every tool.
    pub include: Vec<String>,
    /// Tool-name wildcard patterns transparent to the chain.
    pub exclude: Vec<String>,
    /// Maximum UTF-16 code units quoted from canonical arguments.
    pub arguments_preview_chars: f64,
}

impl Default for RepeatToolReminderConfig {
    fn default() -> Self {
        Self {
            thresholds: default_thresholds(),
            include: Vec::new(),
            exclude: Vec::new(),
            arguments_preview_chars: default_preview_chars(),
        }
    }
}

#[derive(Clone, Debug)]
struct Wildcard {
    units: Vec<u16>,
}

impl Wildcard {
    fn new(pattern: &str) -> Self {
        Self {
            units: pattern.encode_utf16().collect(),
        }
    }

    fn matches(&self, value: &str) -> bool {
        let value = value.encode_utf16().collect::<Vec<_>>();
        let (mut pattern_index, mut value_index) = (0, 0);
        let (mut star, mut retry_value) = (None, 0);
        while value_index < value.len() {
            if self.units.get(pattern_index) == value.get(value_index)
                && self.units.get(pattern_index) != Some(&u16::from(b'*'))
            {
                pattern_index += 1;
                value_index += 1;
            } else if self.units.get(pattern_index) == Some(&u16::from(b'*')) {
                star = Some(pattern_index);
                pattern_index += 1;
                retry_value = value_index;
            } else if let Some(star_index) = star {
                retry_value += 1;
                value_index = retry_value;
                pattern_index = star_index + 1;
            } else {
                return false;
            }
        }
        self.units[pattern_index..]
            .iter()
            .all(|unit| *unit == u16::from(b'*'))
    }
}

#[derive(Debug)]
struct ValidatedConfig {
    thresholds: Vec<f64>,
    include: Vec<Wildcard>,
    exclude: Vec<Wildcard>,
    arguments_preview_chars: usize,
}

fn validate(config: &RepeatToolReminderConfig) -> anyhow::Result<ValidatedConfig> {
    anyhow::ensure!(
        !config.thresholds.is_empty(),
        "repeat-tool-reminder: `thresholds` must not be empty"
    );
    let mut thresholds = Vec::with_capacity(config.thresholds.len());
    for value in &config.thresholds {
        anyhow::ensure!(
            value.is_finite() && value.fract() == 0.0 && *value >= 2.0,
            "repeat-tool-reminder: invalid threshold {} — every threshold must be an integer >= 2",
            render_number(*value)
        );
        thresholds.push(*value);
    }
    anyhow::ensure!(
        !thresholds
            .iter()
            .enumerate()
            .any(|(index, value)| thresholds[..index].contains(value)),
        "repeat-tool-reminder: `thresholds` must not contain duplicates"
    );
    thresholds.sort_by(f64::total_cmp);
    let preview = config.arguments_preview_chars;
    anyhow::ensure!(
        preview.is_finite() && preview.fract() == 0.0 && preview >= 1.0,
        "repeat-tool-reminder: invalid argumentsPreviewChars {} — must be an integer >= 1",
        render_number(preview)
    );
    Ok(ValidatedConfig {
        thresholds,
        include: config
            .include
            .iter()
            .map(|pattern| Wildcard::new(pattern))
            .collect(),
        exclude: config
            .exclude
            .iter()
            .map(|pattern| Wildcard::new(pattern))
            .collect(),
        arguments_preview_chars: integer_to_usize_saturating(preview),
    })
}

fn render_number(value: f64) -> String {
    ryu_js::Buffer::new().format(value).to_owned()
}

fn integer_to_usize_saturating(value: f64) -> usize {
    format!("{value:.0}").parse().unwrap_or(usize::MAX)
}

#[derive(Debug)]
struct Chain {
    agent: std::sync::Weak<Agent>,
    key: String,
    count: f64,
}

#[derive(Debug)]
struct RepeatGuard {
    config: ValidatedConfig,
    chains: Mutex<Vec<Chain>>,
}

impl RepeatGuard {
    fn tracked(&self, tool_name: &str) -> bool {
        (self.config.include.is_empty()
            || self
                .config
                .include
                .iter()
                .any(|pattern| pattern.matches(tool_name)))
            && !self
                .config
                .exclude
                .iter()
                .any(|pattern| pattern.matches(tool_name))
    }

    fn observe(&self, execution: &ToolExecution) -> Option<UserMessage> {
        let agent = execution.agent.as_ref()?;
        if !self.tracked(&execution.name) {
            return None;
        }
        let canonical = canonicalize(&execution.arguments);
        let key = serde_json::to_string(&[execution.name.as_str(), canonical.as_str()])
            .expect("two Rust strings always serialize");
        let mut chains = self.chains.lock();
        chains.retain(|chain| chain.agent.strong_count() > 0);
        let chain = chains.iter_mut().find(|chain| {
            chain
                .agent
                .upgrade()
                .is_some_and(|owner| Arc::ptr_eq(&owner, agent))
        });
        let count = if let Some(chain) = chain {
            if chain.key == key {
                chain.count += 1.0;
            } else {
                chain.key = key;
                chain.count = 1.0;
            }
            chain.count
        } else {
            chains.push(Chain {
                agent: Arc::downgrade(agent),
                key,
                count: 1.0,
            });
            1.0
        };
        if !self.config.thresholds.contains(&count) {
            return None;
        }
        let text = if count.to_bits() == self.config.thresholds[0].to_bits() {
            GENTLE_REMINDER.to_owned()
        } else {
            detailed_reminder(
                &execution.name,
                count,
                &preview_arguments(&canonical, self.config.arguments_preview_chars),
            )
        };
        Some(reminder(&execution.name, count, text))
    }

    fn reset(&self, agent: &Arc<Agent>) {
        self.chains.lock().retain(|chain| {
            chain
                .agent
                .upgrade()
                .is_some_and(|owner| !Arc::ptr_eq(&owner, agent))
        });
    }
}

fn reminder(tool_name: &str, count: f64, text: String) -> UserMessage {
    let mut source = MessageSource::plugin(NAME);
    source
        .fields
        .insert("form".to_owned(), Value::String("notice".to_owned()));
    source.fields.insert(
        "summary".to_owned(),
        Value::String(format!("{tool_name} × {}", render_number(count))),
    );
    UserMessage::new(vec![ContentBlock::Text { text }], source)
}

fn detailed_reminder(tool_name: &str, count: f64, arguments: &str) -> String {
    let count = render_number(count);
    format!(
        "Repeated tool call detected:\n- tool: {tool_name}\n- consecutive_calls: {count}\n- arguments: {arguments}\nThe repeated calls are not making progress. Do not call this tool with these exact arguments again. Inspect the latest result and choose a different action, different arguments, or finish the task if enough evidence has been gathered."
    )
}

fn preview_arguments(canonical: &str, cap: usize) -> String {
    let units = canonical.encode_utf16().collect::<Vec<_>>();
    if units.len() <= cap {
        return canonical.to_owned();
    }
    format!(
        "{}… (+{} more chars)",
        String::from_utf16_lossy(&units[..cap]),
        units.len() - cap
    )
}

fn fold_reminder(mut decision: PostToolDecision, reminder: UserMessage) -> PostToolDecision {
    let contexts = match &mut decision {
        PostToolDecision::Accept {
            additional_contexts,
            ..
        }
        | PostToolDecision::ReplaceValue {
            additional_contexts,
            ..
        }
        | PostToolDecision::Block {
            additional_contexts,
            ..
        } => additional_contexts,
    };
    contexts.insert(0, reminder);
    decision
}

/// Installs the guard's post-tool enrichment and user-interjection reset hooks.
///
/// # Errors
///
/// Returns fail-loud configuration or inactive-context listener failures.
pub fn apply(context: &Context, config: &RepeatToolReminderConfig) -> anyhow::Result<()> {
    let guard = Arc::new(RepeatGuard {
        config: validate(config)?,
        chains: Mutex::new(Vec::new()),
    });
    let post_guard = guard.clone();
    context.events().on_waterfall(
        context,
        "tools/post-execute",
        move |_, args, next| {
            let Some(execution) = args.get::<ToolExecution>(0) else {
                return Box::pin(async {
                    Err(anyhow::anyhow!(
                        "tools/post-execute is missing its execution"
                    ))
                });
            };
            let reminder = post_guard.observe(&execution);
            Box::pin(async move {
                let reply = next.run().await?;
                let decision = reply
                    .downcast::<PostToolDecision>()
                    .map(|decision| (*decision).clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!("tools/post-execute returned an invalid decision")
                    })?;
                Ok(EventReply::Value(Arc::new(
                    reminder.map_or(decision.clone(), |reminder| {
                        fold_reminder(decision, reminder)
                    }),
                )))
            })
        },
        EventOptions::default(),
    )?;

    context.events().on_waterfall(
        context,
        "agent/pre-step",
        move |_, args, next| {
            let guard = guard.clone();
            let event = args.get::<AgentEvent<AgentPreStepEvent>>(0);
            if let Some(event) = event
                && event
                    .payload
                    .messages
                    .iter()
                    .any(|message| message.source().kind == "user")
            {
                guard.reset(&event.agent);
            }
            Box::pin(async move { next.run().await })
        },
        EventOptions::default(),
    )?;
    Ok(())
}

/// Builds the loader-compatible guard plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: RepeatToolReminderConfig = serde_json::from_value(config)?;
            apply(&context, &config)
        })
    })
    .with_config_validator(|value| {
        let config: RepeatToolReminderConfig = serde_json::from_value(value.clone())?;
        validate(&config)?;
        Ok(serde_json::to_value(config)?)
    })
}

/// Registers the package's intentionally empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-repeat-tool-reminder", InvariantInstaller::noop())
}

fn canonicalize(value: &Value) -> String {
    let mut output = String::new();
    write_canonical(value, &mut output);
    output
}

fn write_canonical(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => output.push_str(&javascript_number(number)),
        Value::String(value) => output.push_str(
            &serde_json::to_string(value).expect("serializing a Rust string cannot fail"),
        ),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical(value, output);
            }
            output.push(']');
        }
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| javascript_key_cmp(left, right));
            output.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(
                    &serde_json::to_string(key).expect("serializing a Rust string cannot fail"),
                );
                output.push(':');
                write_canonical(value, output);
            }
            output.push('}');
        }
    }
}

fn javascript_number(number: &Number) -> String {
    ryu_js::Buffer::new()
        .format(number.as_f64().expect("JSON numbers are finite"))
        .to_owned()
}

fn javascript_key_cmp(left: &str, right: &str) -> Ordering {
    match (array_index(left), array_index(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.encode_utf16().cmp(right.encode_utf16()),
    }
}

fn array_index(value: &str) -> Option<u32> {
    if value == "0" {
        return Some(0);
    }
    if value.starts_with('0')
        || value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_json_sorts_deep_keys_and_emulates_javascript_key_order() {
        assert_eq!(
            canonicalize(&json!({
                "nested": { "y": null, "x": [1, 2] },
                "a": 1
            })),
            r#"{"a":1,"nested":{"x":[1,2],"y":null}}"#
        );
        assert_eq!(
            canonicalize(&json!({"10": true, "2": false, "a": 1})),
            r#"{"2":false,"10":true,"a":1}"#
        );
    }

    #[test]
    fn wildcard_only_treats_star_as_special() {
        assert!(Wildcard::new("pro*").matches("probe"));
        assert!(!Wildcard::new("pr.be").matches("probe"));
        assert!(Wildcard::new("pr.be").matches("pr.be"));
        assert!(Wildcard::new("*").matches(""));
    }
}
