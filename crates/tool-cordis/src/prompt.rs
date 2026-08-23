//! Model guidance for dynamic Cordis package development and recovery.

const RAW_SYSTEM_PROMPT: &str = include_str!("../data/system-prompt.txt");

/// Exact generated system-prompt text, with product identity renamed.
#[must_use]
pub fn cordis_system_prompt() -> &'static str {
    RAW_SYSTEM_PROMPT
        .strip_suffix('\n')
        .unwrap_or(RAW_SYSTEM_PROMPT)
}
