//! User-question domain face, composer state, and plan-review projection.

mod flow;
mod model;
mod pending;

pub use flow::*;
pub use model::*;
pub use pending::*;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-user-questions";
/// Browser plugin dependencies in exact source order.
pub const INJECT: &[&str] = &["slots", "locale"];
/// Dictionary namespace owned by the browser plugin.
pub const LOCALE_NAMESPACE: &str = "question";
/// Simplified-Chinese question-composer copy in source order.
pub const QUESTION_ZH: &[(&str, &str)] = &[
    ("error.incomplete", "请先完成这道问题。"),
    ("error.unanswered", "请选择一个选项或填写自定义答案。"),
    ("nav.prev", "上一题"),
    ("nav.next", "下一题"),
    ("nav.cancel", "放弃整组问题"),
    ("option.recommended", "推荐"),
    ("custom.placeholder", "输入你的答案"),
    ("action.skip", "跳过本题"),
    ("action.next", "下一题"),
    ("plan.header", "计划待审"),
    ("plan.approve", "确认执行"),
    ("plan.decline", "拒绝"),
    ("plan.discuss", "去聊天里说"),
];
/// English question-composer copy in source order.
pub const QUESTION_EN: &[(&str, &str)] = &[
    ("error.incomplete", "Please complete this question first."),
    (
        "error.unanswered",
        "Please select an option or enter a custom answer.",
    ),
    ("nav.prev", "Previous question"),
    ("nav.next", "Next question"),
    ("nav.cancel", "Dismiss all questions"),
    ("option.recommended", "Recommended"),
    ("custom.placeholder", "Type your answer"),
    ("action.skip", "Skip this question"),
    ("action.next", "Next"),
    ("plan.header", "Plan review"),
    ("plan.approve", "Approve"),
    ("plan.decline", "Refuse"),
    ("plan.discuss", "Chat about it"),
];

/// Builds the no-op Host half; model-facing tool composition belongs to presets.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
