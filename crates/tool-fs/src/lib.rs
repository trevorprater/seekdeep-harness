//! Model-facing filesystem tools over the filesystem service.

pub mod diff;
pub mod edit;
pub mod error;
pub mod index;
pub mod invariant;
pub mod read;
pub mod read_image;
pub mod read_render;
pub mod read_target;
pub mod sandbox;
pub mod session_cwd;
pub mod write;

pub use edit::apply_edit_tool;
pub use error::remediate_fs_error;
pub use index::{Config, INJECT, NAME, apply, plugin};
pub use read::apply_read_tool;
pub use read_image::{apply_read_image_tool, assert_image_capable_route};
pub use read_target::{emit_fs_observed, resolve_regular_read_target};
pub use session_cwd::{SessionResolveOptions, session_cwd, session_resolve_options};
pub use write::apply_write_tool;
