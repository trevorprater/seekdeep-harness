//! Filesystem vocabulary: opaque identities, metadata, write/edit shapes, and
//! the typed error taxonomy.

use serde::{Deserialize, Serialize};
use thiserror::Error;

seekdeep_util::string_brand!(
    /// Opaque key for stale guards and target lookup.
    pub struct FsTargetKey;
);

seekdeep_util::string_brand!(
    /// Opaque file-version token guarding writes and edits.
    pub struct FsVersion;
);

/// One authoritative observation of a target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum FsObservation {
    /// Present with its freshness version.
    Present {
        /// Freshness token.
        version: FsVersion,
    },
    /// Confirmed absent.
    Absent,
}

/// A path resolved by a backend into a stable identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsTarget {
    /// Opaque key for stale guards and target lookup.
    pub target_key: FsTargetKey,
    /// Path for model/UI-facing output.
    pub display_path: String,
}

/// Regular-file vs directory vs other classification for a resolved target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Something else.
    Other,
}

/// Metadata about a target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsInfo {
    /// Opaque freshness token of the target right now.
    pub version: FsVersion,
    /// Whether the target is a regular file, a directory, or something else.
    #[serde(rename = "type")]
    pub kind: FsKind,
    /// Byte size of a regular file, when the backend can report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Path-entry classification that can additionally report a symbolic link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsPathKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
    /// Something else.
    Other,
}

/// Metadata about a path without following a trailing symbolic link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsPathInfo {
    /// Opaque freshness token of the path entry right now.
    pub version: FsVersion,
    /// Whether the path entry is a regular file, directory, symlink, or other.
    #[serde(rename = "type")]
    pub kind: FsPathKind,
    /// Byte size of the path entry, when the backend can report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// One direct child returned by a directory listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsDirEntry {
    /// Basename of the child inside the listed directory.
    pub name: String,
    /// Whether the child is a regular file, a directory, or something else.
    #[serde(rename = "type")]
    pub kind: FsKind,
    /// Resolved child target for follow-up operations.
    pub target: FsTarget,
    /// Opaque freshness token when the backend can report metadata cheaply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<FsVersion>,
    /// Byte size of a regular file, when the backend can report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Guarded write intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FsWriteIntent {
    /// Reject an existing target.
    CreateIfAbsent,
    /// Reject absence or version mismatch.
    ReplaceIfVersion {
        /// Expected freshness token.
        version: FsVersion,
    },
}

/// Outcome of a full-file write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsWriteOutcome {
    /// Whether the write created or replaced.
    pub operation: FsWriteOperation,
    /// Opaque version of the file after the write.
    pub version: FsVersion,
    /// Content before the write, or null when unavailable.
    pub before: Option<String>,
    /// Content after the write, LF-normalized.
    pub after: String,
}

/// Whether a full-file write created or replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsWriteOperation {
    /// A new file.
    Create,
    /// An existing file replaced.
    Update,
}

/// A literal-replacement edit request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEditRequest {
    /// Literal non-empty text to replace.
    pub old_string: String,
    /// Literal replacement text; empty deletes the match.
    pub new_string: String,
    /// Replace every match instead of requiring exactly one.
    pub replace_all: bool,
}

/// Outcome of a literal edit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEditOutcome {
    /// Opaque version of the file after the edit.
    pub version: FsVersion,
    /// Content before the edit, LF-normalized.
    pub before: String,
    /// Content after the edit.
    pub after: String,
}

/// Stable, machine-routable codes for filesystem failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FsErrorCode {
    /// Target does not exist.
    FsNotFound,
    /// A directory was required but the target is not one.
    FsNotDirectory,
    /// Content is not decodable text.
    FsNotText,
    /// A regular file was required but the target is not one.
    FsNotRegularFile,
    /// Content exceeds the configured or requested bound.
    FsTooLarge,
    /// Permission denied.
    FsPermissionDenied,
    /// Sandbox policy denied the effect.
    FsSandboxDenied,
    /// Ordinary I/O failure.
    FsIoError,
    /// The freshness token no longer matches.
    FsStaleVersion,
    /// The target was expected absent or observed but is not.
    FsNotObserved,
    /// The literal match is ambiguous without replaceAll.
    FsAmbiguousEdit,
    /// The literal match was not found.
    FsEditNotFound,
    /// The operation was aborted.
    FsAborted,
}

impl FsErrorCode {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FsNotFound => "FS_NOT_FOUND",
            Self::FsNotDirectory => "FS_NOT_DIRECTORY",
            Self::FsNotText => "FS_NOT_TEXT",
            Self::FsNotRegularFile => "FS_NOT_REGULAR_FILE",
            Self::FsTooLarge => "FS_TOO_LARGE",
            Self::FsPermissionDenied => "FS_PERMISSION_DENIED",
            Self::FsSandboxDenied => "FS_SANDBOX_DENIED",
            Self::FsIoError => "FS_IO_ERROR",
            Self::FsStaleVersion => "FS_STALE_VERSION",
            Self::FsNotObserved => "FS_NOT_OBSERVED",
            Self::FsAmbiguousEdit => "FS_AMBIGUOUS_EDIT",
            Self::FsEditNotFound => "FS_EDIT_NOT_FOUND",
            Self::FsAborted => "FS_ABORTED",
        }
    }
}

/// Typed filesystem error carrying a stable `FsErrorCode`.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct FsError {
    /// Stable routing code.
    pub code: FsErrorCode,
    /// Human-readable diagnostic.
    pub message: String,
    #[source]
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl FsError {
    /// Creates one typed filesystem failure.
    #[must_use]
    pub fn new(message: impl Into<String>, code: FsErrorCode) -> Self {
        Self {
            code,
            message: message.into(),
            cause: None,
        }
    }

    /// Chains the provider or platform failure that caused this filesystem error.
    #[must_use]
    pub fn with_cause(mut self, cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }

    /// JavaScript error-class name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        "FsError"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_serialize_to_stable_wire_spelling() {
        assert_eq!(
            serde_json::to_string(&FsErrorCode::FsStaleVersion).expect("code"),
            "\"FS_STALE_VERSION\""
        );
        assert_eq!(FsErrorCode::FsNotFound.as_str(), "FS_NOT_FOUND");
    }

    #[test]
    fn observations_and_intents_round_trip() {
        let present = FsObservation::Present {
            version: FsVersion::new("v1"),
        };
        assert_eq!(
            serde_json::to_value(&present).expect("present"),
            serde_json::json!({"kind": "present", "version": "v1"})
        );
        let intent = FsWriteIntent::ReplaceIfVersion {
            version: FsVersion::new("v1"),
        };
        assert_eq!(
            serde_json::to_value(&intent).expect("intent"),
            serde_json::json!({"kind": "replaceIfVersion", "version": "v1"})
        );
    }
}
