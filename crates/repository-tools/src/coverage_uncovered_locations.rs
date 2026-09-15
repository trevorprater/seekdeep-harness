//! Source-compatible Istanbul uncovered-location reporting.
#![allow(
    clippy::float_cmp,
    reason = "Istanbul uses exact JavaScript zero and coordinate comparisons, including non-finite sentinels"
)]

use std::{cmp::Ordering, path::Path};

use path_clean::PathClean;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod launcher;
pub use launcher::{coverage_arguments, run_coverage};

/// State passed through the Istanbul compatibility adapter.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UncoveredLocationsReport {
    /// Configured repository root used for editor-clickable relative paths.
    pub project_root: String,
    /// Formatted locations accumulated in file callback order.
    pub records: Vec<String>,
}

struct LocationRecord {
    line: f64,
    column: f64,
    text: String,
}

impl UncoveredLocationsReport {
    /// Starts a fresh report while retaining the configured repository root.
    pub fn start(&mut self) {
        self.records.clear();
    }

    /// Appends one file's uncovered statements, functions, and branch arms.
    ///
    /// # Errors
    ///
    /// Returns a malformed Istanbul map/count or path-resolution diagnostic.
    pub fn detail(&mut self, coverage: &Value) -> anyhow::Result<()> {
        let path = coverage
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("file coverage must carry a path"))?;
        let relative = relative_path(Path::new(&self.project_root), Path::new(path))?;
        let mut items = Vec::new();
        let mut add = |location: &Value, text: String| {
            items.push(LocationRecord {
                line: number(location.pointer("/start/line")),
                column: numeric_coercion(location.pointer("/start/column")),
                text,
            });
        };
        for (id, location) in object_entries(coverage, "statementMap")? {
            if !is_zero(coverage.get("s").and_then(|counts| counts.get(id))) || !usable(location) {
                continue;
            }
            add(
                location,
                format!(
                    "{relative}:{} uncovered statement{}",
                    position(location),
                    end_suffix(location)
                ),
            );
        }
        for (id, function) in object_entries(coverage, "fnMap")? {
            if !is_zero(coverage.get("f").and_then(|counts| counts.get(id))) {
                continue;
            }
            let declaration = function.get("decl").unwrap_or(&Value::Null);
            let location = if usable(declaration) {
                declaration
            } else {
                function.get("loc").unwrap_or(&Value::Null)
            };
            if !usable(location) {
                continue;
            }
            let name = function
                .get("name")
                .filter(|name| truthy(name))
                .map_or_else(String::new, |name| format!(" {}", js_text(Some(name))));
            add(
                location,
                format!("{relative}:{} uncovered function{name}", position(location)),
            );
        }
        for (id, branch) in object_entries(coverage, "branchMap")? {
            let counts = coverage
                .get("b")
                .and_then(|counts| counts.get(id))
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("branch {id} has no count array"))?;
            for (index, count) in counts.iter().enumerate() {
                if !is_zero(Some(count)) {
                    continue;
                }
                let arm = branch
                    .get("locations")
                    .and_then(|locations| locations.get(index))
                    .unwrap_or(&Value::Null);
                let location = if usable(arm) {
                    arm
                } else {
                    branch.get("loc").unwrap_or(&Value::Null)
                };
                if !usable(location) {
                    continue;
                }
                add(
                    location,
                    format!(
                        "{relative}:{} uncovered branch ({}, path {}/{})",
                        position(location),
                        js_text(branch.get("type")),
                        index + 1,
                        counts.len()
                    ),
                );
            }
        }
        items.sort_by(|left, right| {
            left.line
                .partial_cmp(&right.line)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    left.column
                        .partial_cmp(&right.column)
                        .unwrap_or(Ordering::Equal)
                })
        });
        self.records.extend(items.into_iter().map(|item| item.text));
        Ok(())
    }

    /// Returns the exact sequence of `console.log` arguments used by Istanbul.
    #[must_use]
    pub fn finish(&self) -> Vec<String> {
        if self.records.is_empty() {
            return Vec::new();
        }
        std::iter::once(format!(
            "\nUncovered locations (per-file 100% gate): {}",
            self.records.len()
        ))
        .chain(self.records.iter().cloned())
        .chain(std::iter::once(String::new()))
        .collect()
    }
}

/// Executes one source reporter lifecycle operation for its thin CJS adapter.
///
/// # Errors
///
/// Returns malformed state, unknown operation, coverage, or path diagnostics.
pub fn bridge(request: &Value) -> anyhow::Result<Value> {
    match request.get("operation").and_then(Value::as_str) {
        Some("create") => {
            let root = request
                .get("projectRoot")
                .filter(|root| truthy(root))
                .or_else(|| request.get("cwd"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("coverage reporter root must be a path string"))?;
            Ok(serde_json::to_value(UncoveredLocationsReport {
                project_root: root.to_owned(),
                records: Vec::new(),
            })?)
        }
        Some("start" | "detail" | "end") => {
            let mut report: UncoveredLocationsReport =
                serde_json::from_value(request.get("state").cloned().unwrap_or(Value::Null))?;
            match request.get("operation").and_then(Value::as_str) {
                Some("start") => report.start(),
                Some("detail") => report.detail(request.get("coverage").unwrap_or(&Value::Null))?,
                Some("end") => return Ok(serde_json::to_value(report.finish())?),
                _ => unreachable!("operation already checked"),
            }
            Ok(serde_json::to_value(report)?)
        }
        _ => anyhow::bail!("unknown coverage reporter operation"),
    }
}

fn object_entries<'a>(value: &'a Value, key: &str) -> anyhow::Result<Vec<(&'a str, &'a Value)>> {
    let map = value
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("file coverage has no {key} map"))?;
    let mut entries = map
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect::<Vec<_>>();
    entries.sort_by(
        |(left, _), (right, _)| match (array_index(left), array_index(right)) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
    );
    Ok(entries)
}

fn array_index(value: &str) -> Option<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|index| *index != u32::MAX && index.to_string() == value)
}

fn usable(location: &Value) -> bool {
    let line = number(location.pointer("/start/line"));
    line.is_finite() && line >= 1.0
}

fn is_zero(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_f64)
        .is_some_and(|value| value == 0.0)
}

fn number(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(f64::NAN),
        Some(Value::Object(object)) => {
            match object.get("$seekdeepNumber").and_then(Value::as_str) {
                Some("Infinity") => f64::INFINITY,
                Some("-Infinity") => f64::NEG_INFINITY,
                _ => f64::NAN,
            }
        }
        _ => f64::NAN,
    }
}

fn numeric_coercion(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Null) => 0.0,
        Some(Value::Bool(value)) => f64::from(u8::from(*value)),
        Some(Value::String(value)) => {
            if value.trim().is_empty() {
                0.0
            } else {
                value.trim().parse().unwrap_or(f64::NAN)
            }
        }
        _ => number(value),
    }
}

fn js_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        ryu_js::Buffer::new().format(value).to_owned()
    }
}

fn js_text(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::Null) => "null".to_owned(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(_)) => js_number(number(value)),
        Some(Value::Object(object)) if object.contains_key("$seekdeepNumber") => {
            js_number(number(value))
        }
        Some(Value::Object(_)) => "[object Object]".to_owned(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    js_text(Some(value))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(_) => {
            let number = number(Some(value));
            number != 0.0 && !number.is_nan()
        }
        Value::String(value) => !value.is_empty(),
        Value::Object(object) if object.contains_key("$seekdeepNumber") => {
            !number(Some(value)).is_nan()
        }
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn position(location: &Value) -> String {
    let column = location.pointer("/start/column");
    let column = if matches!(column, Some(Value::String(_) | Value::Array(_)))
        || column
            .is_some_and(|column| column.is_object() && column.get("$seekdeepNumber").is_none())
    {
        format!("{}1", js_text(column))
    } else {
        js_number(numeric_coercion(column) + 1.0)
    };
    format!("{}:{column}", js_text(location.pointer("/start/line")))
}

fn end_suffix(location: &Value) -> String {
    let end_line = number(location.pointer("/end/line"));
    if !end_line.is_finite() || end_line < 1.0 {
        return String::new();
    }
    let start_line = number(location.pointer("/start/line"));
    let end_column = number(location.pointer("/end/column"));
    if !end_column.is_finite() {
        return if end_line == start_line {
            String::new()
        } else {
            format!(" (to {})", js_number(end_line))
        };
    }
    if end_line == start_line && end_column == number(location.pointer("/start/column")) {
        return String::new();
    }
    format!(
        " (to {}:{})",
        js_number(end_line),
        js_number(end_column + 1.0)
    )
}

fn relative_path(from: &Path, to: &Path) -> anyhow::Result<String> {
    let from = std::path::absolute(from)?.clean();
    let to = std::path::absolute(to)?.clean();
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut result = vec!["..".to_owned(); from.len() - common];
    result.extend(
        to[common..]
            .iter()
            .map(|component| component.as_os_str().to_string_lossy().into_owned()),
    );
    Ok(result.join("/"))
}
