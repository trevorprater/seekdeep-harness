//! Bounded Markdown-to-text projection shared by trajectory consumers.

use std::collections::{BTreeMap, BTreeSet};

use seekdeep_client_ui_primitives::{MarkdownPlainTextMode, extract_markdown_plain_text};
use seekdeep_lossless_json::JsonString;

use crate::text_value::is_space;

const PREVIEW_SOURCE_CHARACTERS: usize = 2_048;
const PREVIEW_OUTPUT_CHARACTERS: usize = 512;

/// Builds a bounded one-line preview while preserving exact UTF-16 code units.
///
/// # Errors
///
/// Returns the shared GFM parser's diagnostic.
pub fn trajectory_preview_text(text: impl Into<JsonString>) -> Result<JsonString, String> {
    let text = text.into();
    let (source, source_truncated) = utf16_prefix(&text, PREVIEW_SOURCE_CHARACTERS);
    let plain = markdown_plain_text(&source)?;
    let compact = collapse_whitespace(&plain);
    let (preview, preview_truncated) = utf16_prefix(&compact, PREVIEW_OUTPUT_CHARACTERS);
    let end = preview
        .utf16_units()
        .iter()
        .rposition(|unit| !is_space(*unit))
        .map_or(0, |index| index + 1);
    let mut preview = JsonString::from_utf16(&preview.utf16_units()[..end]);
    if source_truncated || preview_truncated {
        preview.push_str("…");
    }
    Ok(preview)
}

fn utf16_prefix(text: &JsonString, limit: usize) -> (JsonString, bool) {
    let units = text.utf16_units();
    (
        JsonString::from_utf16(&units[..units.len().min(limit)]),
        units.len() > limit,
    )
}

fn collapse_whitespace(text: &JsonString) -> JsonString {
    let mut output = Vec::with_capacity(text.len_utf16());
    let mut pending_space = false;
    for unit in text.utf16_units() {
        if is_space(*unit) {
            pending_space = !output.is_empty();
        } else {
            if pending_space {
                output.push(0x20);
                pending_space = false;
            }
            output.push(*unit);
        }
    }
    JsonString::from_utf16(&output)
}

fn markdown_plain_text(text: &JsonString) -> Result<JsonString, String> {
    if let Some(text) = text.as_str() {
        return extract_markdown_plain_text(text, MarkdownPlainTextMode::All).map(Into::into);
    }
    // Private-use characters have the same non-syntax role as lone surrogates.
    // Excluding literal and entity-produced values makes restoration unambiguous.
    let mut used = text.code_points().collect::<BTreeSet<_>>();
    reserve_numeric_entities(text.utf16_units(), &mut used);
    let mut candidates = (0xe000..=0xf8ff)
        .chain(0xf0000..=0x000f_fffd)
        .chain(0x0010_0000..=0x0010_fffd);
    let mut encoded = String::new();
    let mut replacements = BTreeMap::<u16, char>::new();
    for point in char::decode_utf16(text.utf16_units().iter().copied()) {
        match point {
            Ok(character) => encoded.push(character),
            Err(surrogate) => {
                let surrogate = surrogate.unpaired_surrogate();
                let replacement = replacements.entry(surrogate).or_insert_with(|| {
                    let point = candidates
                        .find(|point| !used.contains(point))
                        .expect("bounded preview leaves unused private-use characters");
                    used.insert(point);
                    char::from_u32(point).expect("private-use code points are Unicode scalars")
                });
                encoded.push(*replacement);
            }
        }
    }
    let plain = extract_markdown_plain_text(&encoded, MarkdownPlainTextMode::All)?;
    let replacements = replacements
        .into_iter()
        .map(|(unit, character)| (character, unit))
        .collect::<BTreeMap<_, _>>();
    let mut units = Vec::new();
    for character in plain.chars() {
        if let Some(unit) = replacements.get(&character) {
            units.push(*unit);
        } else {
            let mut buffer = [0_u16; 2];
            units.extend_from_slice(character.encode_utf16(&mut buffer));
        }
    }
    Ok(JsonString::from_utf16(&units))
}

fn reserve_numeric_entities(units: &[u16], used: &mut BTreeSet<u32>) {
    for start in 0..units.len().saturating_sub(2) {
        if units[start..start + 2] != [0x26, 0x23] {
            continue;
        }
        let mut at = start + 2;
        let radix = if matches!(units.get(at), Some(0x78 | 0x58)) {
            at += 1;
            16
        } else {
            10
        };
        let mut point = 0_u32;
        while let Some(unit) = units.get(at) {
            let Some(digit) =
                char::from_u32(u32::from(*unit)).and_then(|value| value.to_digit(radix))
            else {
                break;
            };
            let Some(next) = point
                .checked_mul(radix)
                .and_then(|point| point.checked_add(digit))
            else {
                break;
            };
            point = next;
            if point > 0x0010_ffff {
                break;
            }
            used.insert(point);
            at += 1;
        }
    }
}
