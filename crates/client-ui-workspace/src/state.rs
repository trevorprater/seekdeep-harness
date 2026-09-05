//! Persisted workspace-browser view state and exact actions.

use std::{collections::BTreeMap, rc::Rc};

use indexmap::IndexMap;
use seekdeep_client_runtime::{EngineStoreHandle, StoreAction, StoreDeclaration, StoreEnvironment};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Browser-local order account for the hierarchy-free flat Session list.
pub const FLAT_SESSION_ORDER_KEY: &str = "__flat_session_order__";
/// Persisted browser-store key.
pub const WORKSPACE_VIEW_PERSIST_KEY: &str = "dsh.workspace.view.v5";

/// Session-list grouping mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionGroupBy {
    /// Workspace sections.
    #[default]
    Workspace,
    /// One hierarchy-free list.
    Flat,
}

/// Browser session ordering mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionOrderBy {
    /// User-arranged only.
    Manual,
    /// User-arranged plus activity promotion.
    #[default]
    Updated,
}

/// Workspace browser viewing state persisted across surface remounts and reloads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceViewState {
    /// Grouped or flat layout.
    pub group_by: SessionGroupBy,
    /// Manual or activity-promoted order.
    pub order_by: SessionOrderBy,
    /// Expansion by Workspace group identity.
    pub group_expansion: IndexMap<String, bool>,
    /// Editable order by Workspace/flat account.
    pub session_order_by_account: IndexMap<String, Vec<String>>,
    /// Last observed timestamps by order account.
    pub session_updated_at_by_account: IndexMap<String, IndexMap<String, i64>>,
}

impl Default for WorkspaceViewState {
    fn default() -> Self {
        Self {
            group_by: SessionGroupBy::Workspace,
            order_by: SessionOrderBy::Updated,
            group_expansion: IndexMap::new(),
            session_order_by_account: IndexMap::new(),
            session_updated_at_by_account: IndexMap::new(),
        }
    }
}

impl WorkspaceViewState {
    /// Sets the grouping mode.
    pub fn set_group_by(&mut self, mode: SessionGroupBy) {
        self.group_by = mode;
    }

    /// Sets the ordering mode.
    pub fn set_order_by(&mut self, mode: SessionOrderBy) {
        self.order_by = mode;
    }

    /// Sets one group expansion bit.
    pub fn set_group_expanded(&mut self, key: impl Into<String>, expanded: bool) {
        self.group_expansion.insert(key.into(), expanded);
    }

    /// Removes state for every account outside the retained key set.
    pub fn retain_account_keys(&mut self, workspace_keys: &[String]) {
        self.group_expansion
            .retain(|key, _| workspace_keys.contains(key));
        self.session_order_by_account
            .retain(|key, _| workspace_keys.contains(key));
        self.session_updated_at_by_account
            .retain(|key, _| workspace_keys.contains(key));
    }

    /// Atomically replaces one account's order and timestamp baseline.
    pub fn sync_session_order_account(
        &mut self,
        account_key: impl Into<String>,
        order: Vec<String>,
        updated_at: IndexMap<String, i64>,
    ) {
        let account_key = account_key.into();
        self.session_order_by_account
            .insert(account_key.clone(), order);
        self.session_updated_at_by_account
            .insert(account_key, updated_at);
    }

    /// Replaces one account's editable order without changing its timestamp baseline.
    pub fn set_session_order(&mut self, account_key: impl Into<String>, order: Vec<String>) {
        self.session_order_by_account
            .insert(account_key.into(), order);
    }
}

fn argument<'a>(arguments: &'a [Value], index: usize, action: &str) -> Result<&'a Value, String> {
    arguments
        .get(index)
        .ok_or_else(|| format!("workspace view action {action:?} is missing argument {index}"))
}

fn string_argument(arguments: &[Value], index: usize, action: &str) -> Result<String, String> {
    argument(arguments, index, action)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            format!("workspace view action {action:?} argument {index} must be a string")
        })
}

fn string_list(value: &Value, owner: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{owner} must be an array"))?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("{owner}[{index}] must be a string"))
        })
        .collect()
}

fn set_group_by_action(draft: &mut WorkspaceViewState, arguments: &[Value]) -> Result<(), String> {
    draft.set_group_by(
        match string_argument(arguments, 0, "setGroupBy")?.as_str() {
            "workspace" => SessionGroupBy::Workspace,
            "flat" => SessionGroupBy::Flat,
            mode => return Err(format!("unknown Workspace group mode {mode:?}")),
        },
    );
    Ok(())
}

fn set_order_by_action(draft: &mut WorkspaceViewState, arguments: &[Value]) -> Result<(), String> {
    draft.set_order_by(
        match string_argument(arguments, 0, "setOrderBy")?.as_str() {
            "manual" => SessionOrderBy::Manual,
            "updated" => SessionOrderBy::Updated,
            mode => return Err(format!("unknown Workspace order mode {mode:?}")),
        },
    );
    Ok(())
}

fn set_group_expanded_action(
    draft: &mut WorkspaceViewState,
    arguments: &[Value],
) -> Result<(), String> {
    let key = string_argument(arguments, 0, "setGroupExpanded")?;
    let expanded = argument(arguments, 1, "setGroupExpanded")?
        .as_bool()
        .ok_or_else(|| {
            "workspace view action \"setGroupExpanded\" argument 1 must be a boolean".to_owned()
        })?;
    draft.set_group_expanded(key, expanded);
    Ok(())
}

fn retain_account_keys_action(
    draft: &mut WorkspaceViewState,
    arguments: &[Value],
) -> Result<(), String> {
    let keys = string_list(
        argument(arguments, 0, "retainAccountKeys")?,
        "workspace view retained account keys",
    )?;
    draft.retain_account_keys(&keys);
    Ok(())
}

fn sync_session_order_account_action(
    draft: &mut WorkspaceViewState,
    arguments: &[Value],
) -> Result<(), String> {
    let key = string_argument(arguments, 0, "syncSessionOrderAccount")?;
    let order = string_list(
        argument(arguments, 1, "syncSessionOrderAccount")?,
        "workspace view Session order",
    )?;
    let updated_at = argument(arguments, 2, "syncSessionOrderAccount")?
        .as_object()
        .ok_or_else(|| "workspace view timestamp baseline must be an object".to_owned())?
        .iter()
        .map(|(session_id, value)| {
            value
                .as_i64()
                .map(|value| (session_id.clone(), value))
                .ok_or_else(|| {
                    format!("workspace view timestamp for {session_id:?} must be an integer")
                })
        })
        .collect::<Result<IndexMap<_, _>, _>>()?;
    draft.sync_session_order_account(key, order, updated_at);
    Ok(())
}

fn set_session_order_action(
    draft: &mut WorkspaceViewState,
    arguments: &[Value],
) -> Result<(), String> {
    let key = string_argument(arguments, 0, "setSessionOrder")?;
    let order = string_list(
        argument(arguments, 1, "setSessionOrder")?,
        "workspace view Session order",
    )?;
    draft.set_session_order(key, order);
    Ok(())
}

/// Builds the reusable engine-backed Store handle with the source action vocabulary.
#[must_use]
pub fn create_workspace_view_store(
    environment: StoreEnvironment<WorkspaceViewState>,
) -> Rc<EngineStoreHandle<WorkspaceViewState>> {
    let actions = BTreeMap::<String, StoreAction<WorkspaceViewState>>::from([
        (
            "setGroupBy".to_owned(),
            Rc::new(set_group_by_action) as StoreAction<WorkspaceViewState>,
        ),
        ("setOrderBy".to_owned(), Rc::new(set_order_by_action)),
        (
            "setGroupExpanded".to_owned(),
            Rc::new(set_group_expanded_action),
        ),
        (
            "retainAccountKeys".to_owned(),
            Rc::new(retain_account_keys_action),
        ),
        (
            "syncSessionOrderAccount".to_owned(),
            Rc::new(sync_session_order_account_action),
        ),
        (
            "setSessionOrder".to_owned(),
            Rc::new(set_session_order_action),
        ),
    ]);
    EngineStoreHandle::new(
        StoreDeclaration {
            init: Rc::new(WorkspaceViewState::default),
            persist: Some(WORKSPACE_VIEW_PERSIST_KEY.to_owned()),
            actions,
        },
        environment,
    )
}
