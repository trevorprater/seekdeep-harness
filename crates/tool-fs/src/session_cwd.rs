//! Session working-directory derivation for filesystem tools.

use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::canonical_path;
use seekdeep_tools::ToolExecution;

/// Detects a parent-traversal path segment (`..`) bounded by path separators.
fn has_parent_segment(path: &str) -> bool {
    let bytes = path.as_bytes();
    let len = bytes.len();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'.' && index + 1 < len && bytes[index + 1] == b'.' {
            let before = index == 0 || matches!(bytes[index - 1], b'/' | b'\\');
            let after = index + 2;
            let following = after >= len || matches!(bytes[after], b'/' | b'\\');
            if before && following {
                return true;
            }
        }
    }
    false
}

/// The session workspace cwd for this call, or none when it does not apply.
#[must_use]
pub fn session_cwd(exec: &ToolExecution, requested_path: &str) -> Option<String> {
    let cwd = exec
        .agent
        .as_ref()
        .and_then(|agent| agent.session().header().cwd.clone());
    let cwd = cwd?;
    if !has_parent_segment(&cwd) && !has_parent_segment(requested_path) {
        return Some(cwd);
    }
    Some(canonical_path(&cwd).to_string_lossy().into_owned())
}

/// Resolution options shared by all model-facing filesystem tools.
#[derive(Clone, Debug)]
pub struct SessionResolveOptions {
    /// Working directory for relative resolution.
    pub cwd: Option<String>,
    /// Cancellation scoped to this call.
    pub signal: AbortSignal,
}

/// Provider resolution options for the current tool call.
#[must_use]
pub fn session_resolve_options(
    exec: &ToolExecution,
    requested_path: &str,
    policy_workspace_root: Option<&str>,
) -> SessionResolveOptions {
    let cwd = policy_workspace_root
        .map(str::to_owned)
        .or_else(|| session_cwd(exec, requested_path));
    SessionResolveOptions {
        cwd,
        signal: exec.signal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_segment_detection_matches_path_traversal_shapes() {
        assert!(has_parent_segment("../x"));
        assert!(has_parent_segment("a/../b"));
        assert!(has_parent_segment("a\\..\\b"));
        assert!(has_parent_segment("a/.."));
        assert!(!has_parent_segment("a/b"));
        assert!(!has_parent_segment("..."));
        assert!(!has_parent_segment("a..b"));
    }
}
