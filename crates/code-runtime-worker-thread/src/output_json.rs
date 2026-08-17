//! Exact bounded JSON-prefix accounting for runtime output.

use serde_json::Value;

/// Measures a JSON string, including quotes, without materializing escapes.
#[must_use]
pub fn json_string_bytes_up_to(text: &str, max_bytes: usize) -> Option<usize> {
    json_utf16_string_bytes_up_to(&text.encode_utf16().collect::<Vec<_>>(), max_bytes)
}

/// UTF-16 form of [`json_string_bytes_up_to`], preserving lone surrogates at
/// the JavaScript boundary even though Rust `str` cannot contain them.
#[must_use]
pub fn json_utf16_string_bytes_up_to(units: &[u16], max_bytes: usize) -> Option<usize> {
    if max_bytes < 2 {
        return None;
    }
    let mut bytes = 2usize;
    let mut index = 0usize;
    while index < units.len() {
        let (cost, width) = serialized_unit_cost(units, index);
        bytes = bytes.checked_add(cost)?;
        if bytes > max_bytes {
            return None;
        }
        index += width;
    }
    Some(bytes)
}

fn serialized_unit_cost(units: &[u16], index: usize) -> (usize, usize) {
    let unit = units[index];
    if (0xd800..=0xdbff).contains(&unit)
        && units
            .get(index + 1)
            .is_some_and(|next| (0xdc00..=0xdfff).contains(next))
    {
        return (4, 2);
    }
    if (0xd800..=0xdfff).contains(&unit) {
        return (6, 1);
    }
    match unit {
        0x22 | 0x5c | 0x08 | 0x09 | 0x0a | 0x0c | 0x0d | 0x80..=0x7ff => (2, 1),
        0x00..=0x1f => (6, 1),
        0x20..=0x7f => (1, 1),
        _ => (3, 1),
    }
}

/// Returns the longest code-point-aligned prefix fitting as one JSON string.
#[must_use]
pub fn truncate_json_string_bytes(text: &str, max_bytes: usize) -> String {
    let units = text.encode_utf16().collect::<Vec<_>>();
    let retained = truncate_json_utf16_string_bytes(&units, max_bytes);
    // A Rust input cannot contain lone surrogates, and truncation never splits
    // a valid pair, so reconstruction is infallible for this wrapper.
    String::from_utf16_lossy(&retained)
}

/// UTF-16 form of [`truncate_json_string_bytes`] for JavaScript strings with
/// lone surrogate code units.
#[must_use]
pub fn truncate_json_utf16_string_bytes(units: &[u16], max_bytes: usize) -> Vec<u16> {
    if max_bytes < 2 {
        return Vec::new();
    }
    let mut bytes = 2usize;
    let mut index = 0usize;
    while index < units.len() {
        let (cost, width) = serialized_unit_cost(units, index);
        if bytes.saturating_add(cost) > max_bytes {
            break;
        }
        bytes += cost;
        index += width;
    }
    units[..index].to_vec()
}

enum MeasureTask<'a> {
    Value(&'a Value),
    Array(&'a [Value], usize),
    Object(Vec<(&'a str, &'a Value)>, usize),
}

/// Measures one lossless JSON value and stops immediately when the cap is
/// crossed. Traversal is iterative and independent of application depth.
#[must_use]
pub fn json_value_bytes_up_to(value: &Value, max_bytes: usize) -> Option<usize> {
    let mut bytes = 0usize;
    let mut tasks = vec![MeasureTask::Value(value)];
    while let Some(task) = tasks.pop() {
        match task {
            MeasureTask::Value(current) => match current {
                Value::Null => add(&mut bytes, 4, max_bytes)?,
                Value::Bool(boolean) => add(&mut bytes, if *boolean { 4 } else { 5 }, max_bytes)?,
                Value::Number(number) => {
                    let rendered = render_number(number)?;
                    add(&mut bytes, rendered.len(), max_bytes)?;
                }
                Value::String(text) => {
                    let remaining = max_bytes.checked_sub(bytes)?;
                    bytes += json_string_bytes_up_to(text, remaining)?;
                }
                Value::Array(items) => {
                    add(&mut bytes, 2, max_bytes)?;
                    if !items.is_empty() {
                        tasks.push(MeasureTask::Array(items, 0));
                    }
                }
                Value::Object(object) => {
                    add(&mut bytes, 2, max_bytes)?;
                    if !object.is_empty() {
                        tasks.push(MeasureTask::Object(
                            object
                                .iter()
                                .map(|(key, item)| (key.as_str(), item))
                                .collect(),
                            0,
                        ));
                    }
                }
            },
            MeasureTask::Array(items, index) => {
                if index > 0 {
                    add(&mut bytes, 1, max_bytes)?;
                }
                if index + 1 < items.len() {
                    tasks.push(MeasureTask::Array(items, index + 1));
                }
                tasks.push(MeasureTask::Value(&items[index]));
            }
            MeasureTask::Object(items, index) => {
                if index > 0 {
                    add(&mut bytes, 1, max_bytes)?;
                }
                let (key, item) = items[index];
                let remaining = max_bytes.checked_sub(bytes)?;
                let key_bytes = json_string_bytes_up_to(key, remaining)?;
                add(&mut bytes, key_bytes.saturating_add(1), max_bytes)?;
                if index + 1 < items.len() {
                    tasks.push(MeasureTask::Object(items, index + 1));
                }
                tasks.push(MeasureTask::Value(item));
            }
        }
    }
    Some(bytes)
}

fn add(bytes: &mut usize, cost: usize, max_bytes: usize) -> Option<()> {
    *bytes = bytes.checked_add(cost)?;
    (*bytes <= max_bytes).then_some(())
}

fn render_number(number: &serde_json::Number) -> Option<String> {
    if let Some(integer) = number.as_i64() {
        return Some(integer.to_string());
    }
    if let Some(integer) = number.as_u64() {
        return Some(integer.to_string());
    }
    let float = number.as_f64()?;
    if !float.is_finite() || float == 0.0 && float.is_sign_negative() {
        return None;
    }
    let mut buffer = ryu_js::Buffer::new();
    Some(buffer.format_finite(float).to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn strings_account_every_escape_surrogate_and_aligned_cut() {
        assert_eq!(json_string_bytes_up_to("fits", 6), Some(6));
        assert_eq!(json_string_bytes_up_to("fits", 5), None);
        assert_eq!(truncate_json_string_bytes("fits", 6), "fits");
        assert_eq!(truncate_json_string_bytes("x", 1), "");
        let units = vec![
            u16::from(b'"'),
            u16::from(b'\\'),
            0x08,
            0x09,
            0x0a,
            0x0c,
            0x0d,
            0,
            0xd83d,
            0xde00,
            0xd800,
            0x20ac,
            u16::from(b'a'),
        ];
        let bytes = json_utf16_string_bytes_up_to(&units, usize::MAX).unwrap();
        let mut longer = units.clone();
        longer.push(u16::from(b'z'));
        assert_eq!(truncate_json_utf16_string_bytes(&longer, bytes), units);
        assert_eq!(
            json_utf16_string_bytes_up_to(&truncate_json_utf16_string_bytes(&longer, bytes), bytes),
            Some(bytes)
        );
        assert_eq!(
            truncate_json_string_bytes(&"\"".repeat(10_000), 32),
            "\"".repeat(15)
        );
    }

    #[test]
    fn values_match_json_serialization_and_fail_at_one_byte_under() {
        let value = json!({
            "empty": {}, "nil": null, "yes": true, "no": false,
            "number": 1.5, "text": "\"\n😀", "array": [1, "x"]
        });
        let bytes = serde_json::to_vec(&value).unwrap().len();
        assert_eq!(json_value_bytes_up_to(&value, bytes), Some(bytes));
        assert_eq!(json_value_bytes_up_to(&value, bytes - 1), None);
        assert_eq!(json_value_bytes_up_to(&json!([]), 1), None);
        assert_eq!(json_value_bytes_up_to(&json!([]), 2), Some(2));
        assert_eq!(json_value_bytes_up_to(&Value::Null, 3), None);
        assert_eq!(json_value_bytes_up_to(&json!(10), 1), None);
        assert_eq!(json_value_bytes_up_to(&json!(false), 4), None);
        assert_eq!(json_value_bytes_up_to(&json!([null]), 5), None);
        assert_eq!(json_value_bytes_up_to(&json!([0, 0]), 3), None);
        assert_eq!(json_value_bytes_up_to(&json!({ "a": null }), 9), None);
    }

    #[test]
    fn deep_values_are_measured_iteratively() {
        let mut value = Value::Null;
        for _ in 0..5_000 {
            value = Value::Array(vec![value]);
        }
        assert_eq!(json_value_bytes_up_to(&value, 10_004), Some(10_004));
        assert_eq!(json_value_bytes_up_to(&value, 10_003), None);
        // Avoid recursive `serde_json::Value` drop overflowing debug stacks on
        // platforms with unusually small test-thread stacks.
        std::mem::forget(value);
    }
}
