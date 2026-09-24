//! Model-facing filesystem tool suite composition and plugin entrypoint.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_attachment::ATTACHMENTS;
use seekdeep_cordis::{Context, Fiber, Plugin, fiber::EffectHandle};
use seekdeep_lossless_json::JsonNumber;
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
    pub read_limit: Option<JsonNumber>,
    /// Maximum characters returned for a single line before truncation.
    pub read_max_line_length: Option<JsonNumber>,
    /// Maximum bytes returned for the selected lines of one read call.
    pub read_max_bytes: Option<JsonNumber>,
    /// Files at or above this size stream instead of loading whole into memory.
    pub read_stream_min_size: Option<JsonNumber>,
}

impl Config {
    /// Applies every deployment default and validates the resulting caps.
    ///
    /// # Errors
    ///
    /// Returns a non-positive cap failure.
    pub fn resolved(&self) -> anyhow::Result<ReadToolCaps> {
        let read_limit = self.read_limit.unwrap_or(JsonNumber::from(READ_LIMIT));
        let read_max_line_length = self
            .read_max_line_length
            .unwrap_or(JsonNumber::from(READ_MAX_LINE_LENGTH));
        let read_max_bytes = self
            .read_max_bytes
            .unwrap_or(JsonNumber::from(READ_MAX_BYTES));
        let read_stream_min_size = self
            .read_stream_min_size
            .unwrap_or(JsonNumber::from(STREAM_MIN_SIZE));
        assert_positive_integer("readLimit", read_limit)?;
        assert_positive_integer("readMaxLineLength", read_max_line_length)?;
        assert_positive_integer("readMaxBytes", read_max_bytes)?;
        assert_positive_integer("readStreamMinSize", read_stream_min_size)?;
        Ok(ReadToolCaps {
            limit: read_limit,
            max_line_length: read_max_line_length,
            max_bytes: read_max_bytes,
            stream_min_size: read_stream_min_size,
        })
    }
}

/// Every read cap counts lines, chars, or bytes: a positive integer, or windowing
/// arithmetic misbehaves silently. The source admits any integer the JavaScript
/// number range holds, so the cap keeps that number instead of narrowing to `u64`.
fn assert_positive_integer(name: &str, value: JsonNumber) -> anyhow::Result<()> {
    if !value.is_positive_integer() {
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
    install_optional_read_image(ctx)?;
    // One escalation API shared by both mutating tools.
    let sandbox = Arc::new(FsSandboxController::new(ctx)?);
    apply_write_tool(ctx, &sandbox)?;
    apply_edit_tool(ctx, &sandbox)?;
    Ok(())
}

#[derive(Default)]
struct ImageBinding {
    provider: Option<usize>,
    fiber: Option<Arc<Fiber>>,
}

fn install_optional_read_image(context: &Context) -> anyhow::Result<()> {
    let binding = Arc::new(Mutex::new(ImageBinding::default()));
    reconcile_read_image(context, &binding)?;
    let watched_context = context.clone();
    let watched_binding = binding.clone();
    context.on_service_change_checked(move |name| {
        if name == ATTACHMENTS.name() {
            reconcile_read_image(&watched_context, &watched_binding)?;
        }
        Ok(())
    })?;
    context.own(EffectHandle::new(
        "tool-fs optional read_image",
        move || {
            Box::pin(async move {
                let fiber = binding.lock().fiber.take();
                if let Some(fiber) = fiber {
                    fiber.dispose().await?;
                }
                Ok(())
            })
        },
    ))?;
    Ok(())
}

fn reconcile_read_image(
    context: &Context,
    binding: &Arc<Mutex<ImageBinding>>,
) -> anyhow::Result<()> {
    let attachments = context.get_relaxed(ATTACHMENTS);
    let provider = attachments
        .as_ref()
        .map(|service| Arc::as_ptr(service).cast::<()>() as usize);
    let mut binding = binding.lock();
    if binding.provider == provider {
        return Ok(());
    }
    if let Some(fiber) = binding.fiber.take() {
        futures::executor::block_on(fiber.dispose())?;
    }
    binding.provider = None;
    if attachments.is_some() {
        let fiber = Fiber::active_child("tool-fs read_image");
        let child = context.with_fiber(fiber.clone());
        if let Err(error) = apply_read_image_tool(&child) {
            futures::executor::block_on(fiber.dispose()).ok();
            return Err(error);
        }
        binding.provider = provider;
        binding.fiber = Some(fiber);
    }
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
        assert_eq!(caps.limit, JsonNumber::from(READ_LIMIT));
        assert_eq!(caps.max_line_length, JsonNumber::from(READ_MAX_LINE_LENGTH));
        assert_eq!(caps.max_bytes, JsonNumber::from(READ_MAX_BYTES));
        assert_eq!(caps.stream_min_size, JsonNumber::from(STREAM_MIN_SIZE));

        for invalid in [0.0, 2.5, f64::NAN, f64::INFINITY] {
            let config = Config {
                read_limit: Some(JsonNumber::new(invalid)),
                ..Config::default()
            };
            assert_eq!(
                config.resolved().expect_err("rejected cap").to_string(),
                "tool-fs: readLimit must be a positive integer"
            );
        }
        // The source accepts any finite positive integer, including ones past u64.
        let huge = Config {
            read_limit: Some(JsonNumber::new(1e300)),
            read_max_bytes: Some(JsonNumber::new(18_446_744_073_709_551_616.0)),
            ..Config::default()
        };
        let caps = huge.resolved().expect("huge caps are positive integers");
        assert_eq!(caps.limit, JsonNumber::new(1e300));
        assert_eq!(caps.max_bytes.saturating_usize(), usize::MAX);
    }

    #[test]
    fn config_deserializes_camel_case_and_rejects_unknown_fields() {
        let parsed: Config = serde_json::from_value(serde_json::json!({
            "readLimit": 500,
            "readMaxLineLength": 100,
        }))
        .expect("camelCase fields");
        assert_eq!(parsed.read_limit, Some(JsonNumber::new(500.0)));
        assert_eq!(parsed.read_max_line_length, Some(JsonNumber::new(100.0)));
        let fractional: Config =
            serde_json::from_value(serde_json::json!({"readMaxBytes": 1.5})).expect("a number");
        assert_eq!(fractional.read_max_bytes, Some(JsonNumber::new(1.5)));
        assert!(serde_json::from_value::<Config>(serde_json::json!({"bogus": 1})).is_err());
    }
}
