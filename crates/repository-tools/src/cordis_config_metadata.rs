//! Loader entry metadata expression validation.

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde_json::Value;

const STATIC_METADATA_FIELDS: &[&str] = &["id", "name", "group", "inject", "intercept", "isolate"];

/// Returns expression-node diagnostics for one Loader entry.
#[must_use]
pub fn metadata_expression_errors(
    entry: &serde_json::Map<String, Value>,
    path: &str,
) -> Vec<String> {
    let mut problems = Vec::new();
    for field in STATIC_METADATA_FIELDS {
        let Some(value) = entry.get(*field) else {
            continue;
        };
        let mut paths = Vec::new();
        collect_expression_paths(value, &format!("{path}.{field}"), &mut paths);
        problems.extend(
            paths
                .into_iter()
                .map(|path| format!("{path}: !!js is not interpolated here")),
        );
    }
    if let Some(disabled) = entry.get("disabled") {
        if let Some(expression) = js_expression(disabled) {
            if let Some(detail) = disabled_expression_problem(expression) {
                problems.push(format!("{path}.disabled{detail}"));
            }
        } else {
            let mut paths = Vec::new();
            collect_expression_paths(disabled, &format!("{path}.disabled"), &mut paths);
            problems.extend(
                paths
                    .into_iter()
                    .map(|path| format!("{path}: !!js is not interpolated here")),
            );
        }
    }
    problems
}

fn disabled_expression_problem(expression: &str) -> Option<String> {
    let source = format!("function __check__() {{ return ({expression}); }}");
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &source, SourceType::mjs()).parse();
    (!parsed.errors.is_empty()).then(|| {
        format!(
            ": disabled expression does not parse: {}",
            parsed
                .errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        )
    })
}

fn collect_expression_paths(value: &Value, path: &str, output: &mut Vec<String>) {
    if js_expression(value).is_some() {
        output.push(path.to_owned());
        return;
    }
    match value {
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                collect_expression_paths(value, &format!("{path}[{index}]"), output);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                collect_expression_paths(value, &format!("{path}.{key}"), output);
            }
        }
        _ => {}
    }
}

fn js_expression(value: &Value) -> Option<&str> {
    value.as_object()?.get("__jsExpr")?.as_str()
}
