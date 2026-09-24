//! Harness-source prompt-section ordering and lifecycle parity.

use seekdeep_app_boot::{HARNESS_SOURCE_SECTION, add_harness_source_section};
use seekdeep_cordis::Context;
use seekdeep_system_prompt::{AssembleContext, SystemPromptConfig, render_prompt};

#[tokio::test]
async fn source_section_sits_between_identity_and_persona_and_is_reversible() -> anyhow::Result<()>
{
    let context = Context::new();
    let prompt = seekdeep_system_prompt::install(
        &context,
        SystemPromptConfig {
            persona: "You are a coding agent.".to_owned(),
            ..SystemPromptConfig::default()
        },
    )?;
    let source_root = std::path::Path::new("/opt/harness-src");
    let effect = add_harness_source_section(&context, source_root)?.expect("section");
    let rendered = render_prompt(&prompt.assemble(AssembleContext::default()).await?)?;
    let identity = rendered
        .find("You are an AI agent powered by SeekDeep Harness.")
        .expect("identity");
    let source = rendered
        .find("The SeekDeep Harness implementation checkout is at /opt/harness-src.")
        .expect("source");
    let persona = rendered.find("You are a coding agent.").expect("persona");
    assert!(identity < source && source < persona);
    assert!(
        prompt
            .assemble(AssembleContext::default())
            .await?
            .sections
            .iter()
            .any(|section| section.name == HARNESS_SOURCE_SECTION)
    );
    effect.dispose().await?;
    assert!(
        !prompt
            .assemble(AssembleContext::default())
            .await?
            .sections
            .iter()
            .any(|section| section.name == HARNESS_SOURCE_SECTION)
    );
    context.fiber().dispose().await
}

#[test]
fn source_section_is_a_noop_without_system_prompt() {
    let context = Context::new();
    assert!(
        add_harness_source_section(&context, std::path::Path::new("/opt/harness-src"))
            .unwrap()
            .is_none()
    );
}
