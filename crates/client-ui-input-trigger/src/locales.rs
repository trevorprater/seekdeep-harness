//! Slash-menu dictionary namespace and exact source copy.

/// Candidate-menu dictionary namespace.
pub const MENU_NS: &str = "slash.menu";

/// Key, Simplified Chinese, and English values in source order.
pub const MENU_LOCALES: [(&str, &str, &str); 5] = [
    ("command", "命令", "Commands"),
    ("skill", "技能", "Skills"),
    ("subagent", "子智能体", "Subagents"),
    ("loading", "正在加载…", "Loading…"),
    ("suggestions.aria", "触发候选建议", "Trigger suggestions"),
];
