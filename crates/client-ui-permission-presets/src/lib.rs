//! Permission preset Settings and popup UI semantics.

mod controller;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use controller::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

use seekdeep_client_ui_commands::{SelectConfirmation, SelectOption};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-permission-presets";
/// Permission Settings namespace.
pub const PERMISSION_SETTINGS_NS: &str = "permission";
/// Full access machine value requiring risk confirmation.
pub const FULL_ACCESS_PRESET: &str = "danger-full-access";
/// Settings-row locale namespace.
pub const SETTINGS_NS: &str = "settings.permission";
/// Current-session confirmation namespace.
pub const ACCESS_NS: &str = "permission.access";
/// Compiled Permission Settings row stylesheet.
pub const PERMISSION_ROW_STYLES: &str = include_str!("../data/permission-row.css");

/// Settings key, Chinese, and English values.
pub const SETTINGS_LOCALES: [(&str, &str, &str); 9] = [
    ("title", "权限", "Permission"),
    (
        "description",
        "选择新会话的默认权限模式",
        "Choose the default permission mode for new sessions",
    ),
    ("loading", "加载中", "Loading"),
    ("unavailable", "不可用", "Unavailable"),
    (
        "confirm.title",
        "确认启用 Full access？",
        "Enable Full access?",
    ),
    (
        "confirm.description",
        "启用 Full access 后，新会话将减少确认步骤，并且可以直接执行更多操作，包括敏感操作、文件修改或外部命令。仅建议在你信任后续任务时使用。",
        "Full access lets new sessions reduce confirmation steps and perform more actions directly, including sensitive operations, file changes, or external commands. Only use it when you trust subsequent tasks.",
    ),
    (
        "confirm.acknowledge",
        "我已了解风险，并愿意继续",
        "I understand the risks and want to continue",
    ),
    ("confirm.cancel", "取消", "Cancel"),
    ("confirm.enable", "启用 Full access", "Enable Full access"),
];

/// Access popup key, Chinese, and English values.
pub const ACCESS_LOCALES: [(&str, &str, &str); 5] = [
    (
        "confirm.title",
        "确认启用 Full access？",
        "Enable Full access?",
    ),
    (
        "confirm.description",
        "启用 Full access 后，agent 将减少确认步骤，并且可以直接执行更多操作，包括敏感操作、文件修改或外部命令。仅建议在你信任当前任务时使用。",
        "Full access reduces confirmation steps and lets the agent perform more actions directly, including sensitive operations, file changes, or external commands. Only use it when you trust the current task.",
    ),
    (
        "confirm.acknowledge",
        "我已了解风险，并愿意继续",
        "I understand the risks and want to continue",
    ),
    ("confirm.cancel", "取消", "Cancel"),
    ("confirm.enable", "启用 Full access", "Enable Full access"),
];

/// One selectable future-session default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionDefaultOption {
    /// Preset key written to Settings.
    pub id: String,
    /// Host label or derived title.
    pub label: String,
}

/// Resolved current default plus dynamic enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionDefault {
    /// Current setting.
    pub current_value: String,
    /// Advertised choices.
    pub options: Vec<PermissionDefaultOption>,
}

/// Conventional ASCII kebab-case display name.
#[must_use]
pub fn display_preset_name(name: &str) -> String {
    let conventional = !name.is_empty()
        && name.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
    if !conventional {
        return name.to_owned();
    }
    name.split('-')
        .map(|word| {
            let mut chars = word.chars();
            let mut output = String::new();
            if let Some(first) = chars.next() {
                output.push(first.to_ascii_uppercase());
            }
            output.extend(chars);
            output
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Product label for one permission preset.
#[must_use]
pub fn display_permission_preset(value: &str, name: &str) -> String {
    if value == FULL_ACCESS_PRESET {
        "Full access".to_owned()
    } else {
        display_preset_name(name)
    }
}

fn schema_ref<'a>(schema: &'a Value, id: &Value) -> Option<&'a Value> {
    let id = id.as_u64()?.to_string();
    schema.get("refs")?.get(&id)
}

/// Reads the dynamic defaultPreset enum from one Settings namespace descriptor.
///
/// # Errors
///
/// Rejects absent values, fields, empty/malformed enums, or a current value not advertised.
pub fn permission_default_of(schema: &Value, value: &Value) -> Result<PermissionDefault, String> {
    let current = value
        .get("defaultPreset")
        .and_then(Value::as_str)
        .ok_or_else(|| "permission settings has no defaultPreset value".to_owned())?;
    let root = schema_ref(schema, schema.get("uid").unwrap_or(&Value::Null))
        .ok_or_else(|| "permission settings schema has no root".to_owned())?;
    let field_id = root
        .get("dict")
        .and_then(|dict| dict.get("defaultPreset"))
        .ok_or_else(|| "permission settings schema has no defaultPreset field".to_owned())?;
    let field = schema_ref(schema, field_id)
        .ok_or_else(|| "permission settings schema has no defaultPreset field".to_owned())?;
    let candidates = if field.get("type").and_then(Value::as_str) == Some("union") {
        field
            .get("list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    } else {
        vec![field_id.clone()]
    };
    let options = candidates
        .iter()
        .filter_map(|id| schema_ref(schema, id))
        .filter_map(|choice| {
            if choice.get("type").and_then(Value::as_str) != Some("const") {
                return None;
            }
            let preset = choice.get("value")?.as_str()?;
            let described = choice
                .pointer("/meta/description")
                .and_then(Value::as_str)
                .filter(|description| !description.is_empty())
                .unwrap_or(preset);
            Some(PermissionDefaultOption {
                id: preset.to_owned(),
                label: display_permission_preset(preset, described),
            })
        })
        .collect::<Vec<_>>();
    if options.is_empty() || !options.iter().any(|option| option.id == current) {
        return Err("permission settings schema does not advertise its current preset".to_owned());
    }
    Ok(PermissionDefault {
        current_value: current.to_owned(),
        options,
    })
}

/// One current-session projected option.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    /// Machine value.
    pub value: String,
    /// Host name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
}

/// Current-session permission select projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSelect {
    /// Current machine value.
    pub current_value: String,
    /// Available choices including possible custom display state.
    pub options: Vec<PermissionOption>,
}

/// Flattens current-session presets, excluding custom and adding Full access confirmation.
#[must_use]
pub fn popup_options(
    value: &PermissionSelect,
    confirmation: impl Fn() -> SelectConfirmation,
) -> Vec<SelectOption> {
    match try_popup_options::<std::convert::Infallible>(value, || Ok(confirmation())) {
        Ok(options) => options,
        Err(error) => match error {},
    }
}

/// Flattens current-session presets while permitting fallible confirmation copy.
///
/// # Errors
///
/// Returns the confirmation factory's error only when Full access is advertised.
pub fn try_popup_options<E>(
    value: &PermissionSelect,
    confirmation: impl Fn() -> Result<SelectConfirmation, E>,
) -> Result<Vec<SelectOption>, E> {
    let mut options = Vec::new();
    for option in value
        .options
        .iter()
        .filter(|option| option.value != "custom")
    {
        options.push(SelectOption {
            id: option.value.clone(),
            label: display_permission_preset(&option.value, &option.name),
            detail: option.description.clone(),
            active: (option.value == value.current_value).then_some(true),
            confirmation: if option.value == FULL_ACCESS_PRESET {
                Some(confirmation()?)
            } else {
                None
            },
        });
    }
    Ok(options)
}

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
