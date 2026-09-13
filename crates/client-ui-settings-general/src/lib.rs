//! Settings shell, General section, local-document action, and onboarding settings owner.

mod document;
#[cfg(not(target_arch = "wasm32"))]
mod host;
mod projection;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use document::*;
#[cfg(not(target_arch = "wasm32"))]
pub use host::*;
pub use projection::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Browser dictionary namespace owned by the shell.
pub const SETTINGS_LOCALE_NAMESPACE: &str = "settings";
/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-ui-settings-general-invariant";

/// Simplified-Chinese shell dictionary.
pub const SETTINGS_ZH: &[(&str, &str)] = &[
    ("trigger", "设置"),
    ("title", "设置"),
    ("close", "关闭"),
    ("openDocument", "打开配置文件"),
    ("openDocument.error", "无法打开配置文件"),
    ("general.nav", "通用设置"),
];

/// English shell dictionary with the exact same key set.
pub const SETTINGS_EN: &[(&str, &str)] = &[
    ("trigger", "Settings"),
    ("title", "Settings"),
    ("close", "Close"),
    ("openDocument", "Open configuration file"),
    ("openDocument.error", "Could not open configuration file"),
    ("general.nav", "General"),
];
