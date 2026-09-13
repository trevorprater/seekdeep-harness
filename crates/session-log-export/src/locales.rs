//! Locale dictionaries owned by browser export feedback.

/// Stable locale namespace.
pub const NS: &str = "session-log-download";

/// English locale rows.
pub const EN: &[(&str, &str)] = &[
    ("dialog.preparingTitle", "Exporting Session"),
    (
        "dialog.preparingDescription",
        "Preparing a ZIP containing this Session, its sub-Sessions, and attachments.",
    ),
    ("dialog.successTitle", "Session download started"),
    (
        "dialog.successDescription",
        "The browser is downloading the Session ZIP.",
    ),
    ("dialog.errorTitle", "Session export failed"),
    ("dialog.close", "Close"),
    (
        "dialog.commandFailed",
        "Could not start the Session export.",
    ),
];

/// Simplified-Chinese locale rows.
pub const ZH: &[(&str, &str)] = &[
    ("dialog.preparingTitle", "正在导出 Session"),
    (
        "dialog.preparingDescription",
        "正在准备包含当前 Session、子 Session 和附件的 ZIP 文件。",
    ),
    ("dialog.successTitle", "Session 导出已开始下载"),
    (
        "dialog.successDescription",
        "浏览器正在下载 Session ZIP 文件。",
    ),
    ("dialog.errorTitle", "Session 导出失败"),
    ("dialog.close", "关闭"),
    ("dialog.commandFailed", "无法启动 Session 导出。"),
];
