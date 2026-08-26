//! Per-message feedback controller, browser plugin, and message controls.

#[cfg(target_arch = "wasm32")]
mod browser;
mod controller;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use browser::*;
pub use controller::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Browser plugin dependencies in exact source order.
pub const INJECT: &[&str] = &["slots", "remote", "remote.messageFeedback", "locale"];
/// Dictionary namespace.
pub const LOCALE_NAMESPACE: &str = "feedback";
/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-ui-feedback-invariant";
/// Compiled per-message controls stylesheet.
pub const FEEDBACK_STYLES: &str = include_str!("../data/styles.css");

/// Host half of this pure UI plugin; it intentionally owns no native effects.
pub fn apply_host() {}

/// Simplified-Chinese feedback copy.
pub const FEEDBACK_ZH: &[(&str, &str)] = &[
    ("action.like", "好的回答"),
    ("action.likeActive", "取消标记"),
    ("action.dislike", "有问题的回答"),
    ("action.dislikeActive", "取消标记"),
    ("note.open", "补充说明"),
    ("note.placeholder", "这条回答哪里好，或哪里有问题？（可选）"),
    ("note.save", "保存"),
    ("note.cancel", "取消"),
    ("note.aria", "反馈说明"),
    ("error.conflict", "这条反馈已在别处改动，已显示最新状态"),
    ("error.load", "反馈状态加载失败"),
    ("error.generic", "反馈保存失败"),
];

/// English feedback copy.
pub const FEEDBACK_EN: &[(&str, &str)] = &[
    ("action.like", "Good response"),
    ("action.likeActive", "Remove rating"),
    ("action.dislike", "Bad response"),
    ("action.dislikeActive", "Remove rating"),
    ("note.open", "Add a note"),
    (
        "note.placeholder",
        "What was good, or what went wrong? (optional)",
    ),
    ("note.save", "Save"),
    ("note.cancel", "Cancel"),
    ("note.aria", "Feedback note"),
    (
        "error.conflict",
        "This feedback changed elsewhere; the latest state is shown",
    ),
    ("error.load", "Could not load feedback"),
    ("error.generic", "Could not save feedback"),
];
