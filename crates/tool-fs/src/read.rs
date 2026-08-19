//! Model-facing UTF-8 read tool: validation and caps.

use serde::{Deserialize, Serialize};

/// Default and maximum number of lines returned by one read call.
pub const READ_LIMIT: u64 = 2000;

/// Default streaming threshold in bytes.
pub const STREAM_MIN_SIZE: u64 = 10 * 1024 * 1024;

/// Resolved read-tool caps — plugin config after defaulting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolCaps {
    /// Default and maximum number of lines returned by one call.
    pub limit: u64,
    /// Maximum characters returned for a single line.
    pub max_line_length: usize,
    /// Maximum bytes returned for selected file lines.
    pub max_bytes: usize,
    /// Files at or above this size stream.
    pub stream_min_size: u64,
}

/// Raw schema-validated read arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadArgsRaw {
    /// Path to read.
    pub file_path: String,
    /// 1-based first line to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
    /// Maximum number of lines to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

/// Validated read arguments after defaulting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadInput {
    /// Path to read.
    pub file_path: String,
    /// 1-based first line to return.
    pub offset: u64,
    /// Maximum number of lines to return.
    pub limit: u64,
}

fn parse_positive_integer(value: f64, name: &str) -> anyhow::Result<u64> {
    if !value.is_finite() || value.fract() != 0.0 || value < 1.0 {
        anyhow::bail!("{name} must be a positive integer");
    }
    u64::try_from(value).map_err(|_| anyhow::anyhow!("{name} must be a positive integer"))
}

/// Validates value constraints the schema cannot express and applies defaults.
///
/// # Errors
///
/// Returns a blank-path, non-integer, non-positive, or over-limit failure.
pub fn parse_read_args(args: &ReadArgsRaw, max_limit: u64) -> anyhow::Result<ReadInput> {
    if args.file_path.trim().is_empty() {
        anyhow::bail!("file_path must be a non-empty string");
    }
    let offset = args
        .offset
        .map_or(Ok(1), |value| parse_positive_integer(value, "offset"))?;
    let limit = args.limit.map_or(Ok(max_limit), |value| {
        parse_positive_integer(value, "limit")
    })?;
    if limit > max_limit {
        anyhow::bail!("limit must be less than or equal to {max_limit}");
    }
    Ok(ReadInput {
        file_path: args.file_path.clone(),
        offset,
        limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(file_path: &str, offset: Option<f64>, limit: Option<f64>) -> ReadArgsRaw {
        ReadArgsRaw {
            file_path: file_path.to_owned(),
            offset,
            limit,
        }
    }

    #[test]
    fn defaults_offset_and_limit() {
        let input = parse_read_args(&raw("a.txt", None, None), 100).expect("defaults");
        assert_eq!(input.offset, 1);
        assert_eq!(input.limit, 100);
    }

    #[test]
    fn rejects_blank_path_and_invalid_numbers() {
        assert!(parse_read_args(&raw("  ", None, None), 100).is_err());
        assert!(parse_read_args(&raw("a", Some(0.0), None), 100).is_err());
        assert!(parse_read_args(&raw("a", Some(1.5), None), 100).is_err());
        assert!(parse_read_args(&raw("a", None, Some(101.0)), 100).is_err());
    }

    #[test]
    fn accepts_valid_explicit_values() {
        let input = parse_read_args(&raw("a", Some(5.0), Some(10.0)), 100).expect("valid");
        assert_eq!(input.offset, 5);
        assert_eq!(input.limit, 10);
    }
}
