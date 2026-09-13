//! Subagent dictionary namespace and exact Chinese/English copy.

/// Dictionary namespace owned by the plugin.
pub const SUBAGENT_NS: &str = "subagent";

/// Key, Simplified Chinese, and English values in source order.
pub const SUBAGENT_LOCALES: [(&str, &str, &str); 33] = [
    (
        "diagnostic.corrupt",
        "会话记录损坏",
        "corrupted session record",
    ),
    (
        "diagnostic.unsupported",
        "子代理记录版本不受支持",
        "unsupported subagent record version",
    ),
    (
        "diagnostic.unavailable",
        "会话记录暂不可用",
        "session record temporarily unavailable",
    ),
    ("duration.seconds", "{seconds}秒", "{seconds}s"),
    (
        "duration.minutes",
        "{minutes}分{seconds}秒",
        "{minutes}m {seconds}s",
    ),
    (
        "duration.hours",
        "{hours}小时{minutes}分{seconds}秒",
        "{hours}h {minutes}m {seconds}s",
    ),
    ("duration.days", "{days}天", "{days}d"),
    (
        "duration.daysHours",
        "{days}天{hours}小时",
        "{days}d {hours}h",
    ),
    ("duration.months", "约{months}个月", "~{months}mo"),
    (
        "duration.monthsDays",
        "约{months}个月{days}天",
        "~{months}mo {days}d",
    ),
    ("duration.years", "约{years}年", "~{years}y"),
    (
        "duration.yearsMonths",
        "约{years}年{months}个月",
        "~{years}y {months}mo",
    ),
    (
        "duration.exactDays",
        "{days}天{hours}小时{minutes}分{seconds}秒",
        "{days}d {hours}h {minutes}m {seconds}s",
    ),
    (
        "duration.exactTitle",
        "总活跃耗时：{duration}",
        "Total active duration: {duration}",
    ),
    ("loading.label", "正在加载子代理…", "Loading subagents…"),
    ("loading.aria", "正在加载子代理", "Loading subagents"),
    ("load.error", "无法加载子代理", "Unable to load subagents"),
    ("retry", "重试", "Retry"),
    ("mode.oneShot", "一次性", "one-shot"),
    ("mode.continuable", "可继续", "continuable"),
    ("activity.running", "正在运行", "running"),
    ("activity.inactive", "当前未运行", "not running"),
    (
        "branch.collapse",
        "收起 {label} 的下级子代理",
        "Collapse {label} descendants",
    ),
    (
        "branch.expand",
        "展开 {label} 的下级子代理",
        "Expand {label} descendants",
    ),
    ("count.total.one", "{count} 个子代理", "{count} subagent"),
    ("count.total.other", "{count} 个子代理", "{count} subagents"),
    (
        "count.running.one",
        "{count} 个子代理，正在运行",
        "{count} subagent running",
    ),
    (
        "count.running.other",
        "{count} 个子代理，正在运行",
        "{count} subagents running",
    ),
    ("tree.aria", "子代理会话", "Subagent sessions"),
    (
        "readonly.oneShot.title",
        "一次性子代理记录",
        "One-shot subagent record",
    ),
    (
        "readonly.title",
        "此子代理暂时只读",
        "This subagent is read-only for now",
    ),
    (
        "readonly.oneShot.body",
        "一次性任务不支持后续消息，可在这里查看完整执行记录。",
        "One-shot tasks do not accept follow-ups; review the full execution record here.",
    ),
    (
        "readonly.body",
        "父会话当前不在线，重新打开父会话后即可继续发送消息。",
        "The parent session is offline; reopen it to continue sending messages.",
    ),
];
