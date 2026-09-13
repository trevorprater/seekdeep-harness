//! Command popup dictionary namespace and exact source copy.

/// Popup-select dictionary namespace.
pub const COMMAND_NS: &str = "command";

/// Key, Simplified Chinese, and English values in source order.
pub const COMMAND_LOCALES: [(&str, &str, &str); 7] = [
    ("search.placeholder", "搜索…", "Search…"),
    ("search.aria", "筛选选项", "Filter options"),
    ("status.loading", "正在加载选项…", "Loading options…"),
    ("status.applying", "正在应用…", "Applying…"),
    ("status.empty", "无选项", "No options"),
    ("overlay.aria", "/{command} 选项", "/{command} options"),
    ("listbox.aria", "/{command} 匹配项", "/{command} matches"),
];
