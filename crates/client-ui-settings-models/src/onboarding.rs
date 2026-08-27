//! Product-wide welcome notice and model-onboarding constants.

/// Durable namespace for product-wide GUI onboarding facts.
pub const WELCOME_NOTICE_SETTINGS_NAMESPACE: &str = "ui-onboarding";
/// Last acknowledged welcome-copy version field.
pub const WELCOME_NOTICE_ACK_FIELD: &str = "welcomeNoticeVersion";
/// Current materially distinct notice version.
pub const WELCOME_NOTICE_VERSION: &str = "2026-08-13.1";

/// Localized welcome notice copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WelcomeNoticeCopy {
    /// Dialog title.
    pub title: &'static str,
    /// Complete two-paragraph notice body.
    pub body: &'static str,
    /// Continue action label.
    pub continue_label: &'static str,
}

/// Simplified-Chinese internal testing notice.
pub const WELCOME_NOTICE_ZH: WelcomeNoticeCopy = WelcomeNoticeCopy {
    title: "内测声明",
    body: "SeekDeep Harness 目前的 0.1 版本仍处在面向 Harness 开发者进行测试的阶段，还有许多地方需要持续改进和打磨，希望听取广大开发者的反馈建议。预计 SeekDeep Harness 的核心插件以及基础 API 都会在接下来的一段时间内快速迭代、持续演化。\n\n我们期待与全球开发者一起，在开源、开放、可复用、可组合的基础设施之上，共同探索智能上限。欢迎全球 Harness 开发者加入 SeekDeep 插件生态。",
    continue_label: "继续",
};

/// English internal testing notice.
pub const WELCOME_NOTICE_EN: WelcomeNoticeCopy = WelcomeNoticeCopy {
    title: "Internal Testing Notice",
    body: "SeekDeep Harness 0.1 remains in testing for Harness developers. Many areas need further improvement, and we welcome feedback from the developer community. SeekDeep Harness's core plugins and foundational APIs will continue to evolve rapidly over the coming months.\n\nWe look forward to exploring the limits of intelligence with developers around the world, building on open-source, open, reusable, and composable infrastructure. We welcome Harness developers everywhere to join the SeekDeep plugin ecosystem.",
    continue_label: "Continue",
};
