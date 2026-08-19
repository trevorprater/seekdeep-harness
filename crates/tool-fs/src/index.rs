//! Model-facing filesystem tool suite composition and plugin entrypoint.

use std::sync::Arc;

use seekdeep_attachment::ATTACHMENTS;
use seekdeep_cordis::{Context, Plugin};
use serde::{Deserialize, Serialize};

use crate::edit::apply_edit_tool;
use crate::read::{READ_LIMIT, ReadToolCaps, STREAM_MIN_SIZE, apply_read_tool};
use crate::read_image::apply_read_image_tool;
use crate::read_render::{READ_MAX_BYTES, READ_MAX_LINE_LENGTH};
use crate::sandbox::FsSandboxController;
use crate::write::apply_write_tool;

/// Cordis plugin name used by loader diagnostics.
pub const NAME: &str = "tool-fs";

/// Services required by the filesystem tool suite.
pub const INJECT: &[&str] = &["tools", "fs", "systemPrompt"];

/// Plugin config (all optional; [`Config::resolved`] supplies the defaults).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Default and maximum number of lines returned by one read call.
    pub read_limit: Option<u64>,
    /// Maximum characters returned for a single line before truncation.
    pub read_max_line_length: Option<u64>,
    /// Maximum bytes returned for the selected lines of one read call.
    pub read_max_bytes: Option<u64>,
    /// Files at or above this size stream instead of loading whole into memory.
    pub read_stream_min_size: Option<u64>,
}

impl Config {
    /// Applies every deployment default and validates the resulting caps.
    ///
    /// # Errors
    ///
    /// Returns a non-positive cap failure.
    pub fn resolved(&self) -> anyhow::Result<ReadToolCaps> {
        let read_limit = self.read_limit.unwrap_or(READ_LIMIT);
        let read_max_line_length = self
            .read_max_line_length
            .unwrap_or(u64::try_from(READ_MAX_LINE_LENGTH).unwrap_or(u64::MAX));
        let read_max_bytes = self
            .read_max_bytes
            .unwrap_or(u64::try_from(READ_MAX_BYTES).unwrap_or(u64::MAX));
        let read_stream_min_size = self.read_stream_min_size.unwrap_or(STREAM_MIN_SIZE);
        assert_positive_integer("readLimit", read_limit)?;
        assert_positive_integer("readMaxLineLength", read_max_line_length)?;
        assert_positive_integer("readMaxBytes", read_max_bytes)?;
        assert_positive_integer("readStreamMinSize", read_stream_min_size)?;
        Ok(ReadToolCaps {
            limit: read_limit,
            max_line_length: usize::try_from(read_max_line_length).unwrap_or(usize::MAX),
            max_bytes: usize::try_from(read_max_bytes).unwrap_or(usize::MAX),
            stream_min_size: read_stream_min_size,
        })
    }
}

/// Every read cap is a positive integer, or windowing arithmetic misbehaves silently.
fn assert_positive_integer(name: &str, value: u64) -> anyhow::Result<()> {
    if value == 0 {
        anyhow::bail!("tool-fs: {name} must be a positive integer");
    }
    Ok(())
}

/// Registers the full `read`/`write`/`edit` suite, plus `read_image` while `attachments` is mounted.
///
/// # Errors
///
/// Returns config-validation, prompt-registration, or tool-registration failures.
pub fn apply(ctx: &Context, config: &Config) -> anyhow::Result<()> {
    let caps = config.resolved()?;
    apply_read_tool(ctx, &caps)?;
    // read_image is composition-conditional: without a mounted attachment
    // store the deployment cannot durably commit image bytes, so the tool never
    // registers. The execute body keeps a defensive re-check for direct callers.
    if ctx.get(ATTACHMENTS).is_some() {
        apply_read_image_tool(ctx)?;
    }
    // One escalation API shared by both mutating tools.
    let sandbox = Arc::new(FsSandboxController::new(ctx)?);
    apply_write_tool(ctx, &sandbox)?;
    apply_edit_tool(ctx, &sandbox)?;
    Ok(())
}

/// Builds the loader-compatible tool suite plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply(&context, &config)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_defaults_and_rejects_zero() {
        let caps = Config::default().resolved().expect("defaults are valid");
        assert_eq!(caps.limit, READ_LIMIT);
        assert_eq!(caps.max_line_length, READ_MAX_LINE_LENGTH);
        assert_eq!(caps.max_bytes, READ_MAX_BYTES);
        assert_eq!(caps.stream_min_size, STREAM_MIN_SIZE);

        let zero = Config {
            read_limit: Some(0),
            ..Config::default()
        };
        assert!(zero.resolved().is_err());
    }

    #[test]
    fn config_deserializes_camel_case_and_rejects_unknown_fields() {
        let parsed: Config = serde_json::from_value(serde_json::json!({
            "readLimit": 500,
            "readMaxLineLength": 100,
        }))
        .expect("camelCase fields");
        assert_eq!(parsed.read_limit, Some(500));
        assert_eq!(parsed.read_max_line_length, Some(100));
        assert!(serde_json::from_value::<Config>(serde_json::json!({"bogus": 1})).is_err());
    }
}
