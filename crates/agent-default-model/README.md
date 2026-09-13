# seekdeep-agent-default-model

English | [中文](README.zh.md)

This crate provides the deployment default used when an entry point creates an Agent that has no session-local model selection. `AgentDefaultModel` provides the `agentDefaultModel` Cordis service; direct entry points such as `seekdeep --profile headless` and Host-backed entry points such as ApiProxy can read the same service instead of owning parallel provider/model defaults.

Plugin configuration requires `{ provider, model }`. That composition entry is the base of the `agent-default-model` Settings section. A mounted settings provider layers the user's choice over it, and changes are visible on the next `current_selection()` read. `reasoningEffort` belongs to the Settings section but deliberately not to plugin configuration: saving a complete selection can clear an effort when the newly selected model has none, while a composition value would otherwise be inherited again.

- `AgentDefaultModel::current_selection()` returns a detached provider, model, and optional reasoning-effort selection for a newly created Agent.
- `AgentDefaultModel::save_selection(selection)` saves the complete user selection. Without a settings provider it is a no-op and the composition entry remains current.

The service does not validate model-catalog membership. A provider route may serve an unadvertised model, and the consumer that opens a model request owns availability diagnostics.

## Model Experience

The service affects the model indirectly through the provider/model selection supplied to an entry point. Request assembly and adapters own the model-visible request.

#### KV Cache effect

Changing the default affects only Agents that subsequently resolve from it. An existing session whose request log already names a selection keeps that selection, so this service does not invalidate its established prefix.

## Known Limitations and Deferred Work

- The service owns one process-wide default; per-session selection remains the entry point's responsibility.
- Without a settings provider, `save_selection()` cannot retain a selection for a later Agent.
