//! Model-facing literal edit: validation and output formatting.

use serde::{Deserialize, Serialize};

/// Raw schema-validated edit arguments, including the escalation fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditArgsRaw {
    /// Path to edit.
    pub file_path: String,
    /// Literal text to replace.
    pub old_string: String,
    /// Literal replacement text.
    pub new_string: String,
    /// Replace every match instead of requiring exactly one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_all: Option<bool>,
    /// The wider sandbox mode this edit needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_permissions: Option<String>,
    /// One-sentence justification for the wider access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// Validated edit arguments after defaulting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditInput {
    /// Path to edit.
    pub file_path: String,
    /// Literal text to replace.
    pub old_string: String,
    /// Literal replacement text.
    pub new_string: String,
    /// Replace every match instead of requiring exactly one.
    pub replace_all: bool,
}

/// Validates the constraints the schema cannot express.
///
/// # Errors
///
/// Returns a blank-path, empty-old-string, or equal-pair failure.
pub fn parse_edit_args(args: &EditArgsRaw) -> anyhow::Result<EditInput> {
    if args.file_path.trim().is_empty() {
        anyhow::bail!("file_path must be a non-empty string");
    }
    if args.old_string.is_empty() {
        anyhow::bail!("old_string must be a non-empty string");
    }
    if args.old_string == args.new_string {
        anyhow::bail!("old_string and new_string must differ");
    }
    Ok(EditInput {
        file_path: args.file_path.clone(),
        old_string: args.old_string.clone(),
        new_string: args.new_string.clone(),
        replace_all: args.replace_all.unwrap_or(false),
    })
}

/// Formats an edit success as a Claude-style model-facing message.
#[must_use]
pub fn format_edit_output(display_path: &str, replace_all: bool) -> String {
    if replace_all {
        format!(
            "The file {display_path} has been updated. All occurrences were successfully replaced."
        )
    } else {
        format!("The file {display_path} has been updated successfully.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(old_string: &str, new_string: &str) -> EditArgsRaw {
        EditArgsRaw {
            file_path: "a.txt".to_owned(),
            old_string: old_string.to_owned(),
            new_string: new_string.to_owned(),
            replace_all: None,
            sandbox_permissions: None,
            justification: None,
        }
    }

    #[test]
    fn parse_rejects_blank_empty_and_equal() {
        assert!(
            parse_edit_args(&EditArgsRaw {
                file_path: "  ".to_owned(),
                old_string: "a".to_owned(),
                new_string: "b".to_owned(),
                replace_all: None,
                sandbox_permissions: None,
                justification: None,
            })
            .is_err()
        );
        assert!(parse_edit_args(&raw("", "b")).is_err());
        assert!(parse_edit_args(&raw("a", "a")).is_err());
    }

    #[test]
    fn parse_defaults_replace_all_to_false() {
        let input = parse_edit_args(&raw("a", "b")).expect("valid");
        assert_eq!(input.old_string, "a");
        assert_eq!(input.new_string, "b");
        assert!(!input.replace_all);
    }

    #[test]
    fn format_edit_output_selects_wording() {
        assert_eq!(
            format_edit_output("/a", false),
            "The file /a has been updated successfully."
        );
        assert_eq!(
            format_edit_output("/a", true),
            "The file /a has been updated. All occurrences were successfully replaced."
        );
    }
}
