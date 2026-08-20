//! The host-worker wire protocol: one string-valued enum of message tags per
//! direction and the discriminated message unions derived from them. Payloads
//! are plain JSON by construction for structured clone.

use seekdeep_workflow::{WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowResult};
use serde::{Deserialize, Serialize};

use crate::types::{ChildResult, ChildStartRequest};

/// Message tags the worker sends the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerToHostType {
    /// The startup handshake: the session is listening and awaits the go message.
    Ready,
    /// Observer narration: a phase call.
    Phase,
    /// Observer narration: a log call.
    Log,
    /// Observer lifecycle: one agent call started a child.
    AgentStart,
    /// Observer lifecycle: one agent call settled.
    AgentEnd,
    /// Child RPC: start a child on the host.
    ChildStart,
    /// Child RPC: dispose a started child.
    ChildDispose,
    /// The run's single terminal result.
    Result,
}

/// One worker-to-host message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum WorkerToHostMessage {
    /// Ready carries nothing.
    Ready,
    /// The phase title, verbatim.
    Phase {
        /// Phase title.
        title: String,
    },
    /// The logged message, verbatim.
    Log {
        /// Message text.
        message: String,
    },
    /// The call's sequence number, label, phase, and child id.
    AgentStart {
        /// Call identity.
        info: WorkflowAgentInfo,
    },
    /// The call identity plus its outcome.
    AgentEnd {
        /// Call settlement.
        info: WorkflowAgentEndInfo,
    },
    /// The RPC correlation id and the prompt plus validated options.
    ChildStart {
        /// RPC correlation id.
        call_id: u64,
        /// Prompt and validated options.
        request: ChildStartRequest,
    },
    /// The RPC correlation id of the child to dispose.
    ChildDispose {
        /// RPC correlation id.
        call_id: u64,
    },
    /// The run's terminal outcome.
    Result {
        /// Terminal outcome.
        result: WorkflowResult,
    },
}

/// Message tags the host sends the worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostToWorkerType {
    /// Releases the startup gate: run the script body.
    Go,
    /// Cancel the run.
    Cancel,
    /// Child RPC reply: the provider fulfilled with a published run.
    ChildStarted,
    /// Child RPC reply: the provider's asynchronous start failed.
    ChildStartError,
    /// Child RPC: a started child's result resolved.
    ChildSettled,
    /// Child RPC: a started child's result rejected (an infrastructure fault).
    ChildFailed,
    /// Child RPC reply: a requested disposal completed.
    ChildDisposed,
}

/// One host-to-worker message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum HostToWorkerMessage {
    /// Go carries nothing.
    Go,
    /// The cancel reason, canonical for the whole run.
    Cancel {
        /// Cancel reason.
        reason: String,
    },
    /// The RPC correlation id and the child agent's id.
    ChildStarted {
        /// RPC correlation id.
        call_id: u64,
        /// The child agent's id.
        child_id: String,
    },
    /// The RPC correlation id and the rendered start failure.
    ChildStartError {
        /// RPC correlation id.
        call_id: u64,
        /// Rendered start failure.
        rendered: String,
    },
    /// The RPC correlation id and the child's terminal result projection.
    ChildSettled {
        /// RPC correlation id.
        call_id: u64,
        /// Terminal result projection.
        result: ChildResult,
    },
    /// The RPC correlation id and the rendered infrastructure fault.
    ChildFailed {
        /// RPC correlation id.
        call_id: u64,
        /// Rendered infrastructure fault.
        rendered: String,
    },
    /// The RPC correlation id of the completed disposal.
    ChildDisposed {
        /// RPC correlation id.
        call_id: u64,
    },
}
