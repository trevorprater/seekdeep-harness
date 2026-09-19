//! Exhaustive unary method registry and second-level schema dispatch.

use std::{fmt, str::FromStr};

use seekdeep_client_connection::RpcResult;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::{
    agent_presets::{
        AgentPresetCopyRequest, AgentPresetIdValue, AgentPresetListValue,
        AgentPresetOpenDocumentValue, AgentPresetReadValue, AgentPresetSelectRequest,
    },
    credentials::{
        CredentialsDescribeRequest, CredentialsDescribeValue, CredentialsSetRequest,
        CredentialsUnsetRequest,
    },
    goals::{GoalClearValue, GoalCreateRequest, GoalEditRequest, GoalRefRequest, GoalRefValue},
    host::{
        DirectoryListing, EmptyRequest, HostCreateDirectoryRequest, HostDescribeValue,
        HostListDirectoryRequest, HostOpenPathRequest, HostOpenPathValue, HostPathValue,
        HostPickDirectoryValue,
    },
    llm::{LlmDiscoverModelsRequest, LlmDiscoverModelsValue, LlmModelsValue, LlmProvidersValue},
    rpc::{ContractError, parse_rpc_error},
    sessions::{
        AcceptedValue, SessionAttachmentRequest, SessionAttachmentValue, SessionCreateRequest,
        SessionCreateValue, SessionForkRequest, SessionHistoryRequest, SessionHistoryValue,
        SessionIdValue, SessionListRequest, SessionListValue, SessionModelsValue,
        SessionPromptRequest, SessionPromptValue, SessionRenameRequest, SessionRenameValue,
        SessionSearchRequest, SessionSearchValue, SessionSelectModelRequest,
        SessionSelectModelValue, SessionUpdateQueueRequest,
    },
    settings::{
        SettingsDescribeValue, SettingsMutateRequest, SettingsNamespaceView,
        SettingsOpenDocumentValue, SettingsReplaceRequest, SettingsUpdateRequest,
    },
    skills::{SkillListRequest, SkillListValue},
    subagents::{
        SubagentHistoryRequest, SubagentInterruptRequest, SubagentListRequest, SubagentListValue,
        SubagentPromptRequest, SubagentPromptValue,
    },
    workspace::{
        WorkspaceArchiveSessionRequest, WorkspaceArchiveSessionValue, WorkspaceCreateRequest,
        WorkspaceCreateValue, WorkspaceDeleteValue, WorkspaceIdRequest,
        WorkspaceInsertBeforeRequest, WorkspaceInsertBeforeValue,
        WorkspaceInsertSessionBeforeRequest, WorkspaceListValue, WorkspaceRenameRequest,
        WorkspaceValue,
    },
};

/// Every unary API Proxy method, compiler-closed at the pinned source contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RpcMethod {
    /// `session.list`.
    SessionList,
    /// `session.search`.
    SessionSearch,
    /// `session.create`.
    SessionCreate,
    /// `session.history`.
    SessionHistory,
    /// `session.models`.
    SessionModels,
    /// `session.selectModel`.
    SessionSelectModel,
    /// `session.rename`.
    SessionRename,
    /// `session.fork`.
    SessionFork,
    /// `session.prompt`.
    SessionPrompt,
    /// `session.attachment`.
    SessionAttachment,
    /// `session.updateQueue`.
    SessionUpdateQueue,
    /// `session.cancel`.
    SessionCancel,
    /// `subagent.list`.
    SubagentList,
    /// `subagent.history`.
    SubagentHistory,
    /// `subagent.prompt`.
    SubagentPrompt,
    /// `subagent.interrupt`.
    SubagentInterrupt,
    /// `host.describe`.
    HostDescribe,
    /// `host.pickDirectory`.
    HostPickDirectory,
    /// `host.listDirectory`.
    HostListDirectory,
    /// `host.createDirectory`.
    HostCreateDirectory,
    /// `host.openPath`.
    HostOpenPath,
    /// `workspace.list`.
    WorkspaceList,
    /// `workspace.create`.
    WorkspaceCreate,
    /// `workspace.rename`.
    WorkspaceRename,
    /// `workspace.delete`.
    WorkspaceDelete,
    /// `workspace.insertBefore`.
    WorkspaceInsertBefore,
    /// `workspace.insertSessionBefore`.
    WorkspaceInsertSessionBefore,
    /// `workspace.archiveSession`.
    WorkspaceArchiveSession,
    /// `skill.list`.
    SkillList,
    /// `agentPreset.list`.
    AgentPresetList,
    /// `agentPreset.select`.
    AgentPresetSelect,
    /// `agentPreset.read`.
    AgentPresetRead,
    /// `agentPreset.copy`.
    AgentPresetCopy,
    /// `agentPreset.openDocument`.
    AgentPresetOpenDocument,
    /// `agentPreset.remove`.
    AgentPresetRemove,
    /// `goal.create`.
    GoalCreate,
    /// `goal.edit`.
    GoalEdit,
    /// `goal.pause`.
    GoalPause,
    /// `goal.resume`.
    GoalResume,
    /// `goal.complete`.
    GoalComplete,
    /// `goal.clear`.
    GoalClear,
    /// `settings.describe`.
    SettingsDescribe,
    /// `settings.openDocument`.
    SettingsOpenDocument,
    /// `settings.update`.
    SettingsUpdate,
    /// `settings.replace`.
    SettingsReplace,
    /// `settings.mutate`.
    SettingsMutate,
    /// `credentials.describe`.
    CredentialsDescribe,
    /// `credentials.set`.
    CredentialsSet,
    /// `credentials.unset`.
    CredentialsUnset,
    /// `llm.providers`.
    LlmProviders,
    /// `llm.models`.
    LlmModels,
    /// `llm.discoverModels`.
    LlmDiscoverModels,
}

/// Exact compiler-closed method ordering used by parity/invariant checks.
pub const ALL_RPC_METHODS: [RpcMethod; 52] = [
    RpcMethod::SessionList,
    RpcMethod::SessionSearch,
    RpcMethod::SessionCreate,
    RpcMethod::SessionHistory,
    RpcMethod::SessionModels,
    RpcMethod::SessionSelectModel,
    RpcMethod::SessionRename,
    RpcMethod::SessionFork,
    RpcMethod::SessionPrompt,
    RpcMethod::SessionAttachment,
    RpcMethod::SessionUpdateQueue,
    RpcMethod::SessionCancel,
    RpcMethod::SubagentList,
    RpcMethod::SubagentHistory,
    RpcMethod::SubagentPrompt,
    RpcMethod::SubagentInterrupt,
    RpcMethod::HostDescribe,
    RpcMethod::HostPickDirectory,
    RpcMethod::HostListDirectory,
    RpcMethod::HostCreateDirectory,
    RpcMethod::HostOpenPath,
    RpcMethod::WorkspaceList,
    RpcMethod::WorkspaceCreate,
    RpcMethod::WorkspaceRename,
    RpcMethod::WorkspaceDelete,
    RpcMethod::WorkspaceInsertBefore,
    RpcMethod::WorkspaceInsertSessionBefore,
    RpcMethod::WorkspaceArchiveSession,
    RpcMethod::SkillList,
    RpcMethod::AgentPresetList,
    RpcMethod::AgentPresetSelect,
    RpcMethod::AgentPresetRead,
    RpcMethod::AgentPresetCopy,
    RpcMethod::AgentPresetOpenDocument,
    RpcMethod::AgentPresetRemove,
    RpcMethod::GoalCreate,
    RpcMethod::GoalEdit,
    RpcMethod::GoalPause,
    RpcMethod::GoalResume,
    RpcMethod::GoalComplete,
    RpcMethod::GoalClear,
    RpcMethod::SettingsDescribe,
    RpcMethod::SettingsOpenDocument,
    RpcMethod::SettingsUpdate,
    RpcMethod::SettingsReplace,
    RpcMethod::SettingsMutate,
    RpcMethod::CredentialsDescribe,
    RpcMethod::CredentialsSet,
    RpcMethod::CredentialsUnset,
    RpcMethod::LlmProviders,
    RpcMethod::LlmModels,
    RpcMethod::LlmDiscoverModels,
];

impl RpcMethod {
    /// Exact HTTP path segment and logical method tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionList => "session.list",
            Self::SessionSearch => "session.search",
            Self::SessionCreate => "session.create",
            Self::SessionHistory => "session.history",
            Self::SessionModels => "session.models",
            Self::SessionSelectModel => "session.selectModel",
            Self::SessionRename => "session.rename",
            Self::SessionFork => "session.fork",
            Self::SessionPrompt => "session.prompt",
            Self::SessionAttachment => "session.attachment",
            Self::SessionUpdateQueue => "session.updateQueue",
            Self::SessionCancel => "session.cancel",
            Self::SubagentList => "subagent.list",
            Self::SubagentHistory => "subagent.history",
            Self::SubagentPrompt => "subagent.prompt",
            Self::SubagentInterrupt => "subagent.interrupt",
            Self::HostDescribe => "host.describe",
            Self::HostPickDirectory => "host.pickDirectory",
            Self::HostListDirectory => "host.listDirectory",
            Self::HostCreateDirectory => "host.createDirectory",
            Self::HostOpenPath => "host.openPath",
            Self::WorkspaceList => "workspace.list",
            Self::WorkspaceCreate => "workspace.create",
            Self::WorkspaceRename => "workspace.rename",
            Self::WorkspaceDelete => "workspace.delete",
            Self::WorkspaceInsertBefore => "workspace.insertBefore",
            Self::WorkspaceInsertSessionBefore => "workspace.insertSessionBefore",
            Self::WorkspaceArchiveSession => "workspace.archiveSession",
            Self::SkillList => "skill.list",
            Self::AgentPresetList => "agentPreset.list",
            Self::AgentPresetSelect => "agentPreset.select",
            Self::AgentPresetRead => "agentPreset.read",
            Self::AgentPresetCopy => "agentPreset.copy",
            Self::AgentPresetOpenDocument => "agentPreset.openDocument",
            Self::AgentPresetRemove => "agentPreset.remove",
            Self::GoalCreate => "goal.create",
            Self::GoalEdit => "goal.edit",
            Self::GoalPause => "goal.pause",
            Self::GoalResume => "goal.resume",
            Self::GoalComplete => "goal.complete",
            Self::GoalClear => "goal.clear",
            Self::SettingsDescribe => "settings.describe",
            Self::SettingsOpenDocument => "settings.openDocument",
            Self::SettingsUpdate => "settings.update",
            Self::SettingsReplace => "settings.replace",
            Self::SettingsMutate => "settings.mutate",
            Self::CredentialsDescribe => "credentials.describe",
            Self::CredentialsSet => "credentials.set",
            Self::CredentialsUnset => "credentials.unset",
            Self::LlmProviders => "llm.providers",
            Self::LlmModels => "llm.models",
            Self::LlmDiscoverModels => "llm.discoverModels",
        }
    }

    /// Parses and normalizes this method's Client-to-Host business payload.
    ///
    /// # Errors
    ///
    /// Returns a contract error when the payload violates the method's exact schema.
    pub fn parse_request(self, value: &Value) -> Result<Value, ContractError> {
        match self {
            Self::SessionList => normalized(SessionListRequest::parse(value)),
            Self::SessionSearch => normalized(SessionSearchRequest::parse(value)),
            Self::SessionCreate => normalized(SessionCreateRequest::parse(value)),
            Self::SessionHistory => normalized(SessionHistoryRequest::parse(value)),
            Self::SessionModels | Self::SessionCancel => normalized(SessionIdValue::parse(value)),
            Self::SessionSelectModel => normalized(SessionSelectModelRequest::parse(value)),
            Self::SessionRename => normalized(SessionRenameRequest::parse(value)),
            Self::SessionFork => normalized(SessionForkRequest::parse(value)),
            Self::SessionPrompt => normalized(SessionPromptRequest::parse(value)),
            Self::SessionAttachment => normalized(SessionAttachmentRequest::parse(value)),
            Self::SessionUpdateQueue => normalized(SessionUpdateQueueRequest::parse(value)),
            Self::SubagentList => normalized(SubagentListRequest::parse(value)),
            Self::SubagentHistory => normalized(SubagentHistoryRequest::parse(value)),
            Self::SubagentPrompt => normalized(SubagentPromptRequest::parse(value)),
            Self::SubagentInterrupt => normalized(SubagentInterruptRequest::parse(value)),
            Self::HostDescribe
            | Self::HostPickDirectory
            | Self::WorkspaceList
            | Self::AgentPresetList
            | Self::SettingsDescribe
            | Self::SettingsOpenDocument
            | Self::LlmProviders
            | Self::LlmModels => normalized(EmptyRequest::parse(value)),
            Self::HostListDirectory => normalized(HostListDirectoryRequest::parse(value)),
            Self::HostCreateDirectory => normalized(HostCreateDirectoryRequest::parse(value)),
            Self::HostOpenPath => normalized(HostOpenPathRequest::parse(value)),
            Self::WorkspaceCreate => normalized(WorkspaceCreateRequest::parse(value)),
            Self::WorkspaceRename => normalized(WorkspaceRenameRequest::parse(value)),
            Self::WorkspaceDelete => normalized(WorkspaceIdRequest::parse(value)),
            Self::WorkspaceInsertBefore => normalized(WorkspaceInsertBeforeRequest::parse(value)),
            Self::WorkspaceInsertSessionBefore => {
                normalized(WorkspaceInsertSessionBeforeRequest::parse(value))
            }
            Self::WorkspaceArchiveSession => {
                normalized(WorkspaceArchiveSessionRequest::parse(value))
            }
            Self::SkillList => normalized(SkillListRequest::parse(value)),
            Self::AgentPresetSelect => normalized(AgentPresetSelectRequest::parse(value)),
            Self::AgentPresetRead | Self::AgentPresetOpenDocument | Self::AgentPresetRemove => {
                normalized(AgentPresetIdValue::parse_request(value))
            }
            Self::AgentPresetCopy => normalized(AgentPresetCopyRequest::parse(value)),
            Self::GoalCreate => normalized(GoalCreateRequest::parse(value)),
            Self::GoalEdit => normalized(GoalEditRequest::parse(value)),
            Self::GoalPause | Self::GoalResume | Self::GoalComplete | Self::GoalClear => {
                normalized(GoalRefRequest::parse(value))
            }
            Self::SettingsUpdate => normalized(SettingsUpdateRequest::parse(value)),
            Self::SettingsReplace => normalized(SettingsReplaceRequest::parse(value)),
            Self::SettingsMutate => normalized(SettingsMutateRequest::parse(value)),
            Self::CredentialsDescribe => normalized(CredentialsDescribeRequest::parse(value)),
            Self::CredentialsSet => normalized(CredentialsSetRequest::parse(value)),
            Self::CredentialsUnset => normalized(CredentialsUnsetRequest::parse(value)),
            Self::LlmDiscoverModels => normalized(LlmDiscoverModelsRequest::parse(value)),
        }
    }

    /// Parses and normalizes this method's Host-to-Client success value.
    ///
    /// # Errors
    ///
    /// Returns a contract error when the value violates the method's exact schema.
    pub fn parse_value(self, value: &Value) -> Result<Value, ContractError> {
        match self {
            Self::SessionList => normalized(SessionListValue::parse(value)),
            Self::SessionSearch => normalized(SessionSearchValue::parse(value)),
            Self::SessionCreate => normalized(SessionCreateValue::parse(value)),
            Self::SessionHistory | Self::SubagentHistory => {
                normalized(SessionHistoryValue::parse(value))
            }
            Self::SessionModels => normalized(SessionModelsValue::parse(value)),
            Self::SessionSelectModel => normalized(SessionSelectModelValue::parse(value)),
            Self::SessionRename => normalized(SessionRenameValue::parse(value)),
            Self::SessionFork => normalized(SessionIdValue::parse(value)),
            Self::SessionPrompt => normalized(SessionPromptValue::parse(value)),
            Self::SessionAttachment => normalized(SessionAttachmentValue::parse(value)),
            Self::SessionUpdateQueue | Self::SessionCancel | Self::SubagentInterrupt => {
                normalized(AcceptedValue::parse(value))
            }
            Self::SubagentList => normalized(SubagentListValue::parse(value)),
            Self::SubagentPrompt => normalized(SubagentPromptValue::parse(value)),
            Self::HostDescribe => normalized(HostDescribeValue::parse(value)),
            Self::HostPickDirectory => normalized(HostPickDirectoryValue::parse(value)),
            Self::HostListDirectory => normalized(DirectoryListing::parse(value)),
            Self::HostCreateDirectory => normalized(HostPathValue::parse(value)),
            Self::HostOpenPath => normalized(HostOpenPathValue::parse(value)),
            Self::WorkspaceList => normalized(WorkspaceListValue::parse(value)),
            Self::WorkspaceCreate => normalized(WorkspaceCreateValue::parse(value)),
            Self::WorkspaceRename | Self::WorkspaceInsertSessionBefore => {
                normalized(WorkspaceValue::parse(value))
            }
            Self::WorkspaceDelete => normalized(WorkspaceDeleteValue::parse(value)),
            Self::WorkspaceInsertBefore => normalized(WorkspaceInsertBeforeValue::parse(value)),
            Self::WorkspaceArchiveSession => normalized(WorkspaceArchiveSessionValue::parse(value)),
            Self::SkillList => normalized(SkillListValue::parse(value)),
            Self::AgentPresetList => normalized(AgentPresetListValue::parse(value)),
            Self::AgentPresetSelect | Self::AgentPresetCopy => {
                normalized(AgentPresetIdValue::parse_value(value))
            }
            Self::AgentPresetRead => normalized(AgentPresetReadValue::parse(value)),
            Self::AgentPresetOpenDocument => normalized(AgentPresetOpenDocumentValue::parse(value)),
            Self::AgentPresetRemove | Self::CredentialsSet | Self::CredentialsUnset => {
                normalized(EmptyRequest::parse(value))
            }
            Self::GoalCreate
            | Self::GoalEdit
            | Self::GoalPause
            | Self::GoalResume
            | Self::GoalComplete => normalized(GoalRefValue::parse(value)),
            Self::GoalClear => normalized(GoalClearValue::parse(value)),
            Self::SettingsDescribe => normalized(SettingsDescribeValue::parse(value)),
            Self::SettingsOpenDocument => normalized(SettingsOpenDocumentValue::parse(value)),
            Self::SettingsUpdate | Self::SettingsReplace | Self::SettingsMutate => {
                normalized(SettingsNamespaceView::parse(value))
            }
            Self::CredentialsDescribe => normalized(CredentialsDescribeValue::parse(value)),
            Self::LlmProviders => normalized(LlmProvidersValue::parse(value)),
            Self::LlmModels => normalized(LlmModelsValue::parse(value)),
            Self::LlmDiscoverModels => normalized(LlmDiscoverModelsValue::parse(value)),
        }
    }
}

impl fmt::Display for RpcMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An arbitrary string is not a registered unary method.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("unknown API Proxy method: {0}")]
pub struct UnknownRpcMethod(String);

impl UnknownRpcMethod {
    /// Unrecognized wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RpcMethod {
    type Err = UnknownRpcMethod;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let method = match value {
            "session.list" => Self::SessionList,
            "session.search" => Self::SessionSearch,
            "session.create" => Self::SessionCreate,
            "session.history" => Self::SessionHistory,
            "session.models" => Self::SessionModels,
            "session.selectModel" => Self::SessionSelectModel,
            "session.rename" => Self::SessionRename,
            "session.fork" => Self::SessionFork,
            "session.prompt" => Self::SessionPrompt,
            "session.attachment" => Self::SessionAttachment,
            "session.updateQueue" => Self::SessionUpdateQueue,
            "session.cancel" => Self::SessionCancel,
            "subagent.list" => Self::SubagentList,
            "subagent.history" => Self::SubagentHistory,
            "subagent.prompt" => Self::SubagentPrompt,
            "subagent.interrupt" => Self::SubagentInterrupt,
            "host.describe" => Self::HostDescribe,
            "host.pickDirectory" => Self::HostPickDirectory,
            "host.listDirectory" => Self::HostListDirectory,
            "host.createDirectory" => Self::HostCreateDirectory,
            "host.openPath" => Self::HostOpenPath,
            "workspace.list" => Self::WorkspaceList,
            "workspace.create" => Self::WorkspaceCreate,
            "workspace.rename" => Self::WorkspaceRename,
            "workspace.delete" => Self::WorkspaceDelete,
            "workspace.insertBefore" => Self::WorkspaceInsertBefore,
            "workspace.insertSessionBefore" => Self::WorkspaceInsertSessionBefore,
            "workspace.archiveSession" => Self::WorkspaceArchiveSession,
            "skill.list" => Self::SkillList,
            "agentPreset.list" => Self::AgentPresetList,
            "agentPreset.select" => Self::AgentPresetSelect,
            "agentPreset.read" => Self::AgentPresetRead,
            "agentPreset.copy" => Self::AgentPresetCopy,
            "agentPreset.openDocument" => Self::AgentPresetOpenDocument,
            "agentPreset.remove" => Self::AgentPresetRemove,
            "goal.create" => Self::GoalCreate,
            "goal.edit" => Self::GoalEdit,
            "goal.pause" => Self::GoalPause,
            "goal.resume" => Self::GoalResume,
            "goal.complete" => Self::GoalComplete,
            "goal.clear" => Self::GoalClear,
            "settings.describe" => Self::SettingsDescribe,
            "settings.openDocument" => Self::SettingsOpenDocument,
            "settings.update" => Self::SettingsUpdate,
            "settings.replace" => Self::SettingsReplace,
            "settings.mutate" => Self::SettingsMutate,
            "credentials.describe" => Self::CredentialsDescribe,
            "credentials.set" => Self::CredentialsSet,
            "credentials.unset" => Self::CredentialsUnset,
            "llm.providers" => Self::LlmProviders,
            "llm.models" => Self::LlmModels,
            "llm.discoverModels" => Self::LlmDiscoverModels,
            _ => return Err(UnknownRpcMethod(value.to_owned())),
        };
        Ok(method)
    }
}

/// Looks up a unary method and parses its Client-to-Host payload.
///
/// # Errors
///
/// Returns an unknown-method or method-specific contract error.
pub fn parse_unary_request(method: &str, value: &Value) -> anyhow::Result<Value> {
    Ok(RpcMethod::from_str(method)?.parse_request(value)?)
}

/// Looks up a unary method and parses its Host-to-Client success value.
///
/// # Errors
///
/// Returns an unknown-method or method-specific contract error.
pub fn parse_unary_value(method: &str, value: &Value) -> anyhow::Result<Value> {
    Ok(RpcMethod::from_str(method)?.parse_value(value)?)
}

/// Applies the full first-level error schema or method-specific success-value schema.
///
/// # Errors
///
/// Returns an error for an unknown method, missing success value, malformed
/// business error, or method-specific value mismatch.
pub fn parse_unary_result(
    method: &str,
    result: &RpcResult<Value>,
) -> anyhow::Result<RpcResult<Value>> {
    match result {
        RpcResult::Success { value: Some(value) } => Ok(RpcResult::Success {
            value: Some(parse_unary_value(method, value)?),
        }),
        RpcResult::Success { value: None } => {
            anyhow::bail!("missing success value for {method}")
        }
        RpcResult::Failure { error } => Ok(RpcResult::Failure {
            error: parse_rpc_error(&serde_json::to_value(error)?)?,
        }),
    }
}

fn normalized<T: Serialize>(parsed: Result<T, ContractError>) -> Result<Value, ContractError> {
    let parsed = parsed?;
    serde_json::to_value(parsed)
        .map_err(|error| ContractError::new("$", format!("normalization failed: {error}")))
}
