//! Content-Length framed stdio LSP provider infrastructure.

pub mod abort;
pub mod connection;
pub mod framing;
pub mod host;
pub mod instance;
pub mod protocol;
pub mod provider;
pub mod translate;

pub use abort::{abort_error, abortable, throw_if_aborted};
pub use connection::{
    ConnectionError, ConnectionRequest, ConnectionServerRequestFuture,
    ConnectionServerRequestHandler, ConnectionSpec, ConnectionWriteFuture, ConnectionWriter,
    LspConnection,
};
pub use framing::{MAX_HEADER_BYTES, MessageDecoder, encode_message};
pub use host::{HostSource, HostWorkspace, canonicalize_workspace, read_host_source};
pub use instance::{InstanceSpec, LspInstance};
pub use protocol::*;
pub use provider::{Config, INJECT, LspLocalServerConfig, NAME, apply, config_schema, plugin};
pub use translate::{
    negotiate_position_encoding, normalize_hover, normalize_locations, request_method,
    supports_operation, supports_transient_open,
};
