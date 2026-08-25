//! Rust client SDK for a complete `SeekDeep` Harness runtime subprocess.

mod api;
mod client;
mod types;

pub use api::{
    DeepSeekHarness, HarnessSession, NotificationObserver, RunInput, RunOptions, final_response,
    normalize_input,
};
pub use client::{
    HarnessClient, NotificationSubscription, RequestTimeoutError, SdkProtocolError,
    TransportClosedError,
};
pub use seekdeep_llm::ContentBlock;
pub use seekdeep_sdk_protocol::JsonRpcResponseError;
pub use types::{
    DeepSeekHarnessOptions, HarnessClientOptions, HarnessNotification, NotificationFilter,
    RunResult,
};
