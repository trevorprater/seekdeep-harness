//! Source-compatible Code Mode presentation over complete JSON snapshots.

use seekdeep_code_runtime::{
    CodeJsonValue,
    json::{CodeJsonRef, CodeJsonToken},
};
use seekdeep_llm::ContentBlock;

pub(crate) fn render_output(value: &CodeJsonValue) -> anyhow::Result<Vec<ContentBlock>> {
    let mut text = Vec::new();
    let logs = value
        .get("logs")
        .and_then(CodeJsonRef::array_items)
        .ok_or_else(|| anyhow::anyhow!("run_code output.logs must be an array"))?;
    for (index, line) in logs.into_iter().enumerate() {
        if index > 0 {
            text.push(u16::from(b'\n'));
        }
        text.extend(
            line.to_utf16()
                .ok_or_else(|| anyhow::anyhow!("run_code output.logs must contain strings"))?,
        );
    }
    if let Some(result) = value.get("result") {
        let rendered = result
            .to_utf16()
            .unwrap_or_else(|| render_json(result).encode_utf16().collect());
        if !rendered.is_empty() {
            if !text.is_empty() {
                text.push(u16::from(b'\n'));
            }
            text.extend(rendered);
        }
    }
    if text.is_empty() {
        text.extend("(run_code completed with no output)".encode_utf16());
    }
    Ok(vec![ContentBlock::text_utf16(&text)])
}

struct Container {
    object: bool,
    count: usize,
    compact: bool,
}

fn before_item(output: &mut String, stack: &mut [Container]) {
    let depth = stack.len();
    let container = stack.last_mut().expect("container is open");
    if container.count > 0 {
        output.push(',');
    }
    if !container.compact {
        output.push('\n');
        output.push_str(&"  ".repeat(depth));
    }
    container.count += 1;
}

fn before_value(output: &mut String, stack: &mut [Container]) {
    if stack.last().is_some_and(|container| !container.object) {
        before_item(output, stack);
    }
}

fn render_json(value: CodeJsonRef<'_>) -> String {
    let mut output = String::new();
    let mut stack: Vec<Container> = Vec::new();
    for token in value.tokens() {
        match token {
            CodeJsonToken::ObjectStart | CodeJsonToken::ArrayStart => {
                before_value(&mut output, &mut stack);
                let object = matches!(token, CodeJsonToken::ObjectStart);
                let compact =
                    stack.last().is_some_and(|container| container.compact) || stack.len() >= 5;
                output.push(if object { '{' } else { '[' });
                stack.push(Container {
                    object,
                    count: 0,
                    compact,
                });
            }
            CodeJsonToken::ObjectEnd | CodeJsonToken::ArrayEnd => {
                let container = stack.pop().expect("JSON container has a matching opening");
                if container.count > 0 && !container.compact {
                    output.push('\n');
                    output.push_str(&"  ".repeat(stack.len()));
                }
                output.push(if container.object { '}' } else { ']' });
            }
            CodeJsonToken::Key(key) => {
                before_item(&mut output, &mut stack);
                write_scalar(&mut output, key);
                output.push(':');
                if !stack.last().expect("object is open").compact {
                    output.push(' ');
                }
            }
            CodeJsonToken::Scalar(value) => {
                before_value(&mut output, &mut stack);
                write_scalar(&mut output, value);
            }
        }
    }
    output
}

fn write_scalar(output: &mut String, value: CodeJsonRef<'_>) {
    if let Some(units) = value.to_utf16() {
        output.push_str(CodeJsonValue::from_utf16(&units).as_raw());
    } else if let Some(number) = value.as_f64() {
        output.push_str(ryu_js::Buffer::new().format(number));
    } else {
        output.push_str(value.as_raw());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_surrogate_object_keys_and_values_as_exact_json() {
        let value = CodeJsonValue::parse(
            r#"{"\ud800":["\udfff","😀","\\ud800"],"empty":[],"object":{},"number":1.0}"#
                .to_owned(),
        )
        .unwrap();
        assert_eq!(
            render_json(value.as_ref()),
            "{\n  \"\\ud800\": [\n    \"\\udfff\",\n    \"😀\",\n    \"\\\\ud800\"\n  ],\n  \"empty\": [],\n  \"object\": {},\n  \"number\": 1\n}"
        );
    }

    #[test]
    fn limits_indentation_without_recursing_over_deep_values() {
        let json = format!("{}\"\\ud800\"{}", "[".repeat(14_000), "]".repeat(14_000));
        let value = CodeJsonValue::parse(json).unwrap();
        let rendered = render_json(value.as_ref());
        assert!(rendered.starts_with("[\n  [\n    [\n      [\n        [\n          [[[["));
        assert!(rendered.ends_with("]\n        ]\n      ]\n    ]\n  ]\n]"));
        assert!(rendered.contains(r#""\ud800""#));
        assert_eq!(rendered.matches('\n').count(), 10);
    }
}
