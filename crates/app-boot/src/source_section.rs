//! Harness implementation-location prompt section.

use std::path::Path;

use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_system_prompt::{PromptSection, SYSTEM_PROMPT};

/// Prompt-section name for the Harness implementation location.
pub const HARNESS_SOURCE_SECTION: &str = "harness:source";

/// Adds the global Harness source section when a prompt service is mounted.
///
/// # Errors
///
/// Returns duplicate-section, invalid-owner, or inactive-context failures.
pub fn add_harness_source_section(
    context: &Context,
    source_root: &Path,
) -> anyhow::Result<Option<EffectHandle>> {
    let Some(prompt) = context.get(SYSTEM_PROMPT) else {
        return Ok(None);
    };
    let text = format!(
        "The SeekDeep Harness implementation checkout is at {}. The checkout location and current working directory are separate values and may differ; never infer the working directory from this path. Use pwd to determine the current working directory. Use this checkout only to inspect or extend SeekDeep itself.",
        source_root.display()
    );
    Ok(Some(prompt.section(
        context,
        PromptSection::new(HARNESS_SOURCE_SECTION, -99.0, text),
    )?))
}
