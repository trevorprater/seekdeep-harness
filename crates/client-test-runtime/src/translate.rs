//! Deterministic locale lookup and template interpolation for feature tests.

use std::collections::BTreeMap;

/// JavaScript-like value accepted by a translation template parameter.
#[derive(Clone, Debug, PartialEq)]
pub enum TestTranslateValue {
    /// JavaScript `undefined`.
    Undefined,
    /// JavaScript `null`.
    Null,
    /// Boolean value.
    Bool(bool),
    /// Finite numeric value.
    Number(f64),
    /// String value.
    String(String),
    /// Array using JavaScript's comma-joined string conversion.
    Array(Vec<Self>),
    /// Plain object using JavaScript's default string conversion.
    Object,
}

impl From<&str> for TestTranslateValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for TestTranslateValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<bool> for TestTranslateValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<f64> for TestTranslateValue {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl std::fmt::Display for TestTranslateValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Undefined => formatter.write_str("undefined"),
            Self::Null => formatter.write_str("null"),
            Self::Bool(value) => value.fmt(formatter),
            Self::Number(value) => formatter.write_str(ryu_js::Buffer::new().format(*value)),
            Self::String(value) => formatter.write_str(value),
            Self::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(",")?;
                    }
                    if !matches!(value, Self::Undefined | Self::Null) {
                        value.fmt(formatter)?;
                    }
                }
                Ok(())
            }
            Self::Object => formatter.write_str("[object Object]"),
        }
    }
}

/// Ordered dictionary chain mirroring the Client locale lookup used by feature tests.
#[derive(Clone, Debug, Default)]
pub struct TestTranslator {
    dictionaries: Vec<BTreeMap<String, String>>,
}

impl TestTranslator {
    /// Constructs a first-match-wins lookup chain.
    #[must_use]
    pub fn new(dictionaries: impl IntoIterator<Item = BTreeMap<String, String>>) -> Self {
        Self {
            dictionaries: dictionaries.into_iter().collect(),
        }
    }

    /// Resolves one key and replaces `{name}` placeholders present in `params`.
    #[must_use]
    pub fn translate(
        &self,
        key: &str,
        params: Option<&BTreeMap<String, TestTranslateValue>>,
    ) -> String {
        let template = self
            .dictionaries
            .iter()
            .find_map(|dictionary| dictionary.get(key))
            .map_or(key, String::as_str);
        let Some(params) = params else {
            return template.to_owned();
        };
        interpolate(template, params)
    }
}

fn interpolate(template: &str, params: &BTreeMap<String, TestTranslateValue>) -> String {
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0;
    while let Some(open_offset) = template[cursor..].find('{') {
        let open = cursor + open_offset;
        output.push_str(&template[cursor..open]);
        let Some(close_offset) = template[open + 1..].find('}') else {
            output.push_str(&template[open..]);
            return output;
        };
        let close = open + 1 + close_offset;
        let name = &template[open + 1..close];
        let replaceable = !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if replaceable && let Some(value) = params.get(name) {
            output.push_str(&value.to_string());
        } else {
            output.push_str(&template[open..=close]);
        }
        cursor = close + 1;
    }
    output.push_str(&template[cursor..]);
    output
}
