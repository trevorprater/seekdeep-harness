//! JavaScript string semantics the source relies on: `\s`, `trim`, and UTF-16 offsets.

/// ECMAScript `\s`: `WhiteSpace` plus `LineTerminator` code points.
pub(crate) fn is_js_space(character: char) -> bool {
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
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `String.prototype.trim`.
pub(crate) fn trim(value: &str) -> &str {
    value.trim_matches(is_js_space)
}

/// `value.replace(/\s+/g, ' ')`.
pub(crate) fn collapse_spaces(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut in_space = false;
    for character in value.chars() {
        if is_js_space(character) {
            if !in_space {
                result.push(' ');
                in_space = true;
            }
        } else {
            result.push(character);
            in_space = false;
        }
    }
    result
}

/// `value.replace(/\s*;?\s*$/, '')`.
pub(crate) fn strip_member_tail(value: &str) -> &str {
    let trimmed = value.trim_end_matches(is_js_space);
    trimmed
        .strip_suffix(';')
        .map_or(trimmed, |head| head.trim_end_matches(is_js_space))
}

/// `value.replace(/\s+/g, ' ').trim()`, or `None` when nothing remains.
pub(crate) fn normalized_doc_text(value: Option<&str>) -> Option<String> {
    let normalized = trim(&collapse_spaces(value?)).to_owned();
    (!normalized.is_empty()).then_some(normalized)
}

/// `(/^(.*?[.!?])(?:\s|$)/.exec(value)?.[1] ?? value).trim()` over normalized text.
pub(crate) fn first_sentence(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    for (index, character) in characters.iter().enumerate() {
        if matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            break;
        }
        if matches!(character, '.' | '!' | '?')
            && characters
                .get(index + 1)
                .is_none_or(|next| is_js_space(*next))
        {
            return trim(&characters[..=index].iter().collect::<String>()).to_owned();
        }
    }
    trim(value).to_owned()
}

/// `value.trim().split(/\s+/)`.
pub(crate) fn split_words(value: &str) -> Vec<String> {
    let trimmed = trim(value);
    if trimmed.is_empty() {
        return vec![String::new()];
    }
    trimmed
        .split(is_js_space)
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `value.trim().split(/\s+/, 1)[0]`.
pub(crate) fn first_word(value: &str) -> String {
    split_words(value).into_iter().next().unwrap_or_default()
}

pub(crate) fn utf16_length(value: &str) -> usize {
    value.encode_utf16().count()
}

/// `value.slice(start, end)` over UTF-16 code units, clamped like JavaScript.
pub(crate) fn utf16_slice(value: &str, start: usize, end: usize) -> String {
    let units = value.encode_utf16().collect::<Vec<_>>();
    let end = end.min(units.len());
    let start = start.min(end);
    String::from_utf16_lossy(&units[start..end])
}

/// `String(value)` for a JavaScript number.
pub(crate) fn number_text(value: f64) -> String {
    let mut buffer = ryu_js::Buffer::new();
    buffer.format(value).to_owned()
}

/// `Number(text)` for compiler-normalized numeric literal text.
#[expect(
    clippy::cast_precision_loss,
    reason = "JavaScript Number() rounds radix literals to the nearest double"
)]
pub(crate) fn parse_number(text: &str) -> f64 {
    let trimmed = trim(text);
    if trimmed.is_empty() {
        return 0.0;
    }
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return u128::from_str_radix(hex, 16).map_or(f64::NAN, |value| value as f64);
    }
    if let Some(octal) = trimmed
        .strip_prefix("0o")
        .or_else(|| trimmed.strip_prefix("0O"))
    {
        return u128::from_str_radix(octal, 8).map_or(f64::NAN, |value| value as f64);
    }
    if let Some(binary) = trimmed
        .strip_prefix("0b")
        .or_else(|| trimmed.strip_prefix("0B"))
    {
        return u128::from_str_radix(binary, 2).map_or(f64::NAN, |value| value as f64);
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

/// `String(BigInt(text))`: decimal digits for a JavaScript bigint literal body.
pub(crate) fn bigint_decimal(text: &str) -> String {
    let trimmed = trim(text);
    let (negative, body) = trimmed
        .strip_prefix('-')
        .map_or((false, trimmed), |body| (true, body));
    let (radix, digits) =
        if let Some(rest) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16, rest)
        } else if let Some(rest) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            (8, rest)
        } else if let Some(rest) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (2, rest)
        } else {
            (10, body)
        };
    let mut decimal: Vec<u8> = vec![0];
    for digit in digits.chars().filter(|digit| *digit != '_') {
        let Some(value) = digit.to_digit(radix) else {
            return trimmed.to_owned();
        };
        let mut carry = value;
        for slot in &mut decimal {
            let product = u32::from(*slot) * radix + carry;
            *slot = (product % 10) as u8;
            carry = product / 10;
        }
        while carry > 0 {
            decimal.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    while decimal.len() > 1 && decimal.last() == Some(&0) {
        decimal.pop();
    }
    let magnitude = decimal
        .iter()
        .rev()
        .map(|digit| char::from(b'0' + digit))
        .collect::<String>();
    if negative && magnitude != "0" {
        format!("-{magnitude}")
    } else {
        magnitude
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_tails_and_whitespace_follow_javascript_semantics() {
        assert_eq!(strip_member_tail("run(): void;  \n"), "run(): void");
        assert_eq!(strip_member_tail("run(): void ; ;"), "run(): void ;");
        assert_eq!(collapse_spaces("a\u{FEFF}\t b\u{0085}c"), "a b\u{0085}c");
        assert_eq!(trim("\u{3000} text \u{2028}"), "text");
        assert_eq!(normalized_doc_text(Some("  ")), None);
        assert_eq!(
            normalized_doc_text(Some(" one\n two ")).as_deref(),
            Some("one two")
        );
    }

    #[test]
    fn first_sentence_stops_at_terminal_punctuation_followed_by_space_or_end() {
        assert_eq!(
            first_sentence("Read v1.2 values. Then more."),
            "Read v1.2 values."
        );
        assert_eq!(
            first_sentence("No terminal punctuation"),
            "No terminal punctuation"
        );
        assert_eq!(first_sentence("Ends here?"), "Ends here?");
        assert_eq!(first_sentence("e.g. this! and that"), "e.g.");
    }

    #[test]
    fn word_splitting_matches_split_on_whitespace_runs() {
        assert_eq!(split_words(""), vec![String::new()]);
        assert_eq!(split_words(" service  key "), vec!["service", "key"]);
        assert_eq!(first_word("  object rest"), "object");
        assert_eq!(first_word(""), "");
    }

    #[test]
    fn utf16_offsets_and_numbers_match_javascript() {
        assert_eq!(utf16_length("a😀b"), 4);
        assert_eq!(utf16_slice("a😀b", 0, 3), "a😀");
        assert_eq!(number_text(1.0), "1");
        assert_eq!(number_text(-2.0), "-2");
        assert_eq!(number_text(1.5), "1.5");
        assert_eq!(number_text(1e21), "1e+21");
        assert_eq!(parse_number("16").to_bits(), 16.0_f64.to_bits());
        assert_eq!(parse_number("0x10").to_bits(), 16.0_f64.to_bits());
        assert!(parse_number("1_000").is_nan());
        assert_eq!(bigint_decimal("0x10"), "16");
        assert_eq!(bigint_decimal("-2"), "-2");
        assert_eq!(bigint_decimal("0b101"), "5");
        assert_eq!(bigint_decimal("1_000"), "1000");
        assert_eq!(bigint_decimal("-0"), "0");
    }
}
