//! JSON-compatible argument formatting for native logger exporters.

use serde_json::Value;

use super::{LogExporter, LogMessage, Logger};

pub(super) fn format(exporter: &LogExporter, message: &LogMessage) -> String {
    let mut values = message.args.iter();
    let format = if let Some(Value::String(format)) = message.args.first() {
        values.next();
        format.as_str()
    } else {
        "%o"
    };
    let mut output = String::new();
    let mut chars = format.chars();
    while let Some(character) = chars.next() {
        if character != '%' {
            output.push(character);
            continue;
        }
        let Some(code) = chars.next() else {
            output.push('%');
            break;
        };
        if code == '%' {
            output.push('%');
        } else if code.is_ascii_alphabetic()
            && (exporter.formatters.contains_key(&code)
                || matches!(code, 's' | 'd' | 'i' | 'f' | 'o' | 'O' | 'c' | 'C'))
        {
            output.push_str(&placeholder(code, values.next(), exporter, message));
        } else {
            output.push('%');
            output.push(code);
        }
    }
    for value in values {
        output.push(' ');
        if value.is_array() || value.is_object() {
            output.push_str(&placeholder('o', Some(value), exporter, message));
        } else {
            output.push_str(&javascript_string(value));
        }
    }
    let mut lines = output.split('\n').peekable();
    let mut formatted = String::new();
    while let Some(line) = lines.next() {
        let line = if lines.peek().is_some() {
            line.strip_suffix('\r').unwrap_or(line)
        } else {
            line
        };
        formatted.push_str(&truncate_line(line, exporter.max_length));
        if lines.peek().is_some() {
            formatted.push('\n');
        }
    }
    formatted
}

fn placeholder(
    code: char,
    value: Option<&Value>,
    exporter: &LogExporter,
    message: &LogMessage,
) -> String {
    if let Some(formatter) = exporter.formatters.get(&code) {
        return formatter(value, exporter, message);
    }
    match code {
        's' => value.map_or_else(|| "undefined".to_owned(), javascript_string),
        'd' | 'i' => numeric_string(value.map_or(f64::NAN, number).trunc()),
        'f' => numeric_string(value.map_or(f64::NAN, number)),
        'o' | 'O' => value.map_or_else(|| "undefined".to_owned(), stringify),
        'c' => String::new(),
        'C' => {
            let text = value.map_or_else(|| "undefined".to_owned(), javascript_string);
            Logger::code(&message.name, exporter.colors).map_or_else(
                || text.clone(),
                |code| Logger::color(exporter, code, &text, ""),
            )
        }
        _ => format!("%{code}"),
    }
}

pub(super) fn number(value: &Value) -> f64 {
    match value {
        Value::Number(value) => value
            .as_f64()
            .unwrap_or_else(|| value.to_string().parse().unwrap_or(f64::NAN)),
        Value::Bool(value) => i32::from(*value).into(),
        Value::Null => 0.0,
        Value::String(value) => parse_number(value),
        Value::Array(_) => parse_number(&javascript_string(value)),
        Value::Object(_) => f64::NAN,
    }
}

fn parse_number(value: &str) -> f64 {
    let value = value.trim_matches(|character| {
        matches!(character, '\u{9}'..='\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
    });
    if value.is_empty() {
        return 0.0;
    }
    if matches!(value, "Infinity" | "+Infinity") {
        return f64::INFINITY;
    }
    if value == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    for (prefixes, width) in [(["0x", "0X"], 4), (["0o", "0O"], 3), (["0b", "0B"], 1)] {
        if let Some(digits) = prefixes
            .into_iter()
            .find_map(|prefix| value.strip_prefix(prefix))
        {
            return radix_number(digits, width);
        }
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E'))
    {
        value.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

#[allow(clippy::cast_precision_loss)] // The retained significand is at most 53 bits before rounding.
fn radix_number(digits: &str, width: u32) -> f64 {
    let radix = 1 << width;
    if digits.is_empty() || !digits.chars().all(|character| character.is_digit(radix)) {
        return f64::NAN;
    }
    let mut significant_bits = 0_usize;
    let mut significand = 0_u64;
    let mut guard = false;
    let mut sticky = false;
    for digit in digits
        .chars()
        .map(|character| character.to_digit(radix).unwrap())
    {
        for shift in (0..width).rev() {
            let bit = digit & (1 << shift) != 0;
            if significant_bits == 0 && !bit {
                continue;
            }
            significant_bits += 1;
            if significant_bits <= 53 {
                significand = (significand << 1) | u64::from(bit);
            } else if significant_bits == 54 {
                guard = bit;
            } else {
                sticky |= bit;
            }
        }
    }
    if significant_bits <= 53 {
        return significand as f64;
    }
    if significant_bits > 1024 {
        return f64::INFINITY;
    }
    if guard && (sticky || significand & 1 != 0) {
        significand += 1;
    }
    (significand as f64) * 2_f64.powi(i32::try_from(significant_bits - 53).unwrap())
}

fn numeric_string(value: f64) -> String {
    ryu_js::Buffer::new().format(value).to_owned()
}

fn javascript_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(_) => numeric_string(number(value)),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    javascript_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn stringify(value: &Value) -> String {
    match value {
        Value::Number(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .map_or_else(|| "null".to_owned(), numeric_string),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(stringify).collect::<Vec<_>>().join(",")
        ),
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| {
                key.parse::<u32>()
                    .ok()
                    .filter(|index| *index != u32::MAX && index.to_string() == **key)
                    .map_or((true, 0), |index| (false, index))
            });
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        stringify(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        _ => serde_json::to_string(value).expect("JSON scalar serialization cannot fail"),
    }
}

fn truncate_line(line: &str, max_length: usize) -> String {
    let units = line.encode_utf16().collect::<Vec<_>>();
    if units.len() <= max_length {
        return line.to_owned();
    }
    format!("{}...", String::from_utf16_lossy(&units[..max_length]))
}
