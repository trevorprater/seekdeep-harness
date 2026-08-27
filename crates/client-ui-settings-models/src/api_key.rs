//! Browser-side API-key judgment.

/// Copy key explaining why a typed key cannot be saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeyFailureKey {
    /// Non-empty input trims to nothing.
    KeyBlank,
    /// Wrapped, environment-line, control, whitespace, or non-ASCII input.
    KeyIllegalCharacters,
}

fn ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            | '\u{2001}'
            | '\u{2002}'
            | '\u{2003}'
            | '\u{2004}'
            | '\u{2005}'
            | '\u{2006}'
            | '\u{2007}'
            | '\u{2008}'
            | '\u{2009}'
            | '\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

fn environment_line(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    let mut offset = 1;
    for byte in bytes {
        if byte == b'=' {
            return value
                .as_bytes()
                .get(offset + 1)
                .is_some_and(|next| *next != b'=');
        }
        if !byte.is_ascii_uppercase() && !byte.is_ascii_digit() && byte != b'_' {
            return false;
        }
        offset += 1;
    }
    false
}

fn quoted(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    matches!(first, '\'' | '"' | '`') && value.chars().count() > 1 && value.ends_with(first)
}

/// Judges one untrimmed key draft.
#[must_use]
pub fn api_key_failure(draft: &str) -> Option<ApiKeyFailureKey> {
    if draft.is_empty() {
        return None;
    }
    let value = draft.trim_matches(ecmascript_whitespace);
    if value.is_empty() {
        return Some(ApiKeyFailureKey::KeyBlank);
    }
    if environment_line(value)
        || quoted(value)
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Some(ApiKeyFailureKey::KeyIllegalCharacters);
    }
    None
}
