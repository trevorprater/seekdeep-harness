//! Sidebar shell geometry, composition contracts, and browser plugin.

mod state;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use state::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Wide-content unmount delay matching the source fade.
pub const COLLAPSE_SETTLE_MS: u64 = 150;
/// Pointer-leave scrollbar linger duration.
pub const SCROLLBAR_LINGER_MS: u64 = 2_000;
/// Browser dictionary namespace.
pub const SIDEBAR_LOCALE_NAMESPACE: &str = "sidebar";
/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-ui-sidebar-invariant";
/// Compiled sidebar stylesheet embedded by the browser module.
pub const SIDEBAR_STYLES: &str = include_str!("../data/styles.css");

/// Simplified-Chinese shell controls dictionary.
pub const SIDEBAR_ZH: &[(&str, &str)] = &[
    ("session.new", "新会话"),
    ("session.new.label", "新建会话"),
    ("toggle.open", "打开侧边栏"),
    ("toggle.collapse", "收起侧边栏"),
];

/// English shell controls dictionary.
pub const SIDEBAR_EN: &[(&str, &str)] = &[
    ("session.new", "New Session"),
    ("session.new.label", "New session"),
    ("toggle.open", "Open sidebar"),
    ("toggle.collapse", "Collapse sidebar"),
];

/// Host-side no-op package row.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new("client-ui-sidebar", std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
