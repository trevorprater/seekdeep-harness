//! Exact directory-browser locale namespace and dictionaries.

/// Locale namespace owned by the browse flow.
pub const DIRECTORY_BROWSER_NS: &str = "directory-browser";

/// Key, Simplified Chinese, and English copy in source order.
pub const DIRECTORY_BROWSER_LOCALES: [(&str, &str, &str); 13] = [
    (
        "browser.title",
        "选择工作区目录",
        "Select Workspace Directory",
    ),
    ("browser.home", "主目录", "Home"),
    ("browser.newFolder", "新建文件夹", "New folder"),
    ("browser.folderName", "文件夹名称", "Folder name"),
    (
        "browser.createIn",
        "在\"{name}\"中新建文件夹",
        "New folder in \"{name}\"",
    ),
    ("browser.untitledFolder", "未命名文件夹", "Untitled folder"),
    ("browser.create", "创建", "Create"),
    ("browser.cancel", "取消", "Cancel"),
    ("browser.open", "打开", "Open"),
    ("browser.editPath", "编辑路径", "Edit path"),
    ("browser.loading", "加载中…", "Loading…"),
    (
        "browser.truncated",
        "文件夹过多，仅显示开头部分。",
        "Too many folders to list; only the beginning is shown.",
    ),
    ("browser.showHidden", "显示隐藏文件", "Show hidden files"),
];
