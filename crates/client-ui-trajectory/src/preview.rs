//! Bounded Markdown-to-text projection shared by trajectory consumers.

use seekdeep_client_ui_primitives::{MarkdownPlainTextMode, extract_markdown_plain_text};

const PREVIEW_SOURCE_CHARACTERS: usize = 2_048;
const PREVIEW_OUTPUT_CHARACTERS: usize = 512;

/// Builds a bounded one-line preview without parsing the complete Markdown document.
///
/// # Errors
///
/// Returns the shared GFM parser's diagnostic.
pub fn trajectory_preview_text(text: &str) -> Result<String, String> {
    let (source, source_truncated) = utf16_prefix(text, PREVIEW_SOURCE_CHARACTERS);
    let plain = extract_markdown_plain_text(&source, MarkdownPlainTextMode::All)?;
    let compact = collapse_whitespace(&plain);
    let (preview, preview_truncated) = utf16_prefix(&compact, PREVIEW_OUTPUT_CHARACTERS);
    let preview = preview.trim_end();
    Ok(if source_truncated || preview_truncated {
        format!("{preview}…")
    } else {
        preview.to_owned()
    })
}

fn utf16_prefix(text: &str, limit: usize) -> (String, bool) {
    let units = text.encode_utf16().collect::<Vec<_>>();
    let truncated = units.len() > limit;
    (
        String::from_utf16_lossy(&units[..units.len().min(limit)]),
        truncated,
    )
}

fn collapse_whitespace(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
        } else {
            if pending_space {
                output.push(' ');
                pending_space = false;
            }
            output.push(character);
        }
    }
    output
}
