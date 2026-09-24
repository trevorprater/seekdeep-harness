//! ECMAScript JSON text parsing and serialization over exact UTF-16 values.

use std::{collections::BTreeMap, fmt::Write as _};

use crate::{JsonRef, JsonString, JsonValue};

pub(crate) fn parse(text: &JsonString) -> serde_json::Result<JsonValue> {
    if let Some(text) = text.as_str() {
        return JsonValue::parse(text.to_owned());
    }
    let mut encoded = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for point in char::decode_utf16(text.utf16_units().iter().copied()) {
        match point {
            Ok(character) => {
                encoded.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' && quoted {
                    escaped = true;
                } else if character == '"' {
                    quoted = !quoted;
                }
            }
            Err(error) if quoted && !escaped => {
                write!(encoded, "\\u{:04x}", error.unpaired_surrogate())
                    .expect("writing a string cannot fail");
            }
            Err(_) => {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    "unexpected UTF-16 surrogate in JSON text",
                ));
            }
        }
    }
    JsonValue::parse(encoded)
}

enum Task<'a> {
    Value(JsonRef<'a>, usize),
    Entry {
        comma: bool,
        depth: usize,
        key: Option<JsonRef<'a>>,
    },
    End(char, usize),
}

fn newline(output: &mut String, depth: usize, pretty: bool) {
    if pretty {
        output.push('\n');
        for _ in 0..depth {
            output.push_str("  ");
        }
    }
}

fn scalar(output: &mut String, value: JsonRef<'_>) {
    if let Some(units) = value.to_utf16() {
        output.push_str(JsonString::from_utf16(&units).as_raw());
    } else if let Some(number) = value.as_f64() {
        if number.is_finite() {
            output.push_str(ryu_js::Buffer::new().format(number));
        } else {
            output.push_str("null");
        }
    } else {
        output.push_str(value.as_raw());
    }
}

fn array_index(units: &[u16]) -> Option<u32> {
    if units.is_empty() || (units.len() > 1 && units[0] == u16::from(b'0')) {
        return None;
    }
    let mut index = 0_u32;
    for unit in units {
        let digit = unit.checked_sub(u16::from(b'0'))?;
        if digit > 9 {
            return None;
        }
        index = index.checked_mul(10)?.checked_add(u32::from(digit))?;
    }
    (index != u32::MAX).then_some(index)
}

fn object_entries(value: JsonRef<'_>) -> Option<Vec<(JsonRef<'_>, JsonRef<'_>)>> {
    let mut indices = BTreeMap::<Vec<u16>, usize>::new();
    let mut entries = Vec::new();
    for (key, value) in value.object_entries()? {
        let units = key.to_utf16().expect("JSON object keys are strings");
        if let Some(index) = indices.get(&units) {
            entries[*index] = (key, value);
        } else {
            indices.insert(units, entries.len());
            entries.push((key, value));
        }
    }
    entries.sort_by_key(|(key, _)| {
        let index = array_index(&key.to_utf16().expect("JSON object keys are strings"));
        (index.is_none(), index.unwrap_or_default())
    });
    Some(entries)
}

pub(crate) fn stringify(value: JsonRef<'_>, pretty: bool) -> String {
    let mut output = String::new();
    let mut stack = vec![Task::Value(value, 0)];
    while let Some(task) = stack.pop() {
        match task {
            Task::Value(value, depth) => {
                let (opening, closing, entries) = if let Some(entries) = object_entries(value) {
                    (
                        '{',
                        '}',
                        entries
                            .into_iter()
                            .map(|(key, value)| (Some(key), value))
                            .collect::<Vec<_>>(),
                    )
                } else if let Some(values) = value.array_items() {
                    (
                        '[',
                        ']',
                        values.into_iter().map(|value| (None, value)).collect(),
                    )
                } else {
                    scalar(&mut output, value);
                    continue;
                };
                output.push(opening);
                if entries.is_empty() {
                    output.push(closing);
                    continue;
                }
                stack.push(Task::End(closing, depth));
                for (index, (key, value)) in entries.into_iter().enumerate().rev() {
                    stack.push(Task::Value(value, depth + 1));
                    stack.push(Task::Entry {
                        comma: index > 0,
                        depth: depth + 1,
                        key,
                    });
                }
            }
            Task::Entry { comma, depth, key } => {
                if comma {
                    output.push(',');
                }
                newline(&mut output, depth, pretty);
                if let Some(key) = key {
                    scalar(&mut output, key);
                    output.push(':');
                    if pretty {
                        output.push(' ');
                    }
                }
            }
            Task::End(closing, depth) => {
                newline(&mut output, depth, pretty);
                output.push(closing);
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stringify_normalizes_strings_numbers_and_property_order() {
        let value = JsonValue::parse(
            r#"{"later":0,"9":"\ud800","1":"\ud83d\ude00","01":1.0,"later":-0,"__proto__":"\udfff","overflow":1e400,"rounded":9007199254740993}"#.to_owned(),
        ).unwrap();
        assert_eq!(
            value.stringify(),
            r#"{"1":"😀","9":"\ud800","later":0,"01":1,"__proto__":"\udfff","overflow":null,"rounded":9007199254740992}"#
        );
        assert_eq!(
            JsonValue::parse(value.stringify_pretty()).unwrap(),
            JsonValue::parse(value.stringify()).unwrap()
        );
    }

    #[test]
    fn parse_text_accepts_literal_surrogates_only_inside_json_strings() {
        let text =
            JsonString::from_utf16(&[0x7b, 0x22, 0xd800, 0x22, 0x3a, 0x22, 0xdfff, 0x22, 0x7d]);
        assert_eq!(
            JsonValue::parse_text(&text).unwrap().stringify(),
            r#"{"\ud800":"\udfff"}"#
        );
        for units in [
            vec![0xd800],
            vec![0x22, 0x5c, 0xd800, 0x22],
            vec![0x22, 0xd800],
        ] {
            assert!(JsonValue::parse_text(&JsonString::from_utf16(&units)).is_err());
        }
    }

    #[test]
    fn stringify_traverses_deep_payloads_without_recursive_calls() {
        let raw = format!("{}\"\\ud800\"{}", "[".repeat(14_000), "]".repeat(14_000));
        let value = JsonValue::parse(raw.clone()).unwrap();
        assert_eq!(value.stringify(), raw);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn parse_and_stringify_match_node_json_operations() {
        use std::{
            io::Write as _,
            process::{Command, Stdio},
        };

        let mut cases = [
            r#"{"z":1,"2":"\ud800","1":"\udc00","z":2,"01":"\u0061","__proto__":["\ud83d\ude00","\\ud800"]}"#,
            r"[1.0,-0,1e400,-1e400,9007199254740993,0.0000001,0.000001,1e21,true,null,{},[]]",
            r#"{"4294967295":0,"4294967294":1,"0":2,"00":3,"-0":4}"#,
            r#""\ud800\udc00\udfff""#,
            r#"{"unterminated":"x"#,
        ].map(JsonString::from).to_vec();
        cases.extend([
            JsonString::from_utf16(&[0x22, 0xd800, 0x22]),
            JsonString::from_utf16(&[0x7b, 0x22, 0xd800, 0x22, 0x3a, 0x22, 0xdfff, 0x22, 0x7d]),
            JsonString::from_utf16(&[0x22, 0x5c, 0xd800, 0x22]),
            JsonString::from_utf16(&[0xd800]),
        ]);
        let script = r"import fs from 'node:fs';
const cases = JSON.parse(fs.readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify(cases.map(text => {
  try { const value = JSON.parse(text); return [JSON.stringify(value), JSON.stringify(value, null, 2)]; }
  catch { return null; }
})));";
        let mut child = Command::new("node")
            .args(["--input-type=module", "-e", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Node JSON oracle");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                JsonValue::from_serialize(&cases)
                    .unwrap()
                    .as_raw()
                    .as_bytes(),
            )
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let expected: Vec<Option<[String; 2]>> = serde_json::from_slice(&output.stdout).unwrap();
        for (text, expected) in cases.iter().zip(expected) {
            let actual = JsonValue::parse_text(text)
                .ok()
                .map(|value| [value.stringify(), value.stringify_pretty()]);
            assert_eq!(actual, expected, "JSON text: {}", text.as_raw());
        }
    }
}
