//! Content-Length framed stdio LSP provider infrastructure.

pub mod abort;
pub mod framing;
pub mod protocol;
pub mod translate;

pub use abort::{abort_error, abortable, throw_if_aborted};
pub use framing::{MAX_HEADER_BYTES, MessageDecoder, encode_message};
pub use protocol::*;
pub use translate::{
    negotiate_position_encoding, normalize_hover, normalize_locations, request_method,
    supports_operation, supports_transient_open,
};
