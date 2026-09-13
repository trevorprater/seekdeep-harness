//! Bidirectional documentation-translation prompt renderer and response parser.

use std::collections::HashSet;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Placeholder names supported by the committed prompt.
pub const TRANSLATION_PROMPT_PLACEHOLDERS: &[&str] = &["source_lang", "target_lang", "terminology"];

const TEMPLATE_OPEN: &str = "## 模板正文\n\n````text\n";
const TEMPLATE_CLOSE: &str = "\n````";
const RESPONSE_SECTIONS: &[&str] = &["translation", "review", "final"];

/// Languages accepted by the bidirectional prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslationLanguage {
    /// English source translated into Chinese.
    English,
    /// Chinese source translated into English.
    Chinese,
}

impl TranslationLanguage {
    fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "Chinese",
        }
    }
}

/// Inputs varying for one rendered prompt.
#[derive(Clone, Debug)]
pub struct TranslationPromptInput {
    /// Source language.
    pub source_language: TranslationLanguage,
    /// Source basename including Markdown suffix.
    pub source_filename: String,
    /// Complete terminology document.
    pub terminology: String,
}

/// One reviewed whole-document example.
#[derive(Clone, Debug)]
pub struct TranslationExample {
    /// English side.
    pub english: String,
    /// Chinese side.
    pub chinese: String,
}

/// Inputs for one complete model request.
#[derive(Clone, Debug)]
pub struct TranslationRequestInput {
    /// Prompt direction and terminology.
    pub prompt: TranslationPromptInput,
    /// Whole source document.
    pub source_document: String,
    /// Reviewed examples.
    pub examples: Vec<TranslationExample>,
}

/// Provider-neutral model role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranslationRole {
    /// System prompt.
    System,
    /// User example/request.
    User,
    /// Assistant reviewed counterpart.
    Assistant,
}

/// One provider-neutral request message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationMessage {
    /// Message role.
    pub role: TranslationRole,
    /// Exact text.
    pub content: String,
}

/// Fully assembled request and target filename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationRequest {
    /// Target basename.
    pub target_filename: String,
    /// Calibrated message sequence.
    pub messages: Vec<TranslationMessage>,
}

/// Parsed three-section response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationResponse {
    /// First-pass translation.
    pub translation: String,
    /// Review notes.
    pub review: String,
    /// Reviewed final document.
    #[serde(rename = "final")]
    pub final_: String,
}

#[derive(Clone, Debug)]
struct TranslationFiles {
    target_filename: String,
    target_switcher: String,
}

/// Extracts placeholder names documented in the contract table.
///
/// # Errors
///
/// Returns a missing-template-body diagnostic.
pub fn documented_translation_prompt_placeholders(document: &str) -> anyhow::Result<Vec<String>> {
    static ROW: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?m)^\| `\{\{([a-z_]+)\}\}` \|").expect("static placeholder-row regex")
    });
    let Some(end) = document.find(TEMPLATE_OPEN) else {
        anyhow::bail!("translation prompt: missing template body");
    };
    Ok(ROW
        .captures_iter(&document[..end])
        .filter_map(|captures| captures.get(1).map(|capture| capture.as_str().to_owned()))
        .collect())
}

/// Renders one system prompt from the checked-in template.
///
/// # Errors
///
/// Returns filename, template, malformed, unknown, or missing-placeholder
/// diagnostics.
pub fn render_translation_prompt(
    document: &str,
    input: &TranslationPromptInput,
) -> anyhow::Result<String> {
    static PLACEHOLDER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"\{\{([a-z_]+)\}\}").expect("static prompt placeholder regex")
    });
    translation_files(input)?;
    let target_language = match input.source_language {
        TranslationLanguage::English => TranslationLanguage::Chinese,
        TranslationLanguage::Chinese => TranslationLanguage::English,
    };
    let template = extract_translation_prompt(document)?;
    let stripped = PLACEHOLDER.replace_all(template, "");
    if stripped.contains("{{") || stripped.contains("}}") {
        anyhow::bail!("translation prompt: template contains malformed placeholder syntax");
    }
    let names = PLACEHOLDER
        .captures_iter(template)
        .filter_map(|captures| captures.get(1).map(|capture| capture.as_str()))
        .collect::<Vec<_>>();
    let mut unknown = Vec::new();
    let mut seen = HashSet::new();
    for name in &names {
        if !TRANSLATION_PROMPT_PLACEHOLDERS.contains(name) && seen.insert(*name) {
            unknown.push(*name);
        }
    }
    if !unknown.is_empty() {
        anyhow::bail!(
            "translation prompt: unsupported placeholder(s): {}",
            unknown.join(", ")
        );
    }
    let missing = TRANSLATION_PROMPT_PLACEHOLDERS
        .iter()
        .filter(|required| !names.contains(required))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        anyhow::bail!(
            "translation prompt: template does not use required placeholder(s): {}",
            missing.join(", ")
        );
    }
    Ok(PLACEHOLDER
        .replace_all(template, |captures: &regex::Captures<'_>| {
            match &captures[1] {
                "source_lang" => input.source_language.label(),
                "target_lang" => target_language.label(),
                "terminology" => &input.terminology,
                _ => "",
            }
        })
        .into_owned())
}

/// Assembles the system prompt, reviewed examples, and source document.
///
/// # Errors
///
/// Returns prompt rendering and filename diagnostics.
pub fn render_translation_request(
    document: &str,
    input: &TranslationRequestInput,
) -> anyhow::Result<TranslationRequest> {
    let files = translation_files(&input.prompt)?;
    let mut messages = vec![TranslationMessage {
        role: TranslationRole::System,
        content: render_translation_prompt(document, &input.prompt)?,
    }];
    for example in &input.examples {
        let (source, target) = match input.prompt.source_language {
            TranslationLanguage::English => (&example.english, &example.chinese),
            TranslationLanguage::Chinese => (&example.chinese, &example.english),
        };
        messages.push(TranslationMessage {
            role: TranslationRole::User,
            content: source.clone(),
        });
        messages.push(TranslationMessage {
            role: TranslationRole::Assistant,
            content: target.clone(),
        });
    }
    messages.push(TranslationMessage {
        role: TranslationRole::User,
        content: input.source_document.clone(),
    });
    Ok(TranslationRequest {
        target_filename: files.target_filename,
        messages,
    })
}

/// Serializes a response in the escaped three-section format.
#[must_use]
pub fn render_translation_response(response: &TranslationResponse) -> String {
    [
        ("translation", &response.translation),
        ("review", &response.review),
        ("final", &response.final_),
    ]
    .into_iter()
    .map(|(section, value)| format!("<{section}>\n{}\n</{section}>", escape_response_body(value)))
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// Parses an ordered, exactly-once three-section response.
///
/// # Errors
///
/// Returns missing, duplicate, order, or outside-content diagnostics.
pub fn parse_translation_response(text: &str) -> anyhow::Result<TranslationResponse> {
    static FENCE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?s)^```(?:xml)?\n(.*?)\n```$").expect("static response fence regex")
    });
    let mut body = text.trim();
    let fenced_storage;
    if let Some(captured) = FENCE
        .captures(body)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().trim())
    {
        fenced_storage = captured.to_owned();
        body = &fenced_storage;
    }
    let lines = body.split('\n').collect::<Vec<_>>();
    let mut values = Vec::new();
    let mut previous_close_end = 0;
    for (index, section) in RESPONSE_SECTIONS.iter().enumerate() {
        let open = format!("<{section}>");
        let close = format!("</{section}>");
        let open_count = lines.iter().filter(|line| **line == open).count();
        let close_count = lines.iter().filter(|line| **line == close).count();
        if open_count == 0 || close_count == 0 {
            anyhow::bail!("translation response: missing or unterminated <{section}> section");
        }
        if open_count > 1 || close_count > 1 {
            anyhow::bail!("translation response: duplicate <{section}> section");
        }
        let open_start = line_delimiter_offset(body, &open).unwrap_or(usize::MAX);
        let close_start = line_delimiter_offset(body, &close).unwrap_or(usize::MAX);
        let separator = body.get(previous_close_end..open_start).unwrap_or_default();
        let separated = if index == 0 {
            separator.is_empty()
        } else {
            !separator.is_empty() && separator.bytes().all(|byte| byte == b'\n')
        };
        if close_start < open_start || !separated {
            anyhow::bail!(
                "translation response: sections must appear in translation, review, final order"
            );
        }
        let mut content_start = open_start + open.len();
        if body.as_bytes().get(content_start) == Some(&b'\n') {
            content_start += 1;
        }
        let mut content_end = close_start;
        if content_end > 0 && body.as_bytes().get(content_end - 1) == Some(&b'\n') {
            content_end -= 1;
        }
        values.push(unescape_response_body(
            body.get(content_start..content_end).unwrap_or_default(),
        ));
        previous_close_end = close_start + close.len();
    }
    if previous_close_end != body.len() {
        anyhow::bail!("translation response: content is not allowed outside response sections");
    }
    Ok(TranslationResponse {
        translation: values.remove(0),
        review: values.remove(0),
        final_: values.remove(0),
    })
}

/// Parses a response and corrects its final language switcher.
///
/// # Errors
///
/// Returns response, filename, frontmatter, or H1 diagnostics.
pub fn consume_translation_response(
    text: &str,
    input: &TranslationPromptInput,
) -> anyhow::Result<TranslationResponse> {
    let mut parsed = parse_translation_response(text)?;
    let files = translation_files(input)?;
    parsed.final_ = correct_language_switcher(&parsed.final_, &files.target_switcher)?;
    Ok(parsed)
}

fn translation_files(input: &TranslationPromptInput) -> anyhow::Result<TranslationFiles> {
    let basename = std::path::Path::new(&input.source_filename)
        .file_name()
        .and_then(std::ffi::OsStr::to_str);
    if basename != Some(input.source_filename.as_str()) {
        anyhow::bail!(
            "translation prompt: sourceFilename must be a basename; got {}",
            serde_json::to_string(&input.source_filename)?
        );
    }
    let chinese = input.source_filename.strip_suffix(".zh.md");
    let english = input.source_filename.strip_suffix(".md");
    let matches = match input.source_language {
        TranslationLanguage::Chinese => chinese.is_some(),
        TranslationLanguage::English => english.is_some() && chinese.is_none(),
    };
    if !matches {
        anyhow::bail!(
            "translation prompt: {} does not match source language {}",
            input.source_filename,
            input.source_language.label()
        );
    }
    if let Some(stem) = chinese {
        Ok(TranslationFiles {
            target_filename: format!("{stem}.md"),
            target_switcher: format!("English | [中文]({})", input.source_filename),
        })
    } else {
        let stem = english.unwrap_or_default();
        Ok(TranslationFiles {
            target_filename: format!("{stem}.zh.md"),
            target_switcher: format!("[English]({}) | 中文", input.source_filename),
        })
    }
}

fn extract_translation_prompt(document: &str) -> anyhow::Result<&str> {
    let Some(start) = document.find(TEMPLATE_OPEN) else {
        anyhow::bail!("translation prompt: missing `## 模板正文` text fence");
    };
    let content_start = start + TEMPLATE_OPEN.len();
    let Some(relative_end) = document[content_start..].find(TEMPLATE_CLOSE) else {
        anyhow::bail!("translation prompt: missing closing four-backtick fence");
    };
    Ok(&document[content_start..content_start + relative_end])
}

fn escape_response_body(value: &str) -> String {
    value
        .split('\n')
        .map(|line| {
            let delimiter = line.trim_start_matches('\\');
            if response_delimiter(delimiter) {
                format!("\\{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn unescape_response_body(value: &str) -> String {
    value
        .split('\n')
        .map(|line| {
            let Some(candidate) = line.strip_prefix('\\') else {
                return line.to_owned();
            };
            if response_delimiter(candidate.trim_start_matches('\\')) {
                candidate.to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn response_delimiter(value: &str) -> bool {
    RESPONSE_SECTIONS
        .iter()
        .any(|section| value == format!("<{section}>") || value == format!("</{section}>"))
}

fn line_delimiter_offset(body: &str, delimiter: &str) -> Option<usize> {
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let value = line.strip_suffix('\n').unwrap_or(line);
        if value == delimiter {
            return Some(offset);
        }
        offset += line.len();
    }
    if body.is_empty() {
        None
    } else if body.rsplit_once('\n').map_or(body, |(_, line)| line) == delimiter {
        Some(body.len() - delimiter.len())
    } else {
        None
    }
}

fn correct_language_switcher(markdown: &str, switcher: &str) -> anyhow::Result<String> {
    static SWITCHER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^(?:English \| \[中文\]\(.+\)|\[English\]\(.+\) \| 中文)$")
            .expect("static switcher regex")
    });
    static H1: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"^#\s+\S").expect("static H1 regex"));
    let normalized = markdown.replace("\r\n", "\n");
    let mut lines = normalized
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut heading = 0;
    if lines.first().is_some_and(|line| line == "---") {
        let Some(end) = lines.iter().skip(1).position(|line| line == "---") else {
            anyhow::bail!("translation response: final document has unterminated YAML frontmatter");
        };
        heading = end + 2;
        while lines.get(heading).is_some_and(String::is_empty) {
            heading += 1;
        }
    }
    if !lines.get(heading).is_some_and(|line| H1.is_match(line)) {
        anyhow::bail!("translation response: final document must start with an H1 heading");
    }
    let mut content = heading + 1;
    while lines.get(content).is_some_and(String::is_empty) {
        content += 1;
    }
    if lines
        .get(content)
        .is_some_and(|line| SWITCHER.is_match(line))
    {
        content += 1;
    }
    while lines.get(content).is_some_and(String::is_empty) {
        content += 1;
    }
    let mut output = lines[..heading].to_vec();
    output.push(lines[heading].clone());
    output.push(String::new());
    output.push(switcher.to_owned());
    if content < lines.len() {
        output.push(String::new());
        output.extend_from_slice(&lines[content..]);
    }
    Ok(format!("{}\n", output.join("\n")))
}
