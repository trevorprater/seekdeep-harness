//! The host-worker wire protocol: one string-valued enum of message tags per
//! direction and the discriminated message unions derived from them. Payloads
//! are plain JSON by construction for structured clone.

use seekdeep_core::session::JsonValue;
use seekdeep_workflow::{WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowResult};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

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
#[derive(Clone, Debug, PartialEq, Serialize)]
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

#[derive(Deserialize)]
struct HostMessageTag {
    #[serde(rename = "type")]
    kind: HostToWorkerType,
}

#[derive(Deserialize)]
struct CancelFields {
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartedFields {
    call_id: u64,
    child_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderedFields {
    call_id: u64,
    rendered: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettledFields {
    call_id: u64,
    result: ChildResult,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisposedFields {
    call_id: u64,
}

impl<'de> Deserialize<'de> for HostToWorkerMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <JsonValue as Deserialize>::deserialize(deserializer)?;
        if !raw.is_object() {
            return Err(D::Error::custom("host message must be an object"));
        }
        let tag: HostMessageTag = raw.deserialize().map_err(D::Error::custom)?;
        match tag.kind {
            HostToWorkerType::Go => Ok(Self::Go),
            HostToWorkerType::Cancel => {
                let fields: CancelFields = raw.deserialize().map_err(D::Error::custom)?;
                Ok(Self::Cancel {
                    reason: fields.reason,
                })
            }
            HostToWorkerType::ChildStarted => {
                let fields: StartedFields = raw.deserialize().map_err(D::Error::custom)?;
                Ok(Self::ChildStarted {
                    call_id: fields.call_id,
                    child_id: fields.child_id,
                })
            }
            HostToWorkerType::ChildStartError => {
                let fields: RenderedFields = raw.deserialize().map_err(D::Error::custom)?;
                Ok(Self::ChildStartError {
                    call_id: fields.call_id,
                    rendered: fields.rendered,
                })
            }
            HostToWorkerType::ChildSettled => {
                let fields: SettledFields = raw.deserialize().map_err(D::Error::custom)?;
                Ok(Self::ChildSettled {
                    call_id: fields.call_id,
                    result: fields.result,
                })
            }
            HostToWorkerType::ChildFailed => {
                let fields: RenderedFields = raw.deserialize().map_err(D::Error::custom)?;
                Ok(Self::ChildFailed {
                    call_id: fields.call_id,
                    rendered: fields.rendered,
                })
            }
            HostToWorkerType::ChildDisposed => {
                let fields: DisposedFields = raw.deserialize().map_err(D::Error::custom)?;
                Ok(Self::ChildDisposed {
                    call_id: fields.call_id,
                })
            }
        }
    }
}
