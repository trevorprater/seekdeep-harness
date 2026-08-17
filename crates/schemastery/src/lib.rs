//! Runtime schemas for plugin configuration and generated forms.
//!
//! Schema nodes follow the bundled Schemastery runtime: they have process-local
//! identities, normalize defaults, keep undeclared object properties, and
//! serialize as a `{ uid, refs }` graph.

use std::{
    collections::HashSet,
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;

static NEXT_UID: AtomicU64 = AtomicU64::new(0);

/// A path component identifying an invalid nested value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathSegment {
    /// Object property.
    Key(String),
    /// Array index.
    Index(usize),
}

/// One validation problem.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// Human-readable diagnostic.
    pub message: String,
    /// Location inside the submitted value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<PathSegment>,
}

/// First validation problem, rendered with Schemastery's `$`-rooted path.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ValidationError {
    /// Rendered source-compatible diagnostic.
    pub message: String,
    /// Location inside the submitted value.
    pub path: Vec<PathSegment>,
}

impl ValidationError {
    fn new(path: &[PathSegment], detail: impl Into<String>) -> Self {
        let detail = detail.into();
        let message = if path.is_empty() {
            detail
        } else {
            format!("{} {detail}", render_path(path))
        };
        Self {
            message,
            path: path.to_vec(),
        }
    }

    /// Converts this failure to the Standard-Schema-like issue shape.
    #[must_use]
    pub fn issue(&self) -> Issue {
        Issue {
            message: self.message.clone(),
            path: self.path.clone(),
        }
    }
}

fn render_path(path: &[PathSegment]) -> String {
    let mut output = "$".to_owned();
    for segment in path {
        match segment {
            PathSegment::Key(key) => {
                output.push('.');
                output.push_str(key);
            }
            PathSegment::Index(index) => write!(output, "[{index}]").expect("writing to String"),
        }
    }
    output
}

/// UI and validation metadata attached to a schema node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SchemaMeta {
    /// Value substituted for a missing or null input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Whether missing and null inputs are rejected.
    #[serde(skip_serializing_if = "is_false")]
    pub required: bool,
    /// Renderer role such as `secret` or `credential-ref`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Role-specific renderer metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    /// Inclusive maximum for numbers or collection lengths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Inclusive minimum for numbers or collection lengths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Required numeric increment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// Regular expression source and flags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<Pattern>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// Serialized regular-expression metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pattern {
    /// Expression source.
    pub source: String,
    /// JavaScript flags; configuration schemas use empty or `i`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub flags: String,
}

/// Structural kind of one schema node.
#[derive(Clone, Debug, PartialEq)]
pub enum SchemaKind {
    /// Accept any JSON value.
    Any,
    /// Accept no non-null value.
    Never,
    /// Accept one exact JSON value.
    Const(Value),
    /// Accept a string.
    String,
    /// Accept a finite JSON number.
    Number,
    /// Accept a boolean.
    Boolean,
    /// Accept an ordered collection.
    Array(Schema),
    /// Accept an object whose values share one schema.
    Dict {
        /// Value schema.
        inner: Schema,
        /// Key schema.
        key: Schema,
    },
    /// Accept a fixed schema prefix followed by untouched extra values.
    Tuple(Vec<Schema>),
    /// Accept an open object with declared properties.
    Object(IndexMap<String, Schema>),
    /// Accept the first matching schema.
    Union(Vec<Schema>),
    /// Accept every schema and merge object outputs.
    Intersect(Vec<Schema>),
}

#[derive(Debug, PartialEq)]
struct SchemaNode {
    uid: u64,
    kind: SchemaKind,
    meta: SchemaMeta,
}

/// Cloneable runtime schema node.
#[derive(Clone, Debug, PartialEq)]
pub struct Schema(Arc<SchemaNode>);

impl Schema {
    fn new(kind: SchemaKind, meta: SchemaMeta) -> Self {
        Self(Arc::new(SchemaNode {
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
            kind,
            meta,
        }))
    }

    fn defaulted(kind: SchemaKind, default: Value) -> Self {
        Self::new(
            kind,
            SchemaMeta {
                default: Some(default),
                ..SchemaMeta::default()
            },
        )
    }

    /// Accepts any JSON value.
    #[must_use]
    pub fn any() -> Self {
        Self::new(SchemaKind::Any, SchemaMeta::default())
    }

    /// Accepts only missing or null input.
    #[must_use]
    pub fn never() -> Self {
        Self::new(SchemaKind::Never, SchemaMeta::default())
    }

    /// Accepts exactly one JSON value.
    #[must_use]
    pub fn constant(value: impl Into<Value>) -> Self {
        Self::new(SchemaKind::Const(value.into()), SchemaMeta::default()).required()
    }

    /// Accepts strings.
    #[must_use]
    pub fn string() -> Self {
        Self::new(SchemaKind::String, SchemaMeta::default())
    }

    /// Accepts finite numbers.
    #[must_use]
    pub fn number() -> Self {
        Self::new(SchemaKind::Number, SchemaMeta::default())
    }

    /// Accepts booleans.
    #[must_use]
    pub fn boolean() -> Self {
        Self::new(SchemaKind::Boolean, SchemaMeta::default())
    }

    /// Accepts arrays whose entries match `inner`.
    #[must_use]
    pub fn array(inner: Self) -> Self {
        Self::defaulted(SchemaKind::Array(inner), json!([]))
    }

    /// Accepts objects whose values match `inner` and keys match strings.
    #[must_use]
    pub fn dict(inner: Self) -> Self {
        Self::dict_with_keys(inner, Self::string())
    }

    /// Accepts objects whose values and property names match their schemas.
    #[must_use]
    pub fn dict_with_keys(inner: Self, key: Self) -> Self {
        Self::defaulted(SchemaKind::Dict { inner, key }, json!({}))
    }

    /// Accepts tuple arrays, preserving entries beyond the declared prefix.
    #[must_use]
    pub fn tuple(items: impl IntoIterator<Item = Self>) -> Self {
        Self::defaulted(SchemaKind::Tuple(items.into_iter().collect()), json!([]))
    }

    /// Accepts an open object with declared property schemas.
    #[must_use]
    pub fn object(fields: impl IntoIterator<Item = (impl Into<String>, Self)>) -> Self {
        Self::defaulted(
            SchemaKind::Object(
                fields
                    .into_iter()
                    .map(|(name, schema)| (name.into(), schema))
                    .collect(),
            ),
            json!({}),
        )
    }

    /// Accepts the first matching variant.
    #[must_use]
    pub fn union(items: impl IntoIterator<Item = Self>) -> Self {
        Self::new(
            SchemaKind::Union(items.into_iter().collect()),
            SchemaMeta::default(),
        )
    }

    /// Accepts every variant and merges object outputs.
    #[must_use]
    pub fn intersect(items: impl IntoIterator<Item = Self>) -> Self {
        Self::new(
            SchemaKind::Intersect(items.into_iter().collect()),
            SchemaMeta::default(),
        )
    }

    /// Marks missing and null input invalid.
    #[must_use]
    pub fn required(self) -> Self {
        self.with_meta(|meta| meta.required = true)
    }

    /// Sets the fallback for missing and null input.
    #[must_use]
    pub fn with_default(self, value: impl Into<Value>) -> Self {
        let value = value.into();
        self.with_meta(|meta| meta.default = Some(value))
    }

    /// Attaches a renderer role.
    #[must_use]
    pub fn role(self, role: impl Into<String>) -> Self {
        let role = role.into();
        self.with_meta(|meta| meta.role = Some(role))
    }

    /// Attaches a renderer role and role-specific metadata.
    #[must_use]
    pub fn role_with_extra(self, role: impl Into<String>, extra: Value) -> Self {
        let role = role.into();
        self.with_meta(|meta| {
            meta.role = Some(role);
            meta.extra = Some(extra);
        })
    }

    /// Sets an inclusive minimum.
    #[must_use]
    pub fn min(self, value: f64) -> Self {
        self.with_meta(|meta| meta.min = Some(value))
    }

    /// Sets an inclusive maximum.
    #[must_use]
    pub fn max(self, value: f64) -> Self {
        self.with_meta(|meta| meta.max = Some(value))
    }

    /// Sets the numeric increment constraint.
    #[must_use]
    pub fn step(self, value: f64) -> Self {
        self.with_meta(|meta| meta.step = Some(value))
    }

    /// Sets a regular-expression constraint.
    #[must_use]
    pub fn pattern(self, source: impl Into<String>, flags: impl Into<String>) -> Self {
        let pattern = Pattern {
            source: source.into(),
            flags: flags.into(),
        };
        self.with_meta(|meta| meta.pattern = Some(pattern))
    }

    fn with_meta(self, update: impl FnOnce(&mut SchemaMeta)) -> Self {
        let mut meta = self.0.meta.clone();
        update(&mut meta);
        Self::new(self.0.kind.clone(), meta)
    }

    /// Process-local node identity used by the canonical wire graph.
    #[must_use]
    pub fn uid(&self) -> u64 {
        self.0.uid
    }

    /// Structural kind.
    #[must_use]
    pub fn kind(&self) -> &SchemaKind {
        &self.0.kind
    }

    /// Validation and renderer metadata.
    #[must_use]
    pub fn meta(&self) -> &SchemaMeta {
        &self.0.meta
    }

    /// Validates and normalizes one JSON value.
    ///
    /// # Errors
    ///
    /// Returns the first source-ordered validation failure.
    pub fn resolve(&self, value: &Value) -> Result<Value, ValidationError> {
        self.resolve_optional(Some(value), &mut Vec::new(), false)
    }

    /// Resolves an absent value, applying this node's default when present.
    ///
    /// # Errors
    ///
    /// Returns the first source-ordered validation failure.
    pub fn resolve_missing(&self) -> Result<Value, ValidationError> {
        self.resolve_optional(None, &mut Vec::new(), false)
    }

    /// Standard-Schema-like validation collecting the first issue.
    ///
    /// # Errors
    ///
    /// Returns an issue vector on failure.
    pub fn validate(&self, value: &Value) -> Result<Value, Vec<Issue>> {
        self.resolve(value).map_err(|error| vec![error.issue()])
    }

    fn resolve_optional(
        &self,
        value: Option<&Value>,
        path: &mut Vec<PathSegment>,
        strict: bool,
    ) -> Result<Value, ValidationError> {
        let fallback;
        let value = if value.is_none_or(Value::is_null) {
            if self.0.meta.required {
                return Err(ValidationError::new(path, "missing required value"));
            }
            let Some(default) = &self.0.meta.default else {
                return Ok(Value::Null);
            };
            fallback = default.clone();
            &fallback
        } else {
            value.expect("non-null value exists")
        };

        match &self.0.kind {
            SchemaKind::Any => Ok(value.clone()),
            SchemaKind::Never => Err(ValidationError::new(
                path,
                format!("expected nullable but got {}", display_value(value)),
            )),
            SchemaKind::Const(expected) if value == expected => Ok(expected.clone()),
            SchemaKind::Const(expected) => Err(ValidationError::new(
                path,
                format!(
                    "expected {} but got {}",
                    display_value(expected),
                    display_value(value)
                ),
            )),
            SchemaKind::String => self.resolve_string(value, path),
            SchemaKind::Number => self.resolve_number(value, path),
            SchemaKind::Boolean => value.as_bool().map(Value::Bool).ok_or_else(|| {
                ValidationError::new(
                    path,
                    format!("expected boolean but got {}", display_value(value)),
                )
            }),
            SchemaKind::Array(inner) => self.resolve_array(value, inner, path),
            SchemaKind::Dict { inner, key } => Self::resolve_dict(value, inner, key, path, strict),
            SchemaKind::Tuple(items) => Self::resolve_tuple(value, items, path, strict),
            SchemaKind::Object(fields) => Self::resolve_object(value, fields, path, strict),
            SchemaKind::Union(items) => {
                for item in items {
                    if let Ok(output) = item.resolve_optional(Some(value), path, strict) {
                        return Ok(output);
                    }
                }
                Err(ValidationError::new(
                    path,
                    format!(
                        "value does not match any union variant: {}",
                        display_value(value)
                    ),
                ))
            }
            SchemaKind::Intersect(items) => {
                let mut output: Option<Value> = None;
                for item in items {
                    let next = item.resolve_optional(Some(value), path, true)?;
                    output = Some(match (output, next) {
                        (None, next) => next,
                        (Some(Value::Object(mut left)), Value::Object(right)) => {
                            for (key, value) in right {
                                left.entry(key).or_insert(value);
                            }
                            Value::Object(left)
                        }
                        (Some(left), right) if left == right => left,
                        _ => {
                            return Err(ValidationError::new(
                                path,
                                format!(
                                    "intersection values disagree for {}",
                                    display_value(value)
                                ),
                            ));
                        }
                    });
                }
                let mut output = output.unwrap_or_else(|| value.clone());
                if !strict
                    && let (Some(source), Some(target)) =
                        (value.as_object(), output.as_object_mut())
                {
                    for (key, value) in source {
                        target.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
                Ok(output)
            }
        }
    }

    fn resolve_string(
        &self,
        value: &Value,
        path: &[PathSegment],
    ) -> Result<Value, ValidationError> {
        let Some(text) = value.as_str() else {
            return Err(ValidationError::new(
                path,
                format!("expected string but got {}", display_value(value)),
            ));
        };
        check_range(
            length_as_f64(text.encode_utf16().count()),
            &self.0.meta,
            "string length",
            path,
        )?;
        if let Some(pattern) = &self.0.meta.pattern {
            let source = if pattern.flags.contains('i') {
                format!("(?i:{})", pattern.source)
            } else {
                pattern.source.clone()
            };
            let regex = regex::Regex::new(&source)
                .map_err(|error| ValidationError::new(path, format!("invalid regexp: {error}")))?;
            if !regex.is_match(text) {
                return Err(ValidationError::new(
                    path,
                    format!(
                        "expect string to match regexp /{}/{}",
                        pattern.source, pattern.flags
                    ),
                ));
            }
        }
        Ok(Value::String(text.to_owned()))
    }

    fn resolve_number(
        &self,
        value: &Value,
        path: &[PathSegment],
    ) -> Result<Value, ValidationError> {
        let Some(number) = value.as_f64() else {
            return Err(ValidationError::new(
                path,
                format!("expected number but got {}", display_value(value)),
            ));
        };
        check_range(number, &self.0.meta, "number", path)?;
        if let Some(step) = self.0.meta.step
            && step != 0.0
        {
            let quotient = (number - self.0.meta.min.unwrap_or(0.0)) / step.abs();
            if (quotient - quotient.round()).abs() > 1e-9 {
                return Err(ValidationError::new(
                    path,
                    format!("expected number multiple of {step} but got {number}"),
                ));
            }
        }
        Ok(value.clone())
    }

    fn resolve_array(
        &self,
        value: &Value,
        inner: &Self,
        path: &mut Vec<PathSegment>,
    ) -> Result<Value, ValidationError> {
        let Some(items) = value.as_array() else {
            return Err(ValidationError::new(
                path,
                format!("expected array but got {}", display_value(value)),
            ));
        };
        check_range(
            length_as_f64(items.len()),
            &self.0.meta,
            "array length",
            path,
        )?;
        let mut output = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            path.push(PathSegment::Index(index));
            let resolved = inner.resolve_optional(Some(item), path, false);
            path.pop();
            output.push(resolved?);
        }
        Ok(Value::Array(output))
    }

    fn resolve_dict(
        value: &Value,
        inner: &Self,
        key_schema: &Self,
        path: &mut Vec<PathSegment>,
        strict: bool,
    ) -> Result<Value, ValidationError> {
        let Some(object) = value.as_object() else {
            return Err(ValidationError::new(
                path,
                format!("expected object but got {}", display_value(value)),
            ));
        };
        let mut output = Map::new();
        for (key, item) in object {
            let normalized_key = match key_schema.resolve(&Value::String(key.clone())) {
                Ok(Value::String(key)) => key,
                Ok(_) => key.clone(),
                Err(_) if strict => continue,
                Err(error) => return Err(error),
            };
            path.push(PathSegment::Key(key.clone()));
            let resolved = inner.resolve_optional(Some(item), path, false);
            path.pop();
            output.insert(normalized_key, resolved?);
        }
        Ok(Value::Object(output))
    }

    fn resolve_tuple(
        value: &Value,
        schemas: &[Self],
        path: &mut Vec<PathSegment>,
        strict: bool,
    ) -> Result<Value, ValidationError> {
        let Some(items) = value.as_array() else {
            return Err(ValidationError::new(
                path,
                format!("expected array but got {}", display_value(value)),
            ));
        };
        let mut output = Vec::with_capacity(items.len().max(schemas.len()));
        for (index, schema) in schemas.iter().enumerate() {
            path.push(PathSegment::Index(index));
            let resolved = schema.resolve_optional(items.get(index), path, false);
            path.pop();
            output.push(resolved?);
        }
        if !strict {
            output.extend(items.iter().skip(schemas.len()).cloned());
        }
        Ok(Value::Array(output))
    }

    fn resolve_object(
        value: &Value,
        fields: &IndexMap<String, Self>,
        path: &mut Vec<PathSegment>,
        strict: bool,
    ) -> Result<Value, ValidationError> {
        let Some(object) = value.as_object() else {
            return Err(ValidationError::new(
                path,
                format!("expected object but got {}", display_value(value)),
            ));
        };
        let mut output = Map::new();
        for (key, schema) in fields {
            path.push(PathSegment::Key(key.clone()));
            let resolved = schema.resolve_optional(object.get(key), path, false);
            path.pop();
            let resolved = resolved?;
            if !resolved.is_null() || object.contains_key(key) {
                output.insert(key.clone(), resolved);
            }
        }
        if !strict {
            for (key, value) in object {
                output.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        Ok(Value::Object(output))
    }

    /// Serializes this graph using Schemastery's canonical `{ uid, refs }`
    /// envelope and numeric relation references.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut refs = Map::new();
        self.collect_wire(&mut refs, &mut HashSet::new());
        json!({ "uid": self.uid(), "refs": refs })
    }

    fn collect_wire(&self, refs: &mut Map<String, Value>, visited: &mut HashSet<u64>) {
        if !visited.insert(self.uid()) {
            return;
        }
        let mut node = Map::from_iter([
            (
                "type".to_owned(),
                Value::String(self.type_name().to_owned()),
            ),
            (
                "meta".to_owned(),
                serde_json::to_value(&self.0.meta).expect("schema metadata is JSON"),
            ),
        ]);
        match &self.0.kind {
            SchemaKind::Const(value) => {
                node.insert("value".to_owned(), value.clone());
            }
            SchemaKind::Array(inner) => {
                node.insert("inner".to_owned(), Value::from(inner.uid()));
                inner.collect_wire(refs, visited);
            }
            SchemaKind::Dict { inner, key } => {
                node.insert("inner".to_owned(), Value::from(inner.uid()));
                node.insert("sKey".to_owned(), Value::from(key.uid()));
                inner.collect_wire(refs, visited);
                key.collect_wire(refs, visited);
            }
            SchemaKind::Tuple(items) | SchemaKind::Union(items) | SchemaKind::Intersect(items) => {
                node.insert(
                    "list".to_owned(),
                    Value::Array(items.iter().map(|item| Value::from(item.uid())).collect()),
                );
                for item in items {
                    item.collect_wire(refs, visited);
                }
            }
            SchemaKind::Object(fields) => {
                node.insert(
                    "dict".to_owned(),
                    Value::Object(
                        fields
                            .iter()
                            .map(|(name, schema)| (name.clone(), Value::from(schema.uid())))
                            .collect(),
                    ),
                );
                for schema in fields.values() {
                    schema.collect_wire(refs, visited);
                }
            }
            SchemaKind::Any
            | SchemaKind::Never
            | SchemaKind::String
            | SchemaKind::Number
            | SchemaKind::Boolean => {}
        }
        refs.insert(self.uid().to_string(), Value::Object(node));
    }

    fn type_name(&self) -> &'static str {
        match self.0.kind {
            SchemaKind::Any => "any",
            SchemaKind::Never => "never",
            SchemaKind::Const(_) => "const",
            SchemaKind::String => "string",
            SchemaKind::Number => "number",
            SchemaKind::Boolean => "boolean",
            SchemaKind::Array(_) => "array",
            SchemaKind::Dict { .. } => "dict",
            SchemaKind::Tuple(_) => "tuple",
            SchemaKind::Object(_) => "object",
            SchemaKind::Union(_) => "union",
            SchemaKind::Intersect(_) => "intersect",
        }
    }
}

fn check_range(
    value: f64,
    meta: &SchemaMeta,
    description: &str,
    path: &[PathSegment],
) -> Result<(), ValidationError> {
    if let Some(max) = meta.max
        && value > max
    {
        return Err(ValidationError::new(
            path,
            format!("expected {description} <= {max} but got {value}"),
        ));
    }
    if let Some(min) = meta.min
        && value < min
    {
        return Err(ValidationError::new(
            path,
            format!("expected {description} >= {min} but got {value}"),
        ));
    }
    Ok(())
}

fn length_as_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::INFINITY, f64::from)
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_open_objects_and_nested_paths_follow_runtime_semantics() {
        let schema = Schema::object([
            (
                "theme",
                Schema::union([Schema::constant("dark"), Schema::constant("light")])
                    .with_default("dark"),
            ),
            ("fontSize", Schema::number().with_default(14)),
        ]);
        assert_eq!(
            schema.resolve(&json!({ "theme": "light", "extra": true })),
            Ok(json!({ "theme": "light", "fontSize": 14, "extra": true }))
        );
        assert_eq!(
            schema
                .resolve(&json!({ "fontSize": "big" }))
                .unwrap_err()
                .message,
            "$.fontSize expected number but got big"
        );
    }

    #[test]
    fn canonical_wire_graph_carries_roles_and_relations() {
        let schema = Schema::object([
            ("apiKey", Schema::string().role("secret")),
            ("models", Schema::array(Schema::string())),
        ]);
        let wire = schema.to_json();
        let uid = wire["uid"].as_u64().unwrap();
        assert_eq!(wire["refs"][uid.to_string()]["type"], "object");
        let api_uid = wire["refs"][uid.to_string()]["dict"]["apiKey"]
            .as_u64()
            .unwrap();
        assert_eq!(wire["refs"][api_uid.to_string()]["meta"]["role"], "secret");
    }

    #[test]
    fn dict_array_tuple_and_numeric_constraints_normalize() {
        let schema = Schema::object([
            ("values", Schema::dict(Schema::number().min(1.0).step(1.0))),
            (
                "tuple",
                Schema::tuple([Schema::string().required(), Schema::boolean()]),
            ),
        ]);
        assert_eq!(
            schema.resolve(&json!({
                "values": { "a": 2 },
                "tuple": ["x", true, "kept"]
            })),
            Ok(json!({
                "values": { "a": 2 },
                "tuple": ["x", true, "kept"]
            }))
        );
        assert!(schema.resolve(&json!({ "values": { "a": 1.5 } })).is_err());
    }
}
