//! Theme settings, registry semantics, Host bootstrap, and browser plugin.

mod appearance;
mod boot;
mod core;
#[cfg(not(target_arch = "wasm32"))]
mod host;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use appearance::*;
pub use boot::*;
pub use core::*;
#[cfg(not(target_arch = "wasm32"))]
pub use host::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Browser plugin dependencies in source order.
pub const INJECT: &[&str] = &["slots", "locale", "connection", "remote", "settingsScope"];
/// Settings-row locale namespace.
pub const SETTINGS_NS: &str = "settings.theme";
/// Durable Host settings namespace.
pub const THEME_SETTINGS_NAMESPACE: &str = "ui-theme";
/// Durable preference field.
pub const THEME_PREFERENCE_FIELD: &str = "preference";
/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-ui-theme-invariant";

/// Simplified-Chinese Appearance copy.
pub const THEME_ZH: &[(&str, &str)] = &[
    ("appearance.title", "外观"),
    ("appearance.light", "浅色"),
    ("appearance.dark", "深色"),
    ("appearance.system", "跟随系统"),
];

/// English Appearance copy.
pub const THEME_EN: &[(&str, &str)] = &[
    ("appearance.title", "Appearance"),
    ("appearance.light", "Light"),
    ("appearance.dark", "Dark"),
    ("appearance.system", "System"),
];

/// Compiled Appearance-row stylesheet.
pub const APPEARANCE_STYLES: &str = include_str!("../data/appearance.css");
/// Source global base stylesheet shipped by the package.
pub const BASE_STYLES: &str = include_str!("../../../packages/client/ui-theme/src/styles/base.css");
/// Source design-token stylesheet shipped by the package.
pub const DESIGN_PLATFORM_STYLES: &str =
    include_str!("../../../packages/client/ui-theme/src/styles/design-platform.css");
/// Source gradient/shadow/text stylesheet shipped by the package.
pub const GRADIENT_SHADOW_TEXT_STYLES: &str =
    include_str!("../../../packages/client/ui-theme/src/styles/gradient-shadow-text.css");
/// Source scrollbar stylesheet shipped by the package.
pub const SCROLLBAR_STYLES: &str =
    include_str!("../../../packages/client/ui-theme/src/styles/scrollbar.css");
/// Source syntax-highlight stylesheet shipped by the package.
pub const SHIKI_STYLES: &str =
    include_str!("../../../packages/client/ui-theme/src/styles/shiki.css");
