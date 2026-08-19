//! Model-facing full-file write: validation and output formatting.

use seekdeep_fs::FsWriteOperation;
use serde::{Deserialize, Serialize};

/// Raw schema-validated write arguments, including the escalation fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteArgsRaw {
    /// Path to write.
    pub file_path: String,
    /// Full UTF-8 text content to write.
    pub content: String,
    /// The wider sandbox mode this write needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_permissions: Option<String>,
    /// One-sentence justification for the wider access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// Validated write arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteInput {
    /// Path to write.
    pub file_path: String,
    /// Full UTF-8 text content to write.
    pub content: String,
}

/// Validates the constraints the schema cannot express: only a non-blank path.
///
/// # Errors
///
/// Returns a blank-path failure.
pub fn parse_write_args(args: &WriteArgsRaw) -> anyhow::Result<WriteInput> {
    if args.file_path.trim().is_empty() {
        anyhow::bail!("file_path must be a non-empty string");
    }
    Ok(WriteInput {
        file_path: args.file_path.clone(),
        content: args.content.clone(),
    })
}

/// Formats a write outcome as one model-facing confirmation envelope.
#[must_use]
pub fn format_write_output(display_path: &str, operation: FsWriteOperation) -> String {
    let verb = match operation {
        FsWriteOperation::Create => "Created",
        FsWriteOperation::Update => "Updated",
    };
    format!("<path>{display_path}</path>\n<type>file</type>\n<content>\n{verb} file\n</content>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(file_path: &str, content: &str) -> WriteArgsRaw {
        WriteArgsRaw {
            file_path: file_path.to_owned(),
            content: content.to_owned(),
            sandbox_permissions: None,
            justification: None,
        }
    }

    #[test]
    fn parse_rejects_blank_path_and_preserves_content() {
        assert!(parse_write_args(&raw("  ", "x")).is_err());
        let input = parse_write_args(&raw("a.txt", "hello")).expect("valid");
        assert_eq!(input.file_path, "a.txt");
        assert_eq!(input.content, "hello");
    }

    #[test]
    fn format_write_output_selects_verb() {
        let created = format_write_output("/a", FsWriteOperation::Create);
        assert!(created.contains("<path>/a</path>"));
        assert!(created.contains("Created file"));
        let updated = format_write_output("/a", FsWriteOperation::Update);
        assert!(updated.contains("Updated file"));
    }
}
