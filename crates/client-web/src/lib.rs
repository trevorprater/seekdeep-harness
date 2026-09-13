//! Browser boot kernel, status projection, platform table, and shell components.

mod boot;
mod signal;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use boot::*;
pub use signal::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-web-invariant";
/// Shell-owned pseudo entry identity.
pub const APP_SHELL_ID: &str = "@seekdeep-ai/seekdeep-client-app-shell";
/// Client Modules graph row identity.
pub const MODULES_ID: &str = "@seekdeep-ai/seekdeep-client-modules";

/// Browser platform module specifiers in frozen seed-table order.
pub const PLATFORM_MODULES: &[&str] = &[
    "react",
    "react/jsx-runtime",
    "react-dom",
    "react-dom/client",
    "@seekdeep-ai/cordis",
    "@seekdeep-ai/seekdeep-client-ui-slots",
    "@seekdeep-ai/seekdeep-client-web-react",
    "@seekdeep-ai/seekdeep-client-ui-primitives",
    "@seekdeep-ai/seekdeep-client-ui-attachment",
    "@seekdeep-ai/seekdeep-client-schema-form",
];

/// Compiled boot-page stylesheet.
pub const APP_ROOT_STYLES: &str = include_str!("../data/app-root.css");
/// Shell global base stylesheet.
pub const BASE_STYLES: &str = include_str!("../../../packages/client/web/src/base.css");
