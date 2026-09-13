//! The one home of this application's forwarded-Host-event allowlist.

/// Host events this application forwards to consumers verbatim: no projection,
/// no redaction, no renaming.
pub const API_REMOTE_FORWARDED_EVENTS: [&str; 11] = [
    "agent-preset/selected",
    "commands/change",
    "credentials/updated",
    "cordis/request-run",
    "cordis/request-run-resolved",
    "cordis/dynamic-package",
    "cordis/dynamic-retract",
    "cordis/inspect-query",
    "cordis/inspect-query-resolved",
    "llm/adapters-updated",
    "settings/document-updated",
];
