//! Runtime schemas for plugin configuration and generated forms.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// Machine-readable schema node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Schema {
    /// Accepts any JSON value.
    Any,
    /// Boolean value.
    Boolean,
    /// String with optional constraints.
    String {
        /// Minimum UTF-16-compatible character count.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<usize>,
        /// Maximum UTF-16-compatible character count.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<usize>,
        /// Regular expression constraint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
    },
    /// Finite JSON number.
    Number {
        /// Smallest accepted value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        /// Largest accepted value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// Restricts values to integers.
        #[serde(default)]
        integer: bool,
    },
    /// Ordered collection.
    Array {
        /// Element schema.
        inner: Box<Schema>,
    },
    /// Named fields.
    Object {
        /// Field schemas in declaration order.
        fields: IndexMap<String, Schema>,
        /// Required field names.
        #[serde(default)]
        required: Vec<String>,
    },
    /// One of several schemas.
    Union {
        /// Candidate schemas.
        variants: Vec<Schema>,
    },
    /// Exact JSON value.
    Const {
        /// Required value.
        value: Value,
    },
}

impl Schema {
    /// Validates a JSON value and returns all issues in deterministic traversal order.
    ///
    /// # Errors
    ///
    /// Returns every validation issue when the value does not satisfy this schema.
    pub fn validate(&self, value: &Value) -> Result<Value, Vec<Issue>> {
        let mut issues = Vec::new();
        self.validate_at(value, &mut Vec::new(), &mut issues);
        if issues.is_empty() {
            Ok(value.clone())
        } else {
            Err(issues)
        }
    }

    fn validate_at(&self, value: &Value, path: &mut Vec<PathSegment>, issues: &mut Vec<Issue>) {
        match self {
            Self::Boolean if !value.is_boolean() => push_issue(issues, path, "expected boolean"),
            Self::String { min, max, pattern } => {
                let Some(text) = value.as_str() else {
                    push_issue(issues, path, "expected string");
                    return;
                };
                let length = text.encode_utf16().count();
                if min.is_some_and(|minimum| length < minimum) {
                    push_issue(issues, path, "string is too short");
                }
                if max.is_some_and(|maximum| length > maximum) {
                    push_issue(issues, path, "string is too long");
                }
                if let Some(expression) = pattern
                    && regex::Regex::new(expression).is_ok_and(|regex| !regex.is_match(text))
                {
                    push_issue(issues, path, "string does not match pattern");
                }
            }
            Self::Number { min, max, integer } => {
                let Some(number) = value.as_f64() else {
                    push_issue(issues, path, "expected number");
                    return;
                };
                if *integer && number.fract() != 0.0 {
                    push_issue(issues, path, "expected integer");
                }
                if min.is_some_and(|minimum| number < minimum) {
                    push_issue(issues, path, "number is too small");
                }
                if max.is_some_and(|maximum| number > maximum) {
                    push_issue(issues, path, "number is too large");
                }
            }
            Self::Array { inner } => {
                let Some(array) = value.as_array() else {
                    push_issue(issues, path, "expected array");
                    return;
                };
                for (index, item) in array.iter().enumerate() {
                    path.push(PathSegment::Index(index));
                    inner.validate_at(item, path, issues);
                    path.pop();
                }
            }
            Self::Object { fields, required } => {
                let Some(object) = value.as_object() else {
                    push_issue(issues, path, "expected object");
                    return;
                };
                for name in required {
                    if !object.contains_key(name) {
                        path.push(PathSegment::Key(name.clone()));
                        push_issue(issues, path, "required property is missing");
                        path.pop();
                    }
                }
                for (name, schema) in fields {
                    if let Some(item) = object.get(name) {
                        path.push(PathSegment::Key(name.clone()));
                        schema.validate_at(item, path, issues);
                        path.pop();
                    }
                }
            }
            Self::Union { variants } => {
                if !variants
                    .iter()
                    .any(|variant| variant.validate(value).is_ok())
                {
                    push_issue(issues, path, "value does not match any union variant");
                }
            }
            Self::Const { value: expected } if value != expected => {
                push_issue(issues, path, "unexpected constant value");
            }
            Self::Any | Self::Boolean | Self::Const { .. } => {}
        }
    }
}

fn push_issue(issues: &mut Vec<Issue>, path: &[PathSegment], message: &str) {
    issues.push(Issue {
        message: message.to_owned(),
        path: path.to_vec(),
    });
}
