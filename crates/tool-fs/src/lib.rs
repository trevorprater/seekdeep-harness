//! Model-facing filesystem tools over the filesystem service.

pub mod error;
pub mod session_cwd;

pub use error::remediate_fs_error;
pub use session_cwd::{SessionResolveOptions, session_cwd, session_resolve_options};
