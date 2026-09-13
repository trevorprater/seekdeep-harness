//! Runnable verification and snapshot assembly for the committed translation prompt.

use std::path::Path;

use regex::Regex;

use crate::translation_prompt::{
    TRANSLATION_PROMPT_PLACEHOLDERS, TranslationExample, TranslationLanguage,
    TranslationPromptInput, TranslationRequestInput, TranslationResponse, TranslationRole,
    consume_translation_response, documented_translation_prompt_placeholders,
    parse_translation_response, render_translation_prompt, render_translation_request,
    render_translation_response,
};

struct VerifierInputs {
    document: String,
    terminology: String,
    examples: Vec<TranslationExample>,
    source_document: String,
    recorded_response: String,
}

/// Renders and validates both prompt directions, examples, and recorded response.
///
/// # Errors
///
/// Returns file, placeholder, prompt, response, request, or snapshot JSON
/// diagnostics.
pub fn verify_translation_prompt(root: &Path, snapshot: bool) -> anyhow::Result<String> {
    static RESPONSE_EXAMPLE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?s)```xml\n(.*?)\n```").expect("static response-example regex")
    });
    let inputs = load_inputs(root)?;
    let VerifierInputs {
        document,
        terminology,
        examples,
        source_document,
        recorded_response,
    } = inputs;
    if documented_translation_prompt_placeholders(&document)? != TRANSLATION_PROMPT_PLACEHOLDERS {
        anyhow::bail!(
            "placeholder table must list exactly: {}",
            TRANSLATION_PROMPT_PLACEHOLDERS.join(", ")
        );
    }

    let english = TranslationPromptInput {
        source_language: TranslationLanguage::English,
        source_filename: "snapshot-note.md".to_owned(),
        terminology: terminology.clone(),
    };
    let chinese = TranslationPromptInput {
        source_language: TranslationLanguage::Chinese,
        source_filename: "snapshot-note.zh.md".to_owned(),
        terminology,
    };
    let english_source = render_translation_prompt(&document, &english)?;
    let chinese_source = render_translation_prompt(&document, &chinese)?;
    if english_source.contains("{{") || chinese_source.contains("{{") {
        anyhow::bail!("rendered prompt contains an unresolved placeholder");
    }
    if !english_source.contains("from English to Chinese") {
        anyhow::bail!("English-source render does not translate into Chinese");
    }
    if !chinese_source.contains("from Chinese to English") {
        anyhow::bail!("Chinese-source render does not translate into English");
    }
    let example = RESPONSE_EXAMPLE
        .captures(&english_source)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str())
        .ok_or_else(|| anyhow::anyhow!("rendered prompt has no three-section response example"))?;
    parse_translation_response(example)?;

    let round_trip = TranslationResponse {
        translation: "first pass\n\nwith **markdown**".to_owned(),
        review: "- 无修正".to_owned(),
        final_: "final text".to_owned(),
    };
    if parse_translation_response(&render_translation_response(&round_trip))? != round_trip {
        anyhow::bail!("three-section response does not round-trip");
    }
    let request = render_translation_request(
        &document,
        &TranslationRequestInput {
            prompt: english.clone(),
            source_document,
            examples,
        },
    )?;
    if request.target_filename != "snapshot-note.zh.md" {
        anyhow::bail!("English request resolves the wrong target filename");
    }
    let mut expected_roles = vec![TranslationRole::System];
    for _ in 0..5 {
        expected_roles.extend([TranslationRole::User, TranslationRole::Assistant]);
    }
    expected_roles.push(TranslationRole::User);
    if request
        .messages
        .iter()
        .map(|message| message.role)
        .ne(expected_roles)
    {
        anyhow::bail!("reviewed examples are not assembled as system, example pairs, then source");
    }
    let consumed = consume_translation_response(&recorded_response, &english)?;
    let expected_prefix =
        "---\nlayout: doc\n---\n\n# 快照说明\n\n[English](snapshot-note.md) | 中文\n\n";
    if !consumed.final_.starts_with(expected_prefix) {
        anyhow::bail!(
            "recorded frontmatter response does not preserve metadata and receive the canonical target switcher"
        );
    }
    if snapshot {
        Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "request": request,
                "response": consumed,
            }))?
        ))
    } else {
        Ok("verify-translation-prompt: both directions render, reviewed examples assemble, and the consumed response is target-path correct.\n".to_owned())
    }
}

fn load_inputs(root: &Path) -> std::io::Result<VerifierInputs> {
    let read = |path: &str| std::fs::read_to_string(root.join(path));
    let examples = [
        ("README.md", "README.zh.md"),
        ("docs/development.md", "docs/development.zh.md"),
        ("docs/i18n/README.md", "docs/i18n/README.zh.md"),
        (
            "docs/i18n/translation-rules.md",
            "docs/i18n/translation-rules.zh.md",
        ),
        (
            ".agents/notes/implemented/process/2026-07-02-bilingual-docs-and-pairing-gate.md",
            ".agents/notes/implemented/process/2026-07-02-bilingual-docs-and-pairing-gate.zh.md",
        ),
    ]
    .into_iter()
    .map(|(english, chinese)| {
        Ok(TranslationExample {
            english: read(english)?,
            chinese: read(chinese)?,
        })
    })
    .collect::<std::io::Result<Vec<_>>>()?;
    Ok(VerifierInputs {
        document: read("docs/i18n/translation-prompt.md")?,
        terminology: read("docs/i18n/terminology.md")?,
        examples,
        source_document: read("scripts/fixtures/translation-prompt/snapshot-note.md")?,
        recorded_response: read("scripts/fixtures/translation-prompt/response.txt")?,
    })
}
