//! JavaScript string-length semantics for text the source measures in UTF-16 code units.
//!
//! The source counts and slices strings by UTF-16 code unit, so an astral character costs two.
//! A Rust string cannot end in a lone surrogate, so a cut that would split such a character
//! stops before it instead (DEV-003 in `porting/DEVIATIONS.md`).

/// The length of `text` in UTF-16 code units, as JavaScript's `length` reports it.
#[must_use]
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The longest prefix of `text` that fits in `max_units` UTF-16 code units.
///
/// Equals JavaScript's `slice(0, max_units)` unless that slice would split an astral character,
/// in which case the prefix ends before the character.
#[must_use]
pub fn utf16_prefix(text: &str, max_units: usize) -> &str {
    let mut units = 0;
    for (index, character) in text.char_indices() {
        let next = units + character.len_utf16();
        if next > max_units {
            return &text[..index];
        }
        units = next;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_astral_characters_as_two_units() {
        assert_eq!(utf16_len(""), 0);
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("a\u{1F600}b"), 4);
        assert_eq!(utf16_len("\u{2026}"), 1);
    }

    #[test]
    fn prefixes_by_units_and_never_splits_a_pair() {
        assert_eq!(utf16_prefix("abc", 0), "");
        assert_eq!(utf16_prefix("abc", 2), "ab");
        assert_eq!(utf16_prefix("abc", 3), "abc");
        assert_eq!(utf16_prefix("abc", 10), "abc");
        assert_eq!(utf16_prefix("a\u{1F600}b", 3), "a\u{1F600}");
        assert_eq!(utf16_prefix("a\u{1F600}b", 2), "a");
        assert_eq!(utf16_prefix("a\u{1F600}b", 4), "a\u{1F600}b");
    }
}
