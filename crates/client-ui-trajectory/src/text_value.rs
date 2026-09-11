//! UTF-16 text operations used by trajectory previews and search.

use seekdeep_lossless_json::{JsonString, JsonValue};

pub(crate) fn member(value: &JsonValue, key: &str) -> Option<JsonString> {
    value.get_value(key)?.deserialize().ok()
}

pub(crate) const fn is_space(unit: u16) -> bool {
    matches!(
        unit,
        0x9..=0xd | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029
            | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

pub(crate) fn lowercase(text: &JsonString) -> JsonString {
    let mut output = JsonString::default();
    let mut scalar = String::new();
    for point in char::decode_utf16(text.utf16_units().iter().copied()) {
        match point {
            Ok(character) => scalar.push(character),
            Err(surrogate) => {
                output.push_str(&scalar.to_lowercase());
                scalar.clear();
                output.push_utf16(&[surrogate.unpaired_surrogate()]);
            }
        }
    }
    output.push_str(&scalar.to_lowercase());
    output
}

pub(crate) fn contains(text: &JsonString, needle: &JsonString) -> bool {
    needle.is_empty()
        || text
            .utf16_units()
            .windows(needle.len_utf16())
            .any(|window| window == needle.utf16_units())
}

pub(crate) fn split_once(text: &JsonString, separator: &str) -> Option<(JsonString, JsonString)> {
    let separator = separator.encode_utf16().collect::<Vec<_>>();
    let at = text
        .utf16_units()
        .windows(separator.len())
        .position(|window| window == separator)?;
    Some((
        JsonString::from_utf16(&text.utf16_units()[..at]),
        JsonString::from_utf16(&text.utf16_units()[at + separator.len()..]),
    ))
}
