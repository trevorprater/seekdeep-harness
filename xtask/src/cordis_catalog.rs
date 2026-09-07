//! Rust-owned Cordis catalog generators over the pinned source tree: the
//! per-subsystem service/event reference regions, the inherited (vendor) tier
//! page, the model-facing runtime API data, the detailed core API pages, the
//! client slot catalog, and the Client inspect catalog. Every curated policy
//! table is pinned against the source scripts, so a drifted table fails before
//! anything renders.

#![expect(
    clippy::too_many_lines,
    reason = "curated policy data and the source's computeOutputs, ported step for step"
)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

use indexmap::{IndexMap, IndexSet};
use seekdeep_repository_tools::{
    client_catalog::{client_catalog_json, collect_slot_entries},
    cordis_catalog_partition::{
        WalkPartitionInput, WalkPartitionMaps, maybe_record_pair, splice_region,
        walk_partition_problems,
    },
    cordis_core_api::render_cordis_core_api_pages,
    cordis_walk::context_merge_blocks,
};
use seekdeep_typert_generator::{
    analyzer::run_with_stack,
    catalog::{
        CordisCatalogModel, CordisCatalogPolicy, CordisCatalogProjection, CordisCatalogProjector,
        InheritedEntry, ServiceEntry, ServiceMethodEntry, project_cordis_catalog,
        render_inherited_page, render_page_region,
    },
    model::TypertFace,
};
use serde::Deserialize;
use serde_json::Value;

const SUBSYSTEMS_DIR: &str = "docs/subsystems";
const OUT_INHERITED: &str = "docs/cordis-api/inherited.md";
/// The runtime API data the Host `cordis_inspect` tool embeds.
const OUT_RUNTIME_API: &str = "crates/tool-cordis/data/api-catalog.json";
/// The client slot catalog data the Client runner embeds.
const OUT_SLOT_CATALOG: &str = "crates/cordis-client-runner/data/slot-catalog.json";
/// The Client inspect catalog data the Client runner embeds.
const OUT_CLIENT_API: &str = "crates/cordis-client-runner/data/api-catalog.json";
const SOURCE_GLOBS: [&str; 2] = ["packages/*/*/src/**/*.ts", "packages/*/*/src/**/*.tsx"];

/// The owning subsystems page for every harness `ctx.<key>` service the
/// projection discovers. Fail-closed both ways.
pub const SERVICE_PAGE: &[(&str, &str)] = &[
    ("agentLoop", "core.md"),
    ("agentDefaultModel", "core.md"),
    ("agentPresets", "core.md"),
    ("agents", "core.md"),
    ("apiProxy", "typert.md"),
    ("approval", "approval.md"),
    ("attachments", "attachment.md"),
    ("shell", "shell.md"),
    ("shellEnv", "shell.md"),
    ("clientModules", "client-modules.md"),
    ("codeRuntime", "code-runtime.md"),
    ("commands", "commands.md"),
    ("compaction", "compaction.md"),
    ("cordisInspect", "extensions.md"),
    ("credentials", "credentials.md"),
    ("directoryPicker", "workspace.md"),
    ("dynamicCordisRunner", "extensions.md"),
    ("e2b", "subprocess.md"),
    ("fs", "filesystem.md"),
    ("goals", "goal.md"),
    ("webServer", "web-server.md"),
    ("invariants", "invariants.md"),
    ("llm", "llm-streaming.md"),
    ("lsp", "lsp.md"),
    ("messageFeedback", "feedback.md"),
    ("permissionPresets", "permission-presets.md"),
    ("planMode", "plan.md"),
    ("terminals", "terminal.md"),
    ("sandbox", "sandbox.md"),
    ("sandboxPolicy", "sandbox.md"),
    ("sessionPersistence", "persistence.md"),
    ("sessionQuery", "session-query.md"),
    ("sessionReferenceResolver", "session-reference.md"),
    ("sessionProjectionCache", "session-projection.md"),
    ("sessionProjections", "session-projection.md"),
    ("sessions", "session.md"),
    ("settings", "settings.md"),
    ("sessionTitle", "session-title.md"),
    ("skills", "skills.md"),
    ("spillStore", "spill.md"),
    ("storage", "storage.md"),
    ("storageDomain", "storage.md"),
    ("subagents", "subagent.md"),
    ("subprocess", "subprocess.md"),
    ("systemPrompt", "system-prompt.md"),
    ("jobs", "jobs.md"),
    ("sessionTelemetry", "session-telemetry.md"),
    ("tokenMeter", "token-meter.md"),
    ("toolResultPruner", "compaction.md"),
    ("tools", "tools.md"),
    ("typert", "typert.md"),
    ("typertGateway", "typert.md"),
    ("userQuestions", "user-questions.md"),
    ("web", "web.md"),
    ("workflowEngine", "workflow.md"),
    ("workspaceRegistry", "workspace.md"),
];

/// Context keys declared in `interface Context` merges that the rendering
/// projection cannot see, each with the reason and its documentation owner.
pub const SERVICE_WALK_EXEMPTIONS: &[(&str, &str)] = &[
    (
        "agent",
        "not a service: the DX accessor field on Agent.ctx (root accessor defaulting to undefined) — docs/subsystems/core.md owns the Agent handle",
    ),
    (
        "appExit",
        "not a service: launcher-provided bounded process-exit callback — packages/boot/cmdline/README.md owns the launcher contract",
    ),
    (
        "cmdlineArgs",
        "not a service: launcher-provided immutable app argument accessor — packages/boot/cmdline/README.md owns the launcher contract",
    ),
    (
        "configuredAgentIdentities",
        "not a service: launcher-provided boot-context value (ConfiguredAgentIdentities | undefined) — packages/core/agent-loop/README.md owns this launcher contract",
    ),
    (
        "launcherSessionQueryPath",
        "not a service: launcher-provided boot-context value (string | undefined) — packages/session-query/session-query-sqlite/README.md owns this launcher contract",
    ),
    (
        "dshHomePath",
        "not a service: boot-provided root accessor function (typeof dshHomePath | undefined) for Loader !!js config expressions — packages/boot/app-boot/README.md owns the boot contract",
    ),
    (
        "launchEnvironment",
        "not a service: launcher-provided root accessor value (LaunchEnvironmentSnapshot | undefined) — packages/util/launch-environment/README.md owns this launcher contract",
    ),
    (
        "connection",
        "interface-typed (HostConnectionHandle); implementing class HostConnectionService is declared in rpc-host.ts — packages/client/connection/README.md owns the API",
    ),
    (
        "appShell",
        "client-side interface-typed browser service — packages/client/web/README.md owns the API",
    ),
    (
        "settingsScope",
        "client-side settings-namespace transport service — packages/client/ui-settings/README.md owns the API",
    ),
    (
        "chatFileMentions",
        "client-side slot-contract accessor (ChatFileMentions) — packages/client/ui-conversation/README.md owns the API",
    ),
    (
        "commandUi",
        "client-side interface-typed browser service — packages/client/ui-commands/README.md owns the API",
    ),
    (
        "conversation",
        "client-side interface-typed browser service — packages/client/ui-conversation/README.md owns the API",
    ),
    (
        "conversationEvents",
        "client-side interface-typed registry — packages/client/runtime/README.md owns the API",
    ),
    (
        "conversationViews",
        "client-side interface-typed registry — packages/client/runtime/README.md owns the API",
    ),
    (
        "layout",
        "client-side interface-typed browser service — packages/client/ui-layout/README.md owns the API",
    ),
    (
        "locale",
        "client-side interface-typed browser service — packages/client/locale/README.md owns the API",
    ),
    (
        "modelDirectories",
        "client-side interface-typed browser service — packages/client/ui-model-selection/README.md owns the API",
    ),
    (
        "modules",
        "client-side interface-typed browser service — packages/client/modules/README.md owns the API",
    ),
    (
        "remote",
        "client-side interface-typed gateway accessor (ClientRemote) — packages/api/gateway/README.md owns the API",
    ),
    (
        "sessionLogDownload",
        "client-side browser download controller — packages/session-query/session-log-export/README.md owns the API",
    ),
    (
        "inputTriggers",
        "client-side interface-typed browser service — packages/client/ui-input-trigger/README.md owns the API",
    ),
    (
        "timer",
        "client-side dynamic-package timer service — packages/extensions/cordis-client-runner/README.md owns the API",
    ),
    (
        "slots",
        "client-side interface-typed browser service — packages/client/runtime/README.md owns the API",
    ),
    (
        "theme",
        "client-side interface-typed browser service — packages/client/ui-theme/README.md owns the API",
    ),
    (
        "workspaces",
        "client-side interface-typed browser service — packages/client/runtime/README.md owns the API",
    ),
];

/// The owning subsystems page for every harness event scope the projection renders.
pub const EVENT_SCOPE_PAGE: &[(&str, &str)] = &[
    ("agent", "core.md"),
    ("agent-loop", "core.md"),
    ("agent-preset", "core.md"),
    ("approval", "approval.md"),
    ("commands", "commands.md"),
    ("cordis", "extensions.md"),
    ("credentials", "credentials.md"),
    ("domain", "storage.md"),
    ("fs", "filesystem.md"),
    ("goal", "goal.md"),
    ("llm", "llm-streaming.md"),
    ("session", "session.md"),
    ("settings", "settings.md"),
    ("skills", "skills.md"),
    ("subagent", "subagent.md"),
    ("system-prompt", "system-prompt.md"),
    ("session-telemetry", "session-telemetry.md"),
    ("tools", "tools.md"),
    ("workflow", "workflow.md"),
];

/// Event names declared in `interface Events` merges that the rendering
/// projection cannot see, each with the reason and its documentation owner.
pub const EVENT_WALK_EXEMPTIONS: &[(&str, &str)] = &[
    (
        "command/executed",
        "client-face local command acknowledgment — packages/client/ui-commands/README.md owns the API",
    ),
    (
        "connection/reset",
        "client-face transport signal — packages/client/runtime/README.md owns the API",
    ),
    (
        "locale/change",
        "client-face locale switch signal — packages/client/locale/README.md owns the API",
    ),
    (
        "slash/input-begin-command",
        "client-face slash-input protocol — packages/client/ui-input-trigger/README.md owns the API",
    ),
    (
        "slash/input-consume-token",
        "client-face slash-input protocol — packages/client/ui-input-trigger/README.md owns the API",
    ),
    (
        "slash/input-insert-reference",
        "client-face slash-input protocol — packages/client/ui-input-trigger/README.md owns the API",
    ),
    (
        "slash/input-insert-text",
        "client-face slash-input protocol — packages/client/ui-input-trigger/README.md owns the API",
    ),
    (
        "slots/changed",
        "client-face slot invalidation signal — packages/client/runtime/README.md owns the API",
    ),
    (
        "theme/change",
        "client-face theme switch signal — packages/client/ui-theme/README.md owns the API",
    ),
];

/// One primary subsystems page per project type used by a generated signature.
pub const LINK_MAP: &[(&str, &str)] = &[
    ("Agent", "core.md"),
    ("AgentCancelCause", "core.md"),
    ("AgentFactory", "core.md"),
    ("AgentHandle", "core.md"),
    ("ModelSelection", "core.md"),
    ("AgentOptions", "core.md"),
    ("AgentStatus", "core.md"),
    ("ContentBlock", "llm-streaming.md"),
    ("CreateAgentOptions", "core.md"),
    ("GenerateOptions", "llm-streaming.md"),
    ("InboxItem", "core.md"),
    ("InboxPlacement", "core.md"),
    ("MessageId", "llm-streaming.md"),
    ("ResumeAgentOptions", "core.md"),
    ("SettleReason", "core.md"),
    ("AdapterRegistrationHandle", "llm-streaming.md"),
    ("DirectoryRegistrationHandle", "llm-streaming.md"),
    ("LlmCallConfig", "llm-streaming.md"),
    ("LlmModelContext", "llm-streaming.md"),
    ("LlmModelReasoningInfo", "llm-streaming.md"),
    ("LlmResolvedModelInfo", "llm-streaming.md"),
    ("LlmFailure", "llm-streaming.md"),
    ("LlmModelInfo", "llm-streaming.md"),
    ("LlmProviderInfo", "llm-streaming.md"),
    ("LlmConfigurableProvider", "llm-streaming.md"),
    ("LlmModelDiscoveryRequest", "llm-streaming.md"),
    ("LlmDiscoveredModel", "llm-streaming.md"),
    ("ResolvedRetryPolicy", "llm-streaming.md"),
    ("Message", "llm-streaming.md"),
    ("MessageSource", "llm-streaming.md"),
    ("MessageFeedbackDeleteRequest", "feedback.md"),
    ("MessageFeedbackDeleteResult", "feedback.md"),
    ("MessageFeedbackDeleteValue", "feedback.md"),
    ("MessageFeedbackFailure", "feedback.md"),
    ("MessageFeedbackItem", "feedback.md"),
    ("MessageFeedbackListRequest", "feedback.md"),
    ("MessageFeedbackListResult", "feedback.md"),
    ("MessageFeedbackListValue", "feedback.md"),
    ("MessageFeedbackNoteBlank", "feedback.md"),
    ("MessageFeedbackNoteTooLarge", "feedback.md"),
    ("MessageFeedbackPutRequest", "feedback.md"),
    ("MessageFeedbackPutResult", "feedback.md"),
    ("MessageFeedbackRating", "feedback.md"),
    ("MessageFeedbackRejected", "feedback.md"),
    ("MessageFeedbackSessionNotFound", "feedback.md"),
    ("MessageFeedbackSuccess", "feedback.md"),
    ("MessageFeedbackTargetNotFound", "feedback.md"),
    ("MessageFeedbackVersion", "feedback.md"),
    ("MessageFeedbackVersionConflict", "feedback.md"),
    ("UserMessage", "session.md"),
    ("PreStepDecision", "core.md"),
    ("PreStepContext", "core.md"),
    ("RequestErrorAction", "core.md"),
    ("RequestFailureContext", "core.md"),
    ("PreparedReferencedMessage", "session-reference.md"),
    ("SessionReferenceCandidate", "session-reference.md"),
    ("SessionReferenceInput", "session-reference.md"),
    ("SessionEvent", "session.md"),
    ("SessionId", "core.md"),
    ("SessionStartSource", "core.md"),
    ("SessionLogSnapshot", "session-query.md"),
    ("SessionSurfaceSnapshot", "session-query.md"),
    ("ApprovalOutcome", "approval.md"),
    ("ApprovalPolicy", "approval.md"),
    ("ApprovalRequest", "approval.md"),
    ("ApprovalService", "approval.md"),
    ("ImageAttachmentRef", "attachment.md"),
    ("SaveImageAttachment", "attachment.md"),
    ("StoredImageAttachment", "attachment.md"),
    ("ShellExecRequest", "shell.md"),
    ("ShellExecSpec", "shell.md"),
    ("ShellProcess", "shell.md"),
    ("ShellRunResult", "shell.md"),
    ("DshEnvironment", "subprocess.md"),
    ("SubprocessHandle", "subprocess.md"),
    ("SubprocessOutcome", "subprocess.md"),
    ("SubprocessOutputRead", "subprocess.md"),
    ("SubprocessOutputReader", "subprocess.md"),
    ("SubprocessSpawnSpec", "subprocess.md"),
    ("SubprocessTerminalHandle", "subprocess.md"),
    ("SubprocessTerminalSpawnSpec", "subprocess.md"),
    ("CodeRunRequest", "code-runtime.md"),
    ("CodeRunResult", "code-runtime.md"),
    ("CompactionResult", "compaction.md"),
    ("CompactionTrigger", "compaction.md"),
    ("PruneResult", "compaction.md"),
    ("FileReadOutcome", "filesystem.md"),
    ("FsDirEntry", "filesystem.md"),
    ("FsEditOutcome", "filesystem.md"),
    ("FsEditRequest", "filesystem.md"),
    ("FsInfo", "filesystem.md"),
    ("FsObservation", "filesystem.md"),
    ("FsPathInfo", "filesystem.md"),
    ("FsObservationActor", "filesystem.md"),
    ("FsTarget", "filesystem.md"),
    ("FsVersion", "filesystem.md"),
    ("FsWriteIntent", "filesystem.md"),
    ("FsWriteOutcome", "filesystem.md"),
    ("CreateGoalRequest", "goal.md"),
    ("EditGoalRequest", "goal.md"),
    ("GoalBlockReason", "goal.md"),
    ("GoalChanged", "goal.md"),
    ("GoalRef", "goal.md"),
    ("GoalView", "goal.md"),
    ("CreateGoalResult", "goal.md"),
    ("CommandDefinition", "commands.md"),
    ("CommandDescriptor", "commands.md"),
    ("CommandId", "commands.md"),
    ("CommandResult", "commands.md"),
    ("CommandSurface", "commands.md"),
    ("LspProvider", "lsp.md"),
    ("LspQueryRequest", "lsp.md"),
    ("LspQueryResult", "lsp.md"),
    ("LlmAdapter", "llm-streaming.md"),
    ("PreparedLlmCall", "llm-streaming.md"),
    ("LlmRuntime", "llm-streaming.md"),
    ("StreamChunk", "llm-streaming.md"),
    ("SkillProviderControl", "skills.md"),
    ("CreateSessionOptions", "persistence.md"),
    ("PrepareSessionOptions", "persistence.md"),
    ("SessionHeader", "persistence.md"),
    ("SessionInspection", "persistence.md"),
    ("SessionLocation", "persistence.md"),
    ("SessionPreparation", "persistence.md"),
    ("SessionPersistenceSnapshot", "persistence.md"),
    ("SessionRawArtifact", "persistence.md"),
    ("ConfinedArgv", "sandbox.md"),
    ("SandboxExecutionPolicy", "sandbox.md"),
    ("SandboxMode", "sandbox.md"),
    ("SandboxPolicy", "sandbox.md"),
    ("TerminalBackend", "terminal.md"),
    ("TerminalReadRequest", "terminal.md"),
    ("TerminalReadResult", "terminal.md"),
    ("TerminalSendOperation", "terminal.md"),
    ("TerminalSendRequest", "terminal.md"),
    ("TerminalSessionId", "terminal.md"),
    ("TerminalSessionSnapshot", "terminal.md"),
    ("TerminalSignal", "terminal.md"),
    ("TerminalSignalResult", "terminal.md"),
    ("TerminalSpawnRequest", "terminal.md"),
    ("TerminalSpawnResult", "terminal.md"),
    ("SandboxPolicyRequest", "sandbox.md"),
    ("ScopeKey", "scope.md"),
    ("Scoped", "scope.md"),
    ("EpochHeader", "session.md"),
    ("Session", "session.md"),
    ("SessionEventMap", "session.md"),
    ("TurnEndReason", "session.md"),
    ("TurnTrigger", "session.md"),
    ("SessionEventReadRequest", "session-query.md"),
    ("SessionEventRecord", "session-query.md"),
    ("SessionEventResultFilter", "session-query.md"),
    ("SessionEventSearchDocument", "session-query.md"),
    ("SessionEventSearchHit", "session-query.md"),
    ("SessionEventSearchPage", "session-query.md"),
    ("SessionEventSearchRequest", "session-query.md"),
    ("SessionEventTrace", "session-query.md"),
    ("SessionEventTraceObservation", "session-query.md"),
    ("SessionEventTraceRequest", "session-query.md"),
    ("SessionEventWindow", "session-query.md"),
    ("SessionLineageTrace", "session-query.md"),
    ("SessionRecord", "session-query.md"),
    ("SessionResultFilter", "session-query.md"),
    ("SessionSearchExecContext", "session-query.md"),
    ("SessionSearchHit", "session-query.md"),
    ("SessionSearchPage", "session-query.md"),
    ("SessionSearchRequest", "session-query.md"),
    ("SessionTitleObservation", "session-query.md"),
    ("SessionTitleObservationResult", "session-query.md"),
    ("SessionTitleProvider", "session-title.md"),
    ("SessionTitleSnapshot", "session-title.md"),
    ("SkillCatalogSnapshot", "skills.md"),
    ("SkillDefinition", "skills.md"),
    ("SkillLookupOptions", "skills.md"),
    ("SkillProvider", "skills.md"),
    ("SkillProviderObservation", "skills.md"),
    ("SkillRegistration", "skills.md"),
    ("SkillViewOptions", "skills.md"),
    ("SkillSummary", "skills.md"),
    ("SaveTextSpill", "spill.md"),
    ("SpillRef", "spill.md"),
    ("ContinuableCreateRequest", "subagent.md"),
    ("ContinuableCreateSpec", "subagent.md"),
    ("ContinuableSetupContribution", "subagent.md"),
    ("ContinuableStart", "subagent.md"),
    ("ContinuableStartSpec", "subagent.md"),
    ("CoordinatorMessageSource", "subagent.md"),
    ("SubagentDescendantListEntry", "subagent.md"),
    ("SubagentFollowupOptions", "subagent.md"),
    ("SubagentInterruptAuthority", "subagent.md"),
    ("SubagentListEntry", "subagent.md"),
    ("SubagentProvider", "subagent.md"),
    ("SubagentReportDelivery", "subagent.md"),
    ("SubagentReportMessageSource", "subagent.md"),
    ("SubagentReportOptions", "subagent.md"),
    ("SubagentRun", "subagent.md"),
    ("SubagentRuntime", "subagent.md"),
    ("SubagentStartRequest", "subagent.md"),
    ("AssembleContext", "system-prompt.md"),
    ("PromptContext", "system-prompt.md"),
    ("PromptSection", "system-prompt.md"),
    ("SystemPrompt", "system-prompt.md"),
    ("ToolProviderResult", "system-prompt.md"),
    ("JobDoneListener", "jobs.md"),
    ("JobId", "jobs.md"),
    ("JobRead", "jobs.md"),
    ("JobSnapshot", "jobs.md"),
    ("JobStart", "jobs.md"),
    ("JobsChangedListener", "jobs.md"),
    ("TokenMeasurement", "token-meter.md"),
    ("CodeDispatchLog", "tools.md"),
    ("PostToolDecision", "tools.md"),
    ("PreToolDecision", "tools.md"),
    ("ToolDefinition", "tools.md"),
    ("ToolExecution", "tools.md"),
    ("ToolDispatchExecution", "tools.md"),
    ("ToolExecutionInput", "tools.md"),
    ("ToolExecutionMode", "tools.md"),
    ("ToolExecutionResult", "tools.md"),
    ("ToolExecutionToken", "tools.md"),
    ("ToolGuard", "tools.md"),
    ("ToolPresentationMode", "tools.md"),
    ("ToolRuntime", "tools.md"),
    ("ToolRestriction", "tools.md"),
    ("ToolSchema", "tools.md"),
    ("SettingsNamespace", "settings.md"),
    ("SettingsRegisterOptions", "settings.md"),
    ("SettingsScope", "settings.md"),
    ("SettingsDescriptor", "settings.md"),
    ("SettingsPathOp", "settings.md"),
    ("SettingsDescribeOptions", "settings.md"),
    ("SettingsUpdateSource", "settings.md"),
    ("CredentialRef", "credentials.md"),
    ("CredentialInfo", "credentials.md"),
    ("ResolvedCredential", "credentials.md"),
    ("AskUserQuestionAnswer", "user-questions.md"),
    ("AskUserQuestionRequest", "user-questions.md"),
    ("UserQuestionProvider", "user-questions.md"),
    ("WebFetchProvider", "web.md"),
    ("WebFetchRequest", "web.md"),
    ("WebFetchResult", "web.md"),
    ("WebSearchProvider", "web.md"),
    ("WebSearchRequest", "web.md"),
    ("WebSearchResult", "web.md"),
    ("WorkflowRun", "workflow.md"),
    ("PresetOption", "permission-presets.md"),
    ("PresetSpec", "permission-presets.md"),
    ("InvariantInstaller", "invariants.md"),
    ("WebRoute", "web-server.md"),
    ("StorageBackend", "storage.md"),
    ("StorageForms", "storage.md"),
    ("Domain", "storage.md"),
    ("DomainSpec", "storage.md"),
    ("DomainChanged", "storage.md"),
    ("DomainFacility", "storage.md"),
    ("Workspace", "workspace.md"),
    ("WorkspaceId", "workspace.md"),
    ("WebBootGraph", "client-modules.md"),
    ("SessionTelemetryRecord", "session-telemetry.md"),
    ("WorkflowRunInfo", "workflow.md"),
    ("WorkflowStartRequest", "workflow.md"),
    ("ProjectionDefinition", "session-projection.md"),
    ("SessionProjectionMap", "session-projection.md"),
    ("ProjectionChangeListener", "session-projection.md"),
    ("ProjectionSnapshot", "session-projection.md"),
    ("ProjectionCheckpoint", "session-projection.md"),
    ("DirectoryPickerCapability", "workspace.md"),
    ("TypertContribution", "invariants.md"),
    ("TypertFace", "invariants.md"),
    ("TypertPackageFilter", "invariants.md"),
    ("TypertPackageRecord", "invariants.md"),
    ("TypertSchemaFilter", "invariants.md"),
    ("TypertSchemaRecord", "invariants.md"),
];

/// TypeScript lib and pinned framework types with no repository-owned data page.
pub const FOUNDATION_TYPE_NAMES: &[&str] = &[
    "AbortSignal",
    "AsyncIterable",
    "Context",
    "Error",
    "Map",
    "Partial",
    "Pick",
    "Promise",
    "Record",
    "Readonly",
    "Uint8Array",
];

/// Project types deliberately documented outside the subsystems catalog.
pub const TYPE_LINK_EXEMPTIONS: &[(&str, &str)] = &[
    (
        "z",
        "schemastery schema constructor is owned by vendor/schemastery (vendored upstream)",
    ),
    (
        "BeginCommandRequest",
        "event-local request contract is owned by packages/client/ui-input-trigger/src/types.ts",
    ),
    (
        "InsertReferenceRequest",
        "event-local request contract is owned by packages/client/ui-input-trigger/src/types.ts",
    ),
    (
        "ConsumeTokenRequest",
        "event-local request contract is owned by packages/client/ui-input-trigger/src/types.ts",
    ),
    (
        "InsertTextRequest",
        "event-local request contract is owned by packages/client/ui-input-trigger/src/types.ts",
    ),
    (
        "AgentHandle",
        "agent ownership handle is owned by packages/core/agent/README.md",
    ),
    (
        "AgentPreset",
        "discovered preset record is owned by packages/preset/agent-presets/README.md",
    ),
    (
        "PresetMetadata",
        "preset display text is owned by packages/preset/agent-presets/README.md",
    ),
    (
        "BashEnvContributor",
        "service-local extension type is owned by packages/shell/tool-bash/src/index.ts",
    ),
    (
        "BashEnvVariableInfo",
        "service-local metadata type is owned by packages/shell/tool-bash/src/index.ts",
    ),
    (
        "CompactionAgentContext",
        "compaction service input is owned by packages/compaction/compaction/src/index.ts",
    ),
    (
        "ManualCompactAgentContext",
        "manual compaction service input is owned by packages/compaction/compaction/src/index.ts",
    ),
    (
        "ClientResponse",
        "wire response message is owned by packages/host/apiproxy/src/api/rpc.ts",
    ),
    (
        "ApprovalRequestId",
        "dynamic Plugin approval identity is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisErrorDetails",
        "Cordis runtime error payload is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisInspectPlatform",
        "Cordis inspect platform identity is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisInspectProviderManifest",
        "Cordis inspect provider manifest is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisInspectProviderView",
        "Cordis inspect provider view is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisInspectQueryRequest",
        "Cordis inspect transport payload is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisInspectQueryResolution",
        "Cordis inspect query result is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisInspectQueryResolved",
        "Cordis inspect transport payload is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisInspectRequestId",
        "Cordis inspect request identity is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisInspectResolveAck",
        "Cordis inspect resolution acknowledgement is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisDynamicPackageId",
        "dynamic Package identity is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisDynamicPluginId",
        "dynamic Plugin identity is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisDynamicPluginRunId",
        "dynamic Plugin run identity is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "CordisDynamicRunMode",
        "dynamic Plugin activation mode is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisClientSource",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisDefineReceipt",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisDefineRequest",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisHostHalfResult",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisInventoryRow",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisInvokeResult",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisPackageInspection",
        "dynamic Package source inspection is owned by packages/extensions/cordis-host-runner/src/registry.ts",
    ),
    (
        "DynamicCordisPluginInspection",
        "dynamic Plugin inspection is owned by packages/extensions/cordis-host-runner/src/registry.ts",
    ),
    (
        "DynamicCordisRequestResolved",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisRetracted",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisRunRequest",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisPackage",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisReference",
        "dynamic Plugin reference is owned by packages/extensions/cordis-host-runner/src/registry.ts",
    ),
    (
        "DynamicCordisRenderFailure",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisResolveAck",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisRunResolution",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisRunResponse",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisSnapshotRow",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisStopResponse",
        "dynamic Plugin stop result is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "DynamicCordisUndefineReceipt",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "HostCordisInspectProviderRegistration",
        "Host inspect provider registration is owned by packages/extensions/cordis-host-runner/src/inspect-registry.ts",
    ),
    (
        "DomainImpl",
        "domain implementation contract is owned by packages/storage/storage-domain/README.md",
    ),
    (
        "CommandExecution",
        "executor return contract is owned by packages/interaction/commands/src/index.ts",
    ),
    (
        "z.core.JSONSchema.BaseSchema",
        "zod projection output is owned by the zod v4 API",
    ),
    (
        "z.core.ToJSONSchemaParams",
        "zod projection parameters are owned by the zod v4 API",
    ),
    (
        "TypertDisposer",
        "Typert lifecycle contract is owned by packages/typert/protocol/README.md",
    ),
    (
        "InvokeRemoteRequest",
        "gateway invocation contract is owned by packages/api/gateway/README.md",
    ),
    (
        "LocaleDict",
        "service-local dictionary fields are owned by packages/client/i18n/src/index.ts",
    ),
    (
        "ThemeTokens",
        "service-local token dictionary is owned by packages/client/ui-theme/src/index.ts",
    ),
    (
        "Translate",
        "service-local bound translator is owned by packages/client/i18n/src/index.ts",
    ),
    (
        "WebUpgradeRoute",
        "upgrade route registration contract is owned by packages/host/webserver/src/index.ts",
    ),
    (
        "InvariantRegistration",
        "service-local lifecycle handle is owned by packages/runtime-diagnostics/invariants/README.md",
    ),
    (
        "JsonValue",
        "JSON value union is owned by packages/core/session/src/json.ts",
    ),
    (
        "KnobState",
        "projection unit state fields are owned by packages/interaction/permission-presets/README.md",
    ),
    (
        "PermissionSelect",
        "permissions projection payload is owned by packages/interaction/permission-presets/src/types.ts",
    ),
    (
        "PromptAssembly",
        "assembly result is owned by packages/core/system-prompt/README.md",
    ),
    (
        "RequestRunId",
        "dynamic-package payload contract is owned by packages/extensions/cordis-host-runner/src/types.ts",
    ),
    (
        "RpcReceipt",
        "carrier-layer receipt is owned by packages/host/apiproxy/src/api/rpc.ts",
    ),
    (
        "Sandbox",
        "external E2B SDK handle is owned by packages/e2b/e2b/README.md",
    ),
    (
        "SessionForkSource",
        "service-local fork input is owned by packages/core/session/src/index.ts",
    ),
    (
        "SubagentRunEndInfo",
        "event payload contract is owned by packages/subagent/subagent/src/types.ts",
    ),
    (
        "SubagentRunInfo",
        "event payload contract is owned by packages/subagent/subagent/src/types.ts",
    ),
    (
        "WorkflowAgentEndInfo",
        "event-local snapshot is owned by packages/workflow/workflow/src/index.ts",
    ),
    (
        "WorkflowAgentInfo",
        "event-local snapshot is owned by packages/workflow/workflow/src/index.ts",
    ),
    (
        "WorkflowResultInfo",
        "event-local snapshot is owned by packages/workflow/workflow/src/index.ts",
    ),
];

/// Client services and the methods a browser half may reach through `cordis_inspect what:"client"`.
pub const CLIENT_SERVICES: &[(&str, &[&str])] = &[
    ("layout", &["toggleSidebar", "openDetails", "closeDetails"]),
    (
        "locale",
        &[
            "getLocale",
            "getSnapshot",
            "subscribe",
            "setLocale",
            "register",
            "bind",
        ],
    ),
    (
        "sessions",
        &[
            "open",
            "openSubagent",
            "setSubagentCatalogOpen",
            "refreshSubagents",
            "search",
            "fork",
            "scope",
            "binding",
        ],
    ),
    ("slots", &["register", "inject"]),
    (
        "theme",
        &["getTheme", "setTheme", "register", "overrideTokens"],
    ),
    (
        "workspaces",
        &[
            "connectWorkspace",
            "startSession",
            "create",
            "pickDirectory",
            "listDirectory",
            "createDirectory",
            "openPath",
            "rename",
            "delete",
            "insertSessionBefore",
            "archiveSession",
        ],
    ),
];

/// Client events served by the Client inspect catalog.
pub const CLIENT_EVENTS: &[&str] = &[
    "connection/reset",
    "locale/change",
    "slots/changed",
    "theme/change",
];

fn table(entries: &[(&str, &str)]) -> IndexMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn inherited(entries: &[(&str, &str, &str)]) -> Vec<InheritedEntry> {
    entries
        .iter()
        .map(|(name, summary, source)| InheritedEntry {
            name: (*name).to_owned(),
            summary: (*summary).to_owned(),
            source: (*source).to_owned(),
        })
        .collect()
}

fn timer_method(signature: &str, js_doc: &str) -> ServiceMethodEntry {
    ServiceMethodEntry {
        kind: None,
        signature: signature.to_owned(),
        js_doc: js_doc.to_owned(),
    }
}

/// Repository data policy consumed by the Cordis catalog projector.
#[must_use]
pub fn cordis_catalog_policy() -> CordisCatalogPolicy {
    CordisCatalogPolicy {
        linked_type_pages: table(LINK_MAP),
        foundation_type_names: FOUNDATION_TYPE_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        type_link_exemptions: table(TYPE_LINK_EXEMPTIONS),
        runtime_service_exclusions: Some(
            ["cordisInspect", "dynamicCordisRunner"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        ),
        runtime_services: Some(vec![ServiceEntry {
            key: "timer".to_owned(),
            type_name: "TimerService".to_owned(),
            is_abstract: false,
            doc: "Disposable timer helpers mixed into Cordis contexts.".to_owned(),
            source: "vendor/timer/src/index.ts:12".to_owned(),
            methods: vec![
                timer_method(
                    "timeout(callback: () => void, delay: number): () => void",
                    "/** Run a callback once and return its disposer. */",
                ),
                timer_method(
                    "timeout(delay: number): Promise<void>",
                    "/** Resolve after a delay; disposal rejects the pending promise. */",
                ),
                timer_method(
                    "interval(callback: () => void, delay: number): () => void",
                    "/** Run a callback repeatedly and return its disposer. */",
                ),
                timer_method(
                    "interval<R = any>(delay: number): AsyncIterableIterator<void, R, void>",
                    "/** Return an async iterator of timer ticks. */",
                ),
                timer_method(
                    "throttle<F extends (...args: any[]) => void>(callback: F, delay: number, noTrailing?: boolean): F & { dispose: () => void }",
                    "/** Return a throttled function whose timer is disposed with the current fiber. */",
                ),
                timer_method(
                    "debounce<F extends (...args: any[]) => void>(callback: F, delay: number): F & { dispose: () => void }",
                    "/** Return a debounced function whose timer is disposed with the current fiber. */",
                ),
            ],
        }]),
        inherited_events: inherited(&[
            (
                "internal/plugin",
                "A plugin fiber was created.",
                "vendor/cordis/src/events.ts:328",
            ),
            (
                "internal/status",
                "A fiber changed lifecycle state.",
                "vendor/cordis/src/events.ts:330",
            ),
            (
                "internal/service",
                "Interception hook for a service binding (no core producer).",
                "vendor/cordis/src/events.ts:332",
            ),
            (
                "internal/update",
                "Waterfall: a fiber config update is being applied.",
                "vendor/cordis/src/events.ts:334",
            ),
            (
                "internal/get",
                "Waterfall: a service is being read from the store.",
                "vendor/cordis/src/events.ts:336",
            ),
            (
                "internal/set",
                "Waterfall: a service is being written to the store.",
                "vendor/cordis/src/events.ts:338",
            ),
            (
                "internal/listener",
                "A listener was registered.",
                "vendor/cordis/src/events.ts:340",
            ),
            (
                "internal/dispatch",
                "An event is being dispatched to listeners.",
                "vendor/cordis/src/events.ts:342",
            ),
            (
                "hmr/change",
                "A watched source file changed on disk.",
                "vendor/hmr/src/index.ts:20",
            ),
            (
                "hmr/reload",
                "Plugins are being reloaded after a change.",
                "vendor/hmr/src/index.ts:21",
            ),
            (
                "exit",
                "The process is exiting on a signal.",
                "vendor/loader/src/index.ts:23",
            ),
            (
                "loader/config-update",
                "The loader config tree changed.",
                "vendor/loader/src/index.ts:24",
            ),
            (
                "loader/entry-init",
                "A config entry is being initialized.",
                "vendor/loader/src/index.ts:25",
            ),
            (
                "loader/partial-dispose",
                "An entry is being partially disposed on reload.",
                "vendor/loader/src/index.ts:26",
            ),
            (
                "loader/patch-context",
                "A context is being patched during a reload.",
                "vendor/loader/src/index.ts:27",
            ),
        ]),
        inherited_services: inherited(&[
            (
                "ctx.on / ctx.once",
                "Register an event listener (disposable).",
                "vendor/cordis/src/events.ts:34",
            ),
            (
                "ctx.emit / ctx.parallel / ctx.serial / ctx.bail / ctx.waterfall",
                "Dispatch an event (sync / awaited / first-bail / short-circuit chain).",
                "vendor/cordis/src/events.ts:34",
            ),
            (
                "ctx.plugin / ctx.inject",
                "Load a plugin / declare required services.",
                "vendor/cordis/src/registry.ts:164",
            ),
            (
                "ctx.effect",
                "Register a disposable side effect tied to the fiber.",
                "vendor/cordis/src/fiber.ts:9",
            ),
            (
                "ctx.get / ctx.set / ctx.provide / ctx.accessor / ctx.mixin",
                "Low-level service-store access and binding.",
                "vendor/cordis/src/reflect.ts:7",
            ),
            (
                "ctx.extend / ctx.isolate / ctx.intercept",
                "Derive a child context (scoped services / isolation / interception).",
                "vendor/cordis/src/context.ts:42",
            ),
            (
                "ctx.root / ctx.scope / ctx.fiber / ctx.registry / ctx.reflect / ctx.events / ctx.logger",
                "Ambient handles onto the running context graph.",
                "vendor/cordis/src/context.ts:16",
            ),
            (
                "ctx.timer (+ interval / timeout / throttle / debounce)",
                "Disposable timer helpers. The `timer` key is provided at runtime; the four supported helpers are mixed onto ctx directly (declared via Pick).",
                "vendor/timer/src/index.ts:4",
            ),
            (
                "ctx.loader",
                "The config Loader that booted the app (present under the loader).",
                "vendor/loader/src/index.ts:30",
            ),
            (
                "ctx.hmr",
                "The hot-module-reload watcher (present under the hmr plugin).",
                "vendor/hmr/src/index.ts:15",
            ),
        ]),
    }
}

/// The curated maps the partition backstop enforces.
#[must_use]
pub fn partition_maps() -> WalkPartitionMaps {
    WalkPartitionMaps {
        service_page: table(SERVICE_PAGE),
        service_walk_exemptions: table(SERVICE_WALK_EXEMPTIONS),
        event_scope_page: table(EVENT_SCOPE_PAGE),
        event_walk_exemptions: table(EVENT_WALK_EXEMPTIONS),
    }
}

/// Product identity applied to source-identity documentation before it lands
/// in the port: the data rename plus the manifest field and scope-scan tag
/// spellings the committed pages carry.
#[must_use]
pub fn document_identity(text: &str) -> String {
    target_identity(text)
        .replace("dsh.client", "seekdeep.client")
        .replace("@dshScopeScan", "@seekdeepScopeScan")
}

/// Product identity applied to source-identity data before it lands in the port.
#[must_use]
pub fn target_identity(text: &str) -> String {
    text.replace("DSH Node.js process", "SeekDeep Harness Host process")
        .replace("DSH process", "SeekDeep Harness process")
        .replace("DSH objects", "SeekDeep Harness objects")
        .replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
        .replace("dsh-", "seekdeep-")
        .replace("DshEnvironment", "SeekdeepEnvironment")
        .replace("DSH_", "SEEKDEEP_")
}

const POLICY_ORACLE: &str = r"
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const { createRequire } = require('node:module');
const root = resolve(process.argv[1]);
const sourceRequire = createRequire(resolve(root, 'package.json'));
const ts = sourceRequire('typescript');
function consts(rel, names) {
  const path = resolve(root, rel);
  const text = readFileSync(path, 'utf8');
  const file = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true);
  const declarations = file.statements.filter(statement => ts.isVariableStatement(statement)
    && statement.declarationList.declarations.some(declaration => ts.isIdentifier(declaration.name) && names.includes(declaration.name.text)))
    .map(statement => statement.getText(file).replace(/^export\s+/u, '')).join('\n');
  return new Function(ts.transpileModule(declarations + '\nreturn {' + names.join(',') + '};', {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None },
  }).outputText)();
}
const catalog = consts('scripts/gen-cordis-catalog.ts', ['SERVICE_PAGE', 'SERVICE_WALK_EXEMPTIONS', 'EVENT_SCOPE_PAGE', 'EVENT_WALK_EXEMPTIONS', 'LINK_MAP', 'FOUNDATION_TYPE_NAMES', 'TYPE_LINK_EXEMPTIONS', 'CORDIS_CATALOG_POLICY']);
const inspect = consts('scripts/gen-cordis-inspect-catalog.ts', ['CLIENT_SERVICES', 'CLIENT_EVENTS']);
process.stdout.write(JSON.stringify({ ...catalog, ...inspect }, (key, value) => value instanceof Set ? [...value] : value));
";

/// The curated tables as the pinned source scripts declare them.
#[derive(Debug, Deserialize)]
pub struct SourcePolicyTables {
    /// `SERVICE_PAGE`.
    #[serde(rename = "SERVICE_PAGE")]
    pub service_page: IndexMap<String, String>,
    /// `SERVICE_WALK_EXEMPTIONS`.
    #[serde(rename = "SERVICE_WALK_EXEMPTIONS")]
    pub service_walk_exemptions: IndexMap<String, String>,
    /// `EVENT_SCOPE_PAGE`.
    #[serde(rename = "EVENT_SCOPE_PAGE")]
    pub event_scope_page: IndexMap<String, String>,
    /// `EVENT_WALK_EXEMPTIONS`.
    #[serde(rename = "EVENT_WALK_EXEMPTIONS")]
    pub event_walk_exemptions: IndexMap<String, String>,
    /// `CORDIS_CATALOG_POLICY`.
    #[serde(rename = "CORDIS_CATALOG_POLICY")]
    pub cordis_catalog_policy: CordisCatalogPolicy,
    /// `CLIENT_SERVICES`.
    #[serde(rename = "CLIENT_SERVICES")]
    pub client_services: IndexMap<String, Vec<String>>,
    /// `CLIENT_EVENTS`.
    #[serde(rename = "CLIENT_EVENTS")]
    pub client_events: Vec<String>,
}

/// Reads the curated tables from the pinned source scripts.
///
/// # Errors
/// Returns a Node failure or unparsable output.
pub fn source_policy_tables(source_root: &Path) -> anyhow::Result<SourcePolicyTables> {
    let output = Command::new("node")
        .args(["-e", POLICY_ORACLE])
        .arg(source_root)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "gen-cordis-catalog: reading the source policy tables failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Fails when any Rust table differs from the pinned source script's.
///
/// # Errors
/// Names every drifted table.
pub fn verify_policy_matches_source(source_root: &Path) -> anyhow::Result<()> {
    let tables = source_policy_tables(source_root)?;
    let maps = partition_maps();
    let mut drift = Vec::new();
    if tables.service_page != maps.service_page {
        drift.push("SERVICE_PAGE");
    }
    if tables.service_walk_exemptions != maps.service_walk_exemptions {
        drift.push("SERVICE_WALK_EXEMPTIONS");
    }
    if tables.event_scope_page != maps.event_scope_page {
        drift.push("EVENT_SCOPE_PAGE");
    }
    if tables.event_walk_exemptions != maps.event_walk_exemptions {
        drift.push("EVENT_WALK_EXEMPTIONS");
    }
    if tables.cordis_catalog_policy != cordis_catalog_policy() {
        drift.push("CORDIS_CATALOG_POLICY");
    }
    if tables.client_services != client_services() {
        drift.push("CLIENT_SERVICES");
    }
    if tables.client_events != CLIENT_EVENTS {
        drift.push("CLIENT_EVENTS");
    }
    if drift.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "gen-cordis-catalog: Rust policy tables differ from the pinned source scripts: {}",
            drift.join(", ")
        )
    }
}

fn client_services() -> IndexMap<String, Vec<String>> {
    CLIENT_SERVICES
        .iter()
        .map(|(key, methods)| {
            (
                (*key).to_owned(),
                methods.iter().map(|method| (*method).to_owned()).collect(),
            )
        })
        .collect()
}

/// One generated artifact: Markdown compared byte for byte, JSON compared as data.
#[derive(Clone, Debug, PartialEq)]
pub enum Artifact {
    /// Exact text.
    Text(String),
    /// JSON data, written pretty-printed with a trailing newline.
    Json(Value),
}

impl Artifact {
    fn rendered(&self) -> anyhow::Result<String> {
        Ok(match self {
            Self::Text(text) => text.clone(),
            Self::Json(value) => format!("{}\n", serde_json::to_string_pretty(value)?),
        })
    }

    fn matches(&self, committed: Option<&str>) -> bool {
        match (self, committed) {
            (Self::Text(text), Some(committed)) => text == committed,
            (Self::Json(value), Some(committed)) => {
                serde_json::from_str::<Value>(committed).is_ok_and(|parsed| &parsed == value)
            }
            (_, None) => false,
        }
    }
}

fn project(source_root: &Path, face: TypertFace) -> anyhow::Result<CordisCatalogProjection> {
    let policy = cordis_catalog_policy();
    let root = source_root.to_owned();
    run_with_stack(move || project_cordis_catalog(&root, &policy, face)).map_err(Into::into)
}

fn renamed_json<T: serde::Serialize>(value: &T) -> anyhow::Result<Value> {
    Ok(serde_json::from_str(&target_identity(
        &serde_json::to_string(value)?,
    ))?)
}

/// Compute every generated artifact of the Cordis catalog: the inherited-tier
/// page, the runtime API data, the per-page bilingual regions, and the core
/// API pages. Partition and page problems are aggregated errors.
///
/// # Errors
/// Returns analysis failures, policy drift, partition violations, and page violations.
pub fn compute_outputs(
    repo_root: &Path,
    source_root: &Path,
) -> anyhow::Result<Vec<(String, Artifact)>> {
    verify_policy_matches_source(source_root)?;
    let projection = project(source_root, TypertFace::Host)?;
    let policy = cordis_catalog_policy();
    let projector =
        CordisCatalogProjector::new(&projection.face, &projection.source_declarations, &policy);
    let model = &projection.model;

    let mut declared_keys = IndexMap::new();
    let mut declared_events = IndexMap::new();
    for block in context_merge_blocks(source_root, &SOURCE_GLOBS)? {
        for key in block.context_keys.keys() {
            declared_keys
                .entry(key.clone())
                .or_insert_with(|| block.rel.clone());
        }
        for name in &block.event_names {
            declared_events
                .entry(name.clone())
                .or_insert_with(|| block.rel.clone());
        }
    }
    let input = WalkPartitionInput {
        rendered_keys: model
            .services
            .iter()
            .map(|service| (service.key.clone(), service.source.clone()))
            .collect(),
        rendered_scopes: model
            .events
            .iter()
            .map(|event| event.scope.clone())
            .collect(),
        rendered_event_names: model
            .events
            .iter()
            .map(|event| event.name.clone())
            .collect(),
        declared_keys,
        declared_events,
    };
    let maps = partition_maps();
    let mut problems = walk_partition_problems(&input, &maps);
    if !problems.is_empty() {
        anyhow::bail!(
            "gen-cordis-catalog: {} partition violation(s):\n{}",
            problems.len(),
            problems
                .iter()
                .map(|problem| format!("  {problem}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let mut pages = maps
        .service_page
        .values()
        .chain(maps.event_scope_page.values())
        .cloned()
        .collect::<IndexSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    pages.sort();
    let mut outputs = vec![
        (
            OUT_INHERITED.to_owned(),
            Artifact::Text(document_identity(&render_inherited_page(&policy))),
        ),
        (
            OUT_RUNTIME_API.to_owned(),
            Artifact::Json(renamed_json(&projector.runtime_catalog(model)?)?),
        ),
    ];
    for page in &pages {
        let services = model
            .services
            .iter()
            .filter(|service| maps.service_page.get(&service.key) == Some(page))
            .cloned()
            .collect::<Vec<_>>();
        let events = model
            .events
            .iter()
            .filter(|event| maps.event_scope_page.get(&event.scope) == Some(page))
            .cloned()
            .collect::<Vec<_>>();
        let region = document_identity(&render_page_region(page, &services, &events, &policy)?);
        for side in [page.clone(), page.replace(".md", ".zh.md")] {
            let rel = format!("{SUBSYSTEMS_DIR}/{side}");
            let Ok(current) = std::fs::read_to_string(repo_root.join(&rel)) else {
                problems.push(format!("{rel}: mapped subsystems page does not exist."));
                continue;
            };
            match splice_region(&current, &region) {
                Ok(spliced) => outputs.push((rel, Artifact::Text(spliced))),
                Err(error) => problems.push(format!("{rel}: {error}")),
            }
        }
    }
    if !problems.is_empty() {
        anyhow::bail!(
            "gen-cordis-catalog: {} page violation(s):\n{}",
            problems.len(),
            problems
                .iter()
                .map(|problem| format!("  {problem}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    for (out, page) in render_cordis_core_api_pages(source_root)? {
        outputs.push((out, Artifact::Text(document_identity(&page))));
    }
    Ok(outputs)
}

fn check_outputs(
    gate: &str,
    regenerate: &str,
    repo_root: &Path,
    outputs: &[(String, Artifact)],
) -> anyhow::Result<()> {
    let stale = outputs
        .iter()
        .filter(|(out, artifact)| {
            !artifact.matches(std::fs::read_to_string(repo_root.join(out)).ok().as_deref())
        })
        .map(|(out, _)| out.as_str())
        .collect::<Vec<_>>();
    if stale.is_empty() {
        println!(
            "{gate}: {} generated file(s)/region(s) are up to date.",
            outputs.len()
        );
        return Ok(());
    }
    anyhow::bail!(
        "{gate}: stale — {}. Run `{regenerate}` and commit the result.",
        stale.join(", ")
    )
}

/// Regenerates every Cordis catalog artifact, or with `check` fails when any is stale.
///
/// # Errors
/// Returns computation failures, stale artifacts under `check`, and write failures.
pub fn run(repo_root: &Path, source_root: &Path, check: bool) -> anyhow::Result<()> {
    let outputs = compute_outputs(repo_root, source_root)?;
    if check {
        return check_outputs(
            "gen-cordis-catalog",
            "cargo xtask cordis-catalog",
            repo_root,
            &outputs,
        );
    }
    let mut before: HashMap<String, Vec<u8>> = HashMap::new();
    for (out, _) in &outputs {
        if let Ok(bytes) = std::fs::read(repo_root.join(out)) {
            before.insert(out.clone(), bytes);
        }
    }
    let mut changed = 0;
    for (out, artifact) in &outputs {
        let rendered = artifact.rendered()?;
        if before
            .get(out)
            .is_some_and(|previous| previous == rendered.as_bytes())
        {
            continue;
        }
        let destination = repo_root.join(out);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(destination, rendered)?;
        changed += 1;
    }
    let mut recorded = 0;
    let maps = partition_maps();
    let pages = maps
        .service_page
        .values()
        .chain(maps.event_scope_page.values())
        .cloned()
        .collect::<IndexSet<_>>();
    for page in pages {
        let rel = format!("{SUBSYSTEMS_DIR}/{page}");
        let zh_rel = rel.replace(".md", ".zh.md");
        let wrote_either = [&rel, &zh_rel].into_iter().any(|side| {
            before.get(side).is_some_and(|previous| {
                std::fs::read(repo_root.join(side)).is_ok_and(|current| current != *previous)
            })
        });
        if wrote_either && maybe_record_pair(&rel, &before, repo_root)? {
            recorded += 1;
        }
    }
    println!(
        "gen-cordis-catalog: {} artifact(s) computed, {changed} written, {recorded} pair record(s) refreshed.",
        outputs.len()
    );
    Ok(())
}

fn write_or_check(
    gate: &str,
    regenerate: &str,
    repo_root: &Path,
    out: &str,
    artifact: &Artifact,
    check: bool,
) -> anyhow::Result<()> {
    let destination: PathBuf = repo_root.join(out);
    if check {
        if artifact.matches(std::fs::read_to_string(&destination).ok().as_deref()) {
            println!("{gate}: {out} is up to date.");
            return Ok(());
        }
        anyhow::bail!("{gate}: stale — {out}. Run `{regenerate}` and commit the result.");
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&destination, artifact.rendered()?)?;
    println!("{gate}: wrote {out}.");
    Ok(())
}

/// Regenerates the client slot catalog data, or with `check` fails when it is stale.
///
/// # Errors
/// Returns scan and contract failures, a stale artifact under `check`, and write failures.
pub fn run_client_catalog(repo_root: &Path, source_root: &Path, check: bool) -> anyhow::Result<()> {
    let entries = collect_slot_entries(source_root)?;
    let artifact = Artifact::Json(renamed_json(&client_catalog_json(&entries))?);
    write_or_check(
        "gen-client-catalog",
        "cargo xtask client-catalog",
        repo_root,
        OUT_SLOT_CATALOG,
        &artifact,
        check,
    )
}

fn method_name(method: &ServiceMethodEntry) -> Option<String> {
    let mut rest = method.signature.as_str();
    for prefix in ["declare", "readonly", "async"] {
        if let Some(after) = rest.strip_prefix(prefix)
            && let Some(first) = after.chars().next()
            && first.is_whitespace()
        {
            rest = after.trim_start();
        }
    }
    let mut characters = rest.chars();
    let first = characters.next().filter(|character| {
        character.is_ascii_alphabetic() || *character == '_' || *character == '$'
    })?;
    let mut name = String::from(first);
    name.extend(characters.take_while(|character| {
        character.is_ascii_alphanumeric() || *character == '_' || *character == '$'
    }));
    Some(name)
}

/// The Client subset of a projected model: curated services and their reachable methods, plus the client events.
#[must_use]
pub fn client_model(model: &CordisCatalogModel) -> CordisCatalogModel {
    let allowed = client_services();
    CordisCatalogModel {
        services: model
            .services
            .iter()
            .filter_map(|service| {
                let names = allowed.get(&service.key)?;
                Some(ServiceEntry {
                    methods: service
                        .methods
                        .iter()
                        .filter(|method| {
                            method_name(method).is_some_and(|name| names.contains(&name))
                        })
                        .cloned()
                        .collect(),
                    ..service.clone()
                })
            })
            .collect(),
        events: model
            .events
            .iter()
            .filter(|event| CLIENT_EVENTS.contains(&event.name.as_str()))
            .cloned()
            .collect(),
    }
}

/// Regenerates the Client inspect catalog data, or with `check` fails when it is stale.
///
/// # Errors
/// Returns analysis failures, policy drift, a stale artifact under `check`, and write failures.
pub fn run_inspect_catalog(
    repo_root: &Path,
    source_root: &Path,
    check: bool,
) -> anyhow::Result<()> {
    verify_policy_matches_source(source_root)?;
    let projection = project(source_root, TypertFace::Client)?;
    let policy = cordis_catalog_policy();
    let projector =
        CordisCatalogProjector::new(&projection.face, &projection.source_declarations, &policy);
    let catalog = projector.runtime_catalog(&client_model(&projection.model))?;
    let artifact = Artifact::Json(renamed_json(&catalog)?);
    write_or_check(
        "gen-cordis-inspect-catalog",
        "cargo xtask cordis-inspect-catalog",
        repo_root,
        OUT_CLIENT_API,
        &artifact,
        check,
    )
}
