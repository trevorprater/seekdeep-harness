//! Binary conversion and deep JSON value helpers.

use base64::{Engine as _, engine::general_purpose};
use serde_json::Value;

/// Binary source conversion helpers.
pub mod binary {
    use super::*;

    /// Encodes bytes as canonical padded base64.
    #[must_use]
    pub fn to_base64(source: &[u8]) -> String {
        general_purpose::STANDARD.encode(source)
    }

    /// Decodes standard base64, accepting the unpadded form accepted by Node.js buffers.
    ///
    /// # Errors
    ///
    /// Returns a decoder error when the input cannot represent base64 bytes.
    pub fn from_base64(source: &str) -> Result<Vec<u8>, base64::DecodeError> {
        general_purpose::STANDARD
            .decode(source)
            .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(source))
    }

    /// Encodes bytes as lowercase hexadecimal.
    #[must_use]
    pub fn to_hex(source: &[u8]) -> String {
        let mut output = String::with_capacity(source.len() * 2);
        for byte in source {
            use std::fmt::Write as _;
            write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
        }
        output
    }

    /// Decodes complete hexadecimal pairs and ignores one trailing nibble.
    ///
    /// # Errors
    ///
    /// Returns the first invalid complete pair.
    pub fn from_hex(source: &str) -> Result<Vec<u8>, std::num::ParseIntError> {
        let complete_length = source.len() - source.len() % 2;
        (0..complete_length)
            .step_by(2)
            .map(|index| u8::from_str_radix(&source[index..index + 2], 16))
            .collect()
    }
}

/// Deep-clones a JSON value.
#[must_use]
pub fn clone_json(source: &Value) -> Value {
    source.clone()
}

/// Deeply compares JSON values, optionally treating absent and null as equal.
#[must_use]
pub fn deep_equal_json(left: Option<&Value>, right: Option<&Value>, strict: bool) -> bool {
    if !strict && left.is_none_or(Value::is_null) && right.is_none_or(Value::is_null) {
        return true;
    }
    left == right
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn binary_round_trips_and_truncates_odd_hex() {
        let bytes = [0, 1, 254, 255];
        assert_eq!(
            binary::from_base64(&binary::to_base64(&bytes)),
            Ok(bytes.to_vec())
        );
        assert_eq!(binary::to_hex(&bytes), "0001feff");
        assert_eq!(binary::from_hex("0001f"), Ok(vec![0, 1]));
    }

    #[test]
    fn non_strict_equality_treats_absent_and_null_as_equal() {
        assert!(deep_equal_json(None, Some(&Value::Null), false));
        assert!(!deep_equal_json(None, Some(&Value::Null), true));
        assert!(deep_equal_json(
            Some(&json!([1, {"a": 2}])),
            Some(&json!([1, {"a": 2}])),
            true
        ));
    }
}
