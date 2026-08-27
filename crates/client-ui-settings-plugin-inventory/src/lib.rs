//! Read-only plugin inventory settings-tab semantics.

use serde::{Deserialize, Serialize};

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-settings-plugin-inventory";
/// Browser dictionary namespace.
pub const LOCALE_NAMESPACE: &str = "settings.pluginInventory";
/// No-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-ui-settings-plugin-inventory-invariant";

/// Simplified-Chinese dictionary.
pub const ZH: &[(&str, &str)] = &[
    ("tab", "插件列表"),
    ("loading", "正在读取插件…"),
    ("error", "暂时无法读取插件。"),
    ("retry", "重试"),
    ("search", "搜索插件"),
    ("catalog", "插件列表"),
    ("empty", "暂无插件。"),
    ("emptySearch", "没有匹配的插件。"),
    ("enabledTag", "已启用"),
    ("disabledTag", "已停用"),
    ("configuration", "配置状态"),
    ("cordis", "Cordis 状态"),
    ("unobserved", "未挂载"),
    ("pending", "等待依赖"),
    ("loadingPhase", "加载中"),
    ("active", "已挂载"),
    ("failed", "挂载失败"),
    ("unloading", "卸载中"),
];

/// English dictionary with the exact same key order.
pub const EN: &[(&str, &str)] = &[
    ("tab", "Plugin list"),
    ("loading", "Reading plugins…"),
    ("error", "Plugins are temporarily unavailable."),
    ("retry", "Retry"),
    ("search", "Search plugins"),
    ("catalog", "Plugin list"),
    ("empty", "No plugins are available."),
    ("emptySearch", "No matching plugins."),
    ("enabledTag", "Enabled"),
    ("disabledTag", "Disabled"),
    ("configuration", "Configuration"),
    ("cordis", "Cordis status"),
    ("unobserved", "Not mounted"),
    ("pending", "Waiting for dependencies"),
    ("loadingPhase", "Loading"),
    ("active", "Mounted"),
    ("failed", "Mount failed"),
    ("unloading", "Unloading"),
];

/// Observable root Fiber phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginFiberPhase {
    /// Waiting for injected services.
    Pending,
    /// Plugin callback is running.
    Loading,
    /// Plugin is mounted.
    Active,
    /// Mount failed.
    Failed,
    /// Disposers are running.
    Unloading,
}

/// One Host inventory row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInventoryEntry {
    /// Stable Loader entry id.
    pub entry_id: String,
    /// Exact configured module specifier.
    pub module_name: String,
    /// Effective configuration enablement.
    pub enabled: bool,
    /// Live root Fiber phase, absent for disabled/unobserved rows.
    pub fiber_phase: Option<PluginFiberPhase>,
}

/// Current Host plugin inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInventorySnapshot {
    /// Rows in Loader order.
    pub entries: Vec<PluginInventoryEntry>,
}

/// Component request state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum InventoryStatus {
    /// Remote request is pending.
    #[default]
    Loading,
    /// Remote request failed without exposing transport detail.
    Error,
    /// Snapshot is available.
    Ready,
}

/// Framework-neutral inventory-tab controller.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginInventoryController {
    /// Current request status.
    pub status: InventoryStatus,
    /// Latest successful snapshot.
    pub snapshot: PluginInventorySnapshot,
    /// Untrimmed local search query.
    pub query: String,
    /// Expanded entry id.
    pub expanded: Option<String>,
    request: u64,
}

impl PluginInventoryController {
    /// Begins a load or retry and returns its generation token.
    pub fn begin_load(&mut self) -> u64 {
        self.request = self.request.wrapping_add(1);
        self.status = InventoryStatus::Loading;
        self.request
    }

    /// Commits one success or generic failure only when it is still current.
    pub fn finish_load(&mut self, generation: u64, result: Result<PluginInventorySnapshot, ()>) {
        if generation != self.request {
            return;
        }
        match result {
            Ok(snapshot) => {
                self.status = InventoryStatus::Ready;
                self.snapshot = snapshot;
                self.reconcile_expansion();
            }
            Err(()) => self.status = InventoryStatus::Error,
        }
    }

    /// Replaces the query and collapses an entry filtered out by it.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.reconcile_expansion();
    }

    /// Expands one entry, or collapses it when already open.
    pub fn toggle(&mut self, entry_id: &str) {
        if self.expanded.as_deref() == Some(entry_id) {
            self.expanded = None;
        } else {
            self.expanded = Some(entry_id.to_owned());
            self.reconcile_expansion();
        }
    }

    /// Returns current query matches in Loader order.
    #[must_use]
    pub fn filtered_entries(&self) -> Vec<&PluginInventoryEntry> {
        let query = self.query.trim().to_lowercase();
        self.snapshot
            .entries
            .iter()
            .filter(|entry| matches_query(entry, &query))
            .collect()
    }

    fn reconcile_expansion(&mut self) {
        let Some(expanded) = self.expanded.as_deref() else {
            return;
        };
        if !self
            .filtered_entries()
            .iter()
            .any(|entry| entry.entry_id == expanded)
        {
            self.expanded = None;
        }
    }
}

/// Locale dictionary key for one observed Fiber phase.
#[must_use]
pub const fn phase_locale_key(phase: Option<PluginFiberPhase>) -> &'static str {
    match phase {
        None => "unobserved",
        Some(PluginFiberPhase::Pending) => "pending",
        Some(PluginFiberPhase::Loading) => "loadingPhase",
        Some(PluginFiberPhase::Active) => "active",
        Some(PluginFiberPhase::Failed) => "failed",
        Some(PluginFiberPhase::Unloading) => "unloading",
    }
}

/// Compacts a module specifier without guessing whether its Loader id was generated.
#[must_use]
pub fn module_short_name(module_name: &str) -> String {
    let unscoped = module_name.strip_prefix('@').map_or(module_name, |name| {
        name.split_once('/').map_or(name, |(_, rest)| rest)
    });
    unscoped
        .strip_prefix("cordis:")
        .or_else(|| unscoped.strip_prefix("cordis-plugin-"))
        .or_else(|| unscoped.strip_prefix("seekdeep-host-"))
        .or_else(|| unscoped.strip_prefix("seekdeep-client-"))
        .or_else(|| unscoped.strip_prefix("seekdeep-"))
        .unwrap_or(unscoped)
        .to_owned()
}

fn matches_query(entry: &PluginInventoryEntry, normalized_query: &str) -> bool {
    normalized_query.is_empty()
        || entry.module_name.to_lowercase().contains(normalized_query)
        || entry.entry_id.to_lowercase().contains(normalized_query)
}

/// Formats a failed generated Remote result without losing its stable code.
#[must_use]
pub fn remote_list_error(code: &str, message: &str) -> String {
    format!("pluginInventory.list failed: {code}: {message}")
}

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
