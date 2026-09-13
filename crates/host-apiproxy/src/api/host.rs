//! Host-level API contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    rpc::ContractError,
    sessions::{
        decode, is_ecmascript_whitespace, optional_string, parse_array, require_array,
        require_bool, require_field, require_literal_true, require_nonempty_string,
        require_nonnegative_integer, require_object, require_string,
    },
};

/// Empty request payload shared by Host methods with no parameters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyRequest {}

impl EmptyRequest {
    /// Parses an object and strips any unknown keys, matching `z.object({})`.
    ///
    /// # Errors
    ///
    /// Returns an error unless `value` is a JSON object.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        require_object(value, "$")?;
        Ok(Self {})
    }
}

/// `host.describe` request.
pub type HostDescribeRequest = EmptyRequest;
/// `host.pickDirectory` request.
pub type HostPickDirectoryRequest = EmptyRequest;

/// One-shot Host snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostDescribeValue {
    /// Host application version.
    pub version: String,
    /// Host process working directory.
    pub cwd: String,
    /// Optional explicit default provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional explicit default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Number of currently attached Sessions.
    pub attached_sessions: u64,
    /// Whether this deployment can hand paths to a visible desktop opener.
    pub can_open_path: bool,
}

impl HostDescribeValue {
    /// Parses a `host.describe` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for missing or malformed snapshot members.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "version", "$.version", false)?;
        require_string(object, "cwd", "$.cwd", false)?;
        optional_string(object, "provider", "$.provider", false)?;
        optional_string(object, "model", "$.model", false)?;
        require_nonnegative_integer(object, "attachedSessions", "$.attachedSessions")?;
        require_bool(object, "canOpenPath", "$.canOpenPath")?;
        decode(value)
    }
}

/// `host.pickDirectory` response; null means user cancellation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPickDirectoryValue {
    /// Selected absolute path, or null on cancellation.
    pub path: Option<String>,
}

impl HostPickDirectoryValue {
    /// Parses a `host.pickDirectory` response value.
    ///
    /// # Errors
    ///
    /// Returns an error unless the required `path` member is a string or null.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let path = require_field(object, "path", "$.path")?;
        if !path.is_null() && !path.is_string() {
            return Err(ContractError::new("$.path", "expected string or null"));
        }
        decode(value)
    }
}

/// One directory row in an entry list or breadcrumb chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    /// Display base name.
    pub name: String,
    /// Absolute Host path.
    pub path: String,
    /// Host-platform hidden marker.
    pub hidden: bool,
}

impl DirectoryEntry {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "name", "$.name", false)?;
        require_string(object, "path", "$.path", false)?;
        require_bool(object, "hidden", "$.hidden")?;
        decode(value)
    }
}

/// One directory level plus its ancestry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryListing {
    /// Listed absolute path.
    pub path: String,
    /// Host account home directory.
    pub home: String,
    /// Root-to-target ancestor chain.
    pub crumbs: Vec<DirectoryEntry>,
    /// Direct child directories.
    pub entries: Vec<DirectoryEntry>,
    /// Whether the name-sorted tail was cut at the backend bound.
    pub truncated: bool,
}

impl DirectoryListing {
    /// Parses a `host.listDirectory` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed paths, directory rows, or truncation state.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            path: require_string(object, "path", "$.path", false)?.to_owned(),
            home: require_string(object, "home", "$.home", false)?.to_owned(),
            crumbs: parse_array(
                require_array(object, "crumbs", "$.crumbs")?,
                DirectoryEntry::parse,
                "$.crumbs",
            )?,
            entries: parse_array(
                require_array(object, "entries", "$.entries")?,
                DirectoryEntry::parse,
                "$.entries",
            )?,
            truncated: require_bool(object, "truncated", "$.truncated")?,
        })
    }
}

/// `host.listDirectory` request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostListDirectoryRequest {
    /// Directory to list; absence selects Host home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl HostListDirectoryRequest {
    /// Parses a `host.listDirectory` request.
    ///
    /// # Errors
    ///
    /// Returns an error unless the payload is an object with an optional string path.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        optional_string(object, "path", "$.path", false)?;
        decode(value)
    }
}

/// `host.createDirectory` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCreateDirectoryRequest {
    /// Existing parent path.
    pub path: String,
    /// One non-blank plain segment.
    pub name: String,
}

impl HostCreateDirectoryRequest {
    /// Parses a `host.createDirectory` request and validates its segment name.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed paths or a blank, dot, dot-dot, or separator-bearing name.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "path", "$.path", false)?;
        let name = require_string(object, "name", "$.name", false)?;
        if name.trim_matches(is_ecmascript_whitespace).is_empty()
            || matches!(name, "." | "..")
            || name.contains(['/', '\\'])
        {
            return Err(ContractError::new(
                "$",
                "host.createDirectory requires a single non-blank path segment name",
            ));
        }
        decode(value)
    }
}

/// Response carrying an absolute Host path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPathValue {
    /// Absolute Host path.
    pub path: String,
}

impl HostPathValue {
    /// Parses an object with a required string path.
    ///
    /// # Errors
    ///
    /// Returns an error unless `path` is a string.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "path", "$.path", false)?;
        decode(value)
    }
}

/// `host.createDirectory` response.
pub type HostCreateDirectoryValue = HostPathValue;

/// `host.openPath` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOpenPathRequest {
    /// Non-empty path to open.
    pub path: String,
}

impl HostOpenPathRequest {
    /// Parses a `host.openPath` request.
    ///
    /// # Errors
    ///
    /// Returns an error unless `path` is a non-empty string.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "path", "$.path")?;
        decode(value)
    }
}

/// `host.openPath` response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOpenPathValue {
    /// Must be literal true.
    pub opened: bool,
}

impl HostOpenPathValue {
    /// Parses a successful native-open response.
    ///
    /// # Errors
    ///
    /// Returns an error unless `opened` is literal true.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_literal_true(object, "opened", "$.opened")?;
        Ok(Self { opened: true })
    }
}
