//! Python JSON spacing and numeric spelling for source-owned snapshot text.

use std::fmt::Write as _;

use serde_json::Value;

pub(crate) fn dumps(value: &Value, pretty: bool, ascii: bool) -> String {
    let mut output = String::new();
    render(value, &mut output, pretty, ascii, true, 0);
    output
}

pub(crate) fn compact(value: &Value) -> String {
    let mut output = String::new();
    render(value, &mut output, false, false, false, 0);
    output
}

fn render(
    value: &Value,
    output: &mut String,
    pretty: bool,
    ascii: bool,
    spaced: bool,
    depth: usize,
) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&number(value)),
        Value::String(value) => string(value, output, ascii),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                separator(output, pretty, spaced, depth, index);
                render(value, output, pretty, ascii, spaced, depth + 1);
            }
            close(output, pretty, depth, values.is_empty(), ']');
        }
        Value::Object(values) => {
            output.push('{');
            for (index, (key, value)) in values.iter().enumerate() {
                separator(output, pretty, spaced, depth, index);
                string(key, output, ascii);
                output.push_str(if spaced { ": " } else { ":" });
                render(value, output, pretty, ascii, spaced, depth + 1);
            }
            close(output, pretty, depth, values.is_empty(), '}');
        }
    }
}

fn separator(output: &mut String, pretty: bool, spaced: bool, depth: usize, index: usize) {
    if index > 0 {
        output.push(',');
        if !pretty && spaced {
            output.push(' ');
        }
    }
    if pretty {
        output.push('\n');
        output.push_str(&"  ".repeat(depth + 1));
    }
}

fn close(output: &mut String, pretty: bool, depth: usize, empty: bool, delimiter: char) {
    if pretty && !empty {
        output.push('\n');
        output.push_str(&"  ".repeat(depth));
    }
    output.push(delimiter);
}

fn string(value: &str, output: &mut String, ascii: bool) {
    let encoded = serde_json::to_string(value).expect("string serialization is infallible");
    for character in encoded.chars() {
        if ascii && character >= '\u{7f}' {
            for unit in character.encode_utf16(&mut [0; 2]) {
                write!(output, "\\u{unit:04x}").expect("string output is infallible");
            }
        } else {
            output.push(character);
        }
    }
}

fn number(value: &serde_json::Number) -> String {
    let raw = value.to_string();
    if !raw.contains(['.', 'e', 'E']) {
        return raw;
    }
    let Some(value) = value.as_f64() else {
        return raw;
    };
    let rendered = format!("{value:?}");
    if let Some((mantissa, exponent)) = rendered.split_once('e') {
        let exponent: i32 = exponent.parse().expect("float exponent is integral");
        format!("{mantissa}e{exponent:+03}")
    } else {
        rendered
    }
}
