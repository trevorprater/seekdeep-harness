//! Shared typed SDK wire vocabulary and newline-delimited JSON-RPC transport.

mod transport;
mod types;

pub use transport::{
    BoxedJsonRpcInput, BoxedJsonRpcOutput, JsonRpcLineTransport, JsonRpcNotificationHandler,
    JsonRpcRequestHandler, JsonRpcResponseError, JsonRpcResponseWrittenHandler,
    JsonRpcTransportFailureHandler,
};
pub use types::{
    HarnessSdkNotification, HarnessSdkRequest, InitializeParams, InitializeResult, SdkRunStatus,
    ServerInfo, SessionEventNotification, SessionPromptParams, SessionPromptResult, SessionStatus,
    SessionStatusNotification, SubagentFinishedNotification, SubagentStartedNotification,
};
