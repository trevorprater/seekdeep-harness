//! Container indentation over validated JSON tokens, including surrogate keys.

use crate::{JsonToken, JsonValue};

struct Frame {
    object: bool,
    count: usize,
}

fn newline(output: &mut String, depth: usize) {
    output.push('\n');
    for _ in 0..depth {
        output.push_str("  ");
    }
}

fn begin_value(output: &mut String, frames: &mut [Frame]) {
    let depth = frames.len();
    if let Some(Frame {
        object: false,
        count,
    }) = frames.last_mut()
    {
        if *count > 0 {
            output.push(',');
        }
        *count += 1;
        newline(output, depth);
    }
}

pub(crate) fn format(value: &JsonValue) -> String {
    let mut output = String::new();
    let mut frames = Vec::<Frame>::new();
    for token in value.tokens() {
        match token {
            JsonToken::ArrayStart | JsonToken::ObjectStart => {
                begin_value(&mut output, &mut frames);
                let object = matches!(token, JsonToken::ObjectStart);
                output.push(if object { '{' } else { '[' });
                frames.push(Frame { object, count: 0 });
            }
            JsonToken::ArrayEnd | JsonToken::ObjectEnd => {
                let frame = frames
                    .pop()
                    .expect("validated JSON containers are balanced");
                if frame.count > 0 {
                    newline(&mut output, frames.len());
                }
                output.push(if frame.object { '}' } else { ']' });
            }
            JsonToken::Key(key) => {
                let frame = frames.last_mut().expect("object keys have a container");
                if frame.count > 0 {
                    output.push(',');
                }
                frame.count += 1;
                newline(&mut output, frames.len());
                output.push_str(key.as_raw());
                output.push_str(": ");
            }
            JsonToken::Scalar(value) => {
                begin_value(&mut output, &mut frames);
                output.push_str(value.as_raw());
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_containers_use_two_spaces_and_empty_containers_stay_inline() {
        let value =
            JsonValue::parse(r#"{"a":[1,{"b":true}],"empty":[],"object":{},"null":null}"#.into())
                .unwrap();
        assert_eq!(
            format(&value),
            "{\n  \"a\": [\n    1,\n    {\n      \"b\": true\n    }\n  ],\n  \"empty\": [],\n  \"object\": {},\n  \"null\": null\n}"
        );
    }

    #[test]
    fn string_values_and_keys_keep_their_exact_code_units() {
        let value = JsonValue::parse(r#"{"\ud800":["\udfff","😀","\\ud800"]}"#.into()).unwrap();
        let pretty = format(&value);
        assert_eq!(
            pretty,
            "{\n  \"\\ud800\": [\n    \"\\udfff\",\n    \"😀\",\n    \"\\\\ud800\"\n  ]\n}"
        );
        assert_eq!(JsonValue::parse(pretty).unwrap(), value);
        let string = JsonValue::parse(r#""\ud800""#.into()).unwrap();
        assert_eq!(format(&string), string.as_raw());
    }
}
