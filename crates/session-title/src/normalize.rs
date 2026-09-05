//! Title text normalization and UTF-8-safe truncation.

use std::sync::LazyLock;

use regress::Regex;

fn osc_sequence() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        let esc = '\u{1b}';
        let bel = '\u{7}';
        let osc = '\u{9d}';
        let bs = r"\\";
        let pattern =
            format!("(?:{esc}]|{osc})(?:(?!{bel}|{esc}{bs})[\\s\\S])*(?:{bel}|{esc}{bs}|$)");
        Regex::new(&pattern).expect("osc sequence regex")
    });
    &RE
}

fn csi_sequence() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new("(?:\u{1b}\\[|\u{9b})[0-?]*[ -/]*[@-~]").expect("csi sequence regex")
    });
    &RE
}

fn esc_sequence() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("\u{1b}[@-_]").expect("esc sequence regex"));
    &RE
}

fn control_character() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new("[\u{0}-\u{8}\u{b}\u{c}\u{e}-\u{1f}\u{7f}-\u{9f}]").expect("control regex")
    });
    &RE
}

fn directional_control() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            "[\u{200b}\u{200e}\u{200f}\u{202a}-\u{202e}\u{2060}-\u{2064}\u{2066}-\u{206f}\u{feff}]",
        )
        .expect("directional regex")
    });
    &RE
}

fn whitespace_run() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new("\\s+").expect("whitespace regex"));
    &RE
}

fn assert_positive_integer(name: &str, value: usize) {
    assert!(value > 0, "{name} must be a positive integer");
}

/// Removes controls and produces one trimmed, whitespace-normalized line.
fn clean_title_text(input: &str) -> String {
    let step = osc_sequence().replace_all(input, "");
    let step = csi_sequence().replace_all(&step, "");
    let step = esc_sequence().replace_all(&step, "");
    let step = control_character().replace_all(&step, "");
    let step = directional_control().replace_all(&step, "");
    whitespace_run().replace_all(&step, " ").trim().to_owned()
}

/// Truncates a string to a UTF-8 byte budget without splitting a code point.
///
/// # Panics
///
/// Panics when `max_bytes` is zero.
#[must_use]
pub fn truncate_title_utf8(input: &str, max_bytes: usize) -> String {
    assert_positive_integer("maxBytes", max_bytes);
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let mut used = 0;
    let mut output = String::new();
    for character in input.chars() {
        let bytes = character.len_utf8();
        if used + bytes > max_bytes {
            break;
        }
        output.push(character);
        used += bytes;
    }
    output
}

/// Normalizes one accepted session title and enforces its UTF-8 byte budget.
///
/// # Panics
///
/// Panics when `max_bytes` is zero.
#[must_use]
pub fn normalize_session_title(input: &str, max_bytes: usize) -> String {
    truncate_title_utf8(&clean_title_text(input), max_bytes)
        .trim_end()
        .to_owned()
}

/// Derives the deterministic first-prompt fallback.
///
/// # Panics
///
/// Panics when `max_words` or `max_bytes` is zero.
#[must_use]
pub fn fallback_session_title(input: &str, max_words: usize, max_bytes: usize) -> String {
    assert_positive_integer("maxWords", max_words);
    let words = clean_title_text(input)
        .split(' ')
        .filter(|word| !word.is_empty())
        .take(max_words)
        .collect::<Vec<_>>()
        .join(" ");
    truncate_title_utf8(&words, max_bytes).trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_controls_and_collapses_whitespace() {
        assert_eq!(
            clean_title_text("  Hand\tpicked   name  "),
            "Hand picked name"
        );
        assert_eq!(clean_title_text("  \u{1b}[31m  "), "");
    }

    #[test]
    fn truncates_to_a_utf8_byte_budget() {
        assert_eq!(truncate_title_utf8("😀😀", 3), "");
        assert_eq!(truncate_title_utf8("abc", 10), "abc");
        assert_eq!(truncate_title_utf8("abcdef", 4), "abcd");
    }

    #[test]
    fn normalizes_and_falls_back() {
        assert_eq!(
            normalize_session_title("  Hand\tpicked   name  ", 40),
            "Hand picked name"
        );
        assert_eq!(
            fallback_session_title("Derivable prompt words", 5, 40),
            "Derivable prompt words"
        );
        assert_eq!(
            fallback_session_title("one two three four five six", 2, 40),
            "one two"
        );
    }
}
