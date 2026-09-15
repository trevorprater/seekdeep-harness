//! Persistent local shell PTY backend for `SeekDeep Harness`.

/// Shared-policy Bash backend and loader-compatible plugin.
pub mod backend;
/// Validated local backend configuration.
pub mod config;
/// Explained-empty package invariant companion.
pub mod invariant;
/// Streaming terminal-control sanitizer.
pub mod sanitize;
/// Persistent shell session over the subprocess terminal primitive.
pub mod session;

pub use backend::{
    BashPtySession, BashPtySessionRef, BashTerminalBackend, INJECT, NAME, apply, child_environment,
    plugin,
};
pub use config::{ResolvedTerminalBashConfig, TerminalBashConfig, TerminalBashConfigError};
pub use sanitize::{
    CONTROLLED_PROMPT, PROMPT_MARKER_PREFIX, SanitizedChunk, TerminalSanitizer,
    normalize_terminal_text,
};
pub use session::LocalPtySession;
