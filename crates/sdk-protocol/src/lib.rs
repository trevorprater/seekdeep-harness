//! Shared typed SDK wire vocabulary and newline-delimited JSON-RPC transport.

mod transport;
mod types;

pub use seekdeep_lossless_json::{JsonString, JsonValue};
pub use transport::{
    BoxedJsonRpcInput, BoxedJsonRpcOutput, JsonRpcJsonNotificationHandler,
    JsonRpcJsonRequestHandler, JsonRpcLineTransport, JsonRpcNotificationHandler,
    JsonRpcRawResponseError, JsonRpcRequestHandler, JsonRpcResponseError,
    JsonRpcResponseWrittenHandler, JsonRpcTransportFailureHandler,
};
pub use types::{
    HarnessSdkNotification, HarnessSdkRequest, InitializeParams, InitializeResult, SdkRunStatus,
    ServerInfo, SessionEventNotification, SessionPromptParams, SessionPromptResult, SessionStatus,
    SessionStatusNotification, SubagentFinishedNotification, SubagentStartedNotification,
};
