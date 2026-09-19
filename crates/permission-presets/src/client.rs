//! Client-namespace projection of the permission domain.

pub use crate::types::{PermissionSelect, PresetOption};

/// Machine value whose GUI presentation requires an explicit risk gate.
pub const FULL_ACCESS_PRESET: &str = "danger-full-access";

/// Converts a conventional ASCII kebab-case key into title case.
#[must_use]
pub fn display_preset_name(name: &str) -> String {
    if !is_conventional_kebab(name) {
        return name.to_owned();
    }
    name.split('-')
        .map(|word| {
            let mut characters = word.chars();
            let mut title = String::with_capacity(word.len());
            if let Some(first) = characters.next() {
                title.push(first.to_ascii_uppercase());
            }
            title.extend(characters);
            title
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Renders one permission preset under its product label.
#[must_use]
pub fn display_permission_preset(value: &str, name: &str) -> String {
    if value == FULL_ACCESS_PRESET {
        "Full access".to_owned()
    } else {
        display_preset_name(name)
    }
}

fn is_conventional_kebab(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_preset_labels_preserve_source_title_and_risk_rules() {
        for (name, expected) in [
            ("workspace-write", "Workspace Write"),
            ("read-only-2", "Read Only 2"),
            ("already Labelled", "already Labelled"),
            ("UPPER-case", "UPPER-case"),
            ("double--dash", "double--dash"),
            ("", ""),
            ("中文", "中文"),
        ] {
            assert_eq!(display_preset_name(name), expected);
        }
        assert_eq!(
            display_permission_preset("danger-full-access", "ignored-name"),
            "Full access"
        );
        assert_eq!(
            display_permission_preset("workspace-write", "custom-label"),
            "Custom Label"
        );
    }
}
