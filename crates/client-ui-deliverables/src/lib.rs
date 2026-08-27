//! Produced-file prompt, projection, and browser UI semantics.

mod locales;
mod produced;
mod projection;

pub use locales::*;
pub use produced::*;
pub use projection::*;

/// Stable plugin identity.
pub const NAME: &str = "client-ui-deliverables";
/// Host service prerequisites.
pub const INJECT: &[&str] = &["systemPrompt"];
/// Ordered system-prompt section identity.
pub const PROMPT_SECTION_NAME: &str = "ui:deliverable-file-references";
/// Ordered system-prompt section position.
pub const PROMPT_SECTION_ORDER: f64 = 190.0;
/// Stable final-response guidance paired with the browser file renderer.
pub const FILE_REFERENCE_PROMPT: &str = "When you successfully create or modify files, mention the primary outputs in your final response. To make those and any other changed-file references clickable in Web, format them as Markdown inline code using the exact file-tool path, or a basename when unique among the files changed in that turn.";

/// Registers the ordered file-reference model guidance.
///
/// # Errors
///
/// Returns when `systemPrompt` is absent or the scoped section cannot be registered.
#[cfg(not(target_arch = "wasm32"))]
pub fn apply_host(context: &seekdeep_cordis::Context) -> anyhow::Result<()> {
    let prompt = context
        .get(seekdeep_system_prompt::SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("client-ui-deliverables requires systemPrompt"))?;
    prompt.section(
        context,
        seekdeep_system_prompt::PromptSection::new(
            PROMPT_SECTION_NAME,
            PROMPT_SECTION_ORDER,
            FILE_REFERENCE_PROMPT,
        ),
    )?;
    Ok(())
}

/// Builds the Host plugin that owns the prompt section lifecycle.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move { apply_host(&context) })
    })
}
