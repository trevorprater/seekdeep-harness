//! Repository-wide durable session-event vocabulary.

use std::{collections::HashSet, sync::LazyLock};

/// Every session event type understood by this source snapshot.
pub static KNOWN_SESSION_EVENT_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "agent-preset/selected",
        "agent/inbox/spliced",
        "approval/asked",
        "approval/decided",
        "approval/policy",
        "assistant/chunk",
        "assistant/message",
        "command/done",
        "command/run",
        "compaction/end",
        "compaction/prune",
        "compaction/start",
        "compaction/summary",
        "feedback/record",
        "goal/change",
        "hook/invoked",
        "hook/result",
        "llm/retry",
        "llm/retry-started",
        "permission/preset",
        "plan/mode",
        "request/context",
        "request/header",
        "sandbox/mode",
        "schedule/change",
        "session/end-seed",
        "session/title",
        "session/title-llm-request",
        "step/end",
        "step/start",
        "subagent/descriptor",
        "todo/write",
        "tool-workflow/agent-end",
        "tool-workflow/agent-start",
        "tool-workflow/run-end",
        "tool-workflow/run-start",
        "tool/call",
        "tool/code-dispatch",
        "tool/code-dispatch-start",
        "tool/result",
        "turn/end",
        "turn/start",
        "user/message",
        "web/deepseek-search-llm-request",
    ]
    .into_iter()
    .collect()
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_core_and_plugin_events() {
        assert!(KNOWN_SESSION_EVENT_TYPES.contains("turn/start"));
        assert!(KNOWN_SESSION_EVENT_TYPES.contains("compaction/summary"));
        assert_eq!(KNOWN_SESSION_EVENT_TYPES.len(), 44);
    }
}
