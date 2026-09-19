//! Deliverables dictionary namespace and exact source copy.

/// Dictionary namespace owned by the plugin.
pub const DELIVERABLES_NS: &str = "deliverables";

/// Simplified Chinese dictionary in source key order.
pub const DELIVERABLES_ZH: [(&str, &str); 5] = [
    ("produced.label", "产物"),
    ("produced.moreOne", "+ 1 个文件"),
    ("produced.more", "+ {count} 个文件"),
    ("produced.open", "打开 {name}"),
    ("produced.showInFolder", "在文件夹中显示"),
];

/// English dictionary in the same key order.
pub const DELIVERABLES_EN: [(&str, &str); 5] = [
    ("produced.label", "Produced"),
    ("produced.moreOne", "+ 1 file"),
    ("produced.more", "+ {count} files"),
    ("produced.open", "Open {name}"),
    ("produced.showInFolder", "Show in folder"),
];
