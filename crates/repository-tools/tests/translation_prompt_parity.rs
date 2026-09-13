//! Direction, placeholders, request assembly, response grammar, and switcher fixtures.

use seekdeep_repository_tools::translation_prompt::{
    TRANSLATION_PROMPT_PLACEHOLDERS, TranslationExample, TranslationLanguage,
    TranslationPromptInput, TranslationRequestInput, TranslationResponse, TranslationRole,
    consume_translation_response, documented_translation_prompt_placeholders,
    parse_translation_response, render_translation_prompt, render_translation_request,
    render_translation_response,
};

fn document() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join("docs/i18n/translation-prompt.md")).unwrap()
}

fn input(language: TranslationLanguage, filename: &str) -> TranslationPromptInput {
    TranslationPromptInput {
        source_language: language,
        source_filename: filename.to_owned(),
        terminology: "term = 术语\n".to_owned(),
    }
}

#[test]
fn committed_placeholder_table_and_both_directions_render_exactly() {
    let document = document();
    assert_eq!(
        documented_translation_prompt_placeholders(&document).unwrap(),
        TRANSLATION_PROMPT_PLACEHOLDERS
    );
    let english =
        render_translation_prompt(&document, &input(TranslationLanguage::English, "source.md"))
            .unwrap();
    let chinese = render_translation_prompt(
        &document,
        &input(TranslationLanguage::Chinese, "source.zh.md"),
    )
    .unwrap();
    assert!(english.contains("from English to Chinese"));
    assert!(chinese.contains("from Chinese to English"));
    assert!(!english.contains("{{"));
    assert!(!chinese.contains("{{"));
}

#[test]
fn unknown_missing_and_malformed_placeholders_are_rejected() {
    let document = document();
    let base = input(TranslationLanguage::English, "source.md");
    assert!(
        render_translation_prompt(&document.replace("{{terminology}}", "{{unknown}}"), &base)
            .unwrap_err()
            .to_string()
            .contains("unsupported placeholder")
    );
    assert!(
        render_translation_prompt(&document.replace("{{terminology}}", "term"), &base)
            .unwrap_err()
            .to_string()
            .contains("does not use required")
    );
    assert!(
        render_translation_prompt(
            &document.replace("{{terminology}}", "{{terminology}"),
            &base
        )
        .unwrap_err()
        .to_string()
        .contains("malformed placeholder")
    );
}

#[test]
fn reviewed_examples_precede_the_real_source_in_both_directions() {
    let request = render_translation_request(
        &document(),
        &TranslationRequestInput {
            prompt: input(TranslationLanguage::English, "source.md"),
            source_document: "real source".to_owned(),
            examples: vec![TranslationExample {
                english: "example English".to_owned(),
                chinese: "示例中文".to_owned(),
            }],
        },
    )
    .unwrap();
    assert_eq!(request.target_filename, "source.zh.md");
    assert_eq!(
        request
            .messages
            .iter()
            .map(|message| message.role)
            .collect::<Vec<_>>(),
        [
            TranslationRole::System,
            TranslationRole::User,
            TranslationRole::Assistant,
            TranslationRole::User,
        ]
    );
    assert_eq!(request.messages[1].content, "example English");
    assert_eq!(request.messages[2].content, "示例中文");
    assert_eq!(request.messages[3].content, "real source");
}

#[test]
fn response_round_trips_markdown_and_delimiter_lines() {
    let response = TranslationResponse {
        translation: "first\n<review>\n\\<final>".to_owned(),
        review: "- no correction".to_owned(),
        final_: "# Final\n\ntext".to_owned(),
    };
    assert_eq!(
        parse_translation_response(&render_translation_response(&response)).unwrap(),
        response
    );
}

#[test]
fn fenced_xml_wrapper_and_inline_close_tags_are_tolerated() {
    let body = "<translation>\ninline </translation> prose\n</translation>\n\n<review>\nok\n</review>\n\n<final>\n# Final\n</final>";
    let parsed = parse_translation_response(&format!("```xml\n{body}\n```")).unwrap();
    assert_eq!(parsed.translation, "inline </translation> prose");
}

#[test]
fn missing_duplicate_order_and_outside_content_are_rejected() {
    for body in [
        "<translation>x</translation>",
        "<translation>\nx\n</translation>\n<translation>\ny\n</translation>\n<review>\nr\n</review>\n<final>\nf\n</final>",
        "<review>\nr\n</review>\n<translation>\nx\n</translation>\n<final>\nf\n</final>",
        "<translation>\nx\n</translation>\n<review>\nr\n</review>\n<final>\nf\n</final>\nextra",
    ] {
        assert!(parse_translation_response(body).is_err(), "{body}");
    }
}

#[test]
fn consuming_response_inserts_target_switcher_after_h1() {
    let response = TranslationResponse {
        translation: "draft".to_owned(),
        review: "review".to_owned(),
        final_: "# 标题\n\n正文".to_owned(),
    };
    let consumed = consume_translation_response(
        &render_translation_response(&response),
        &input(TranslationLanguage::English, "source.md"),
    )
    .unwrap();
    assert_eq!(
        consumed.final_,
        "# 标题\n\n[English](source.md) | 中文\n\n正文\n"
    );
}

#[test]
fn frontmatter_is_preserved_before_corrected_switcher() {
    let response = TranslationResponse {
        translation: "draft".to_owned(),
        review: "review".to_owned(),
        final_: "---\nlayout: doc\n---\n\n# Title\n\n[English](wrong.md) | 中文\n\nBody".to_owned(),
    };
    let consumed = consume_translation_response(
        &render_translation_response(&response),
        &input(TranslationLanguage::Chinese, "source.zh.md"),
    )
    .unwrap();
    assert!(
        consumed
            .final_
            .starts_with("---\nlayout: doc\n---\n\n# Title\n\nEnglish | [中文](source.zh.md)\n\n")
    );
}

#[test]
fn filename_direction_and_final_document_shape_fail_loud() {
    assert!(
        render_translation_prompt(
            &document(),
            &input(TranslationLanguage::English, "source.zh.md"),
        )
        .is_err()
    );
    assert!(
        render_translation_prompt(
            &document(),
            &input(TranslationLanguage::English, "dir/source.md"),
        )
        .is_err()
    );
    let response = TranslationResponse {
        translation: "draft".to_owned(),
        review: "review".to_owned(),
        final_: "---\nunterminated\n# Title".to_owned(),
    };
    assert!(
        consume_translation_response(
            &render_translation_response(&response),
            &input(TranslationLanguage::English, "source.md"),
        )
        .is_err()
    );
}
