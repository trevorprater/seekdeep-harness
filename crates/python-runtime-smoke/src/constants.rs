//! Source scenario inputs shared by the native model and scenario driver.

pub(crate) const EXPECTED_TEXT: &str = "runtime smoke ok";
pub(crate) const CODE_PROMPT: &str = "Use run_code to compute the packaged worker smoke value.";
pub(crate) const CODE_WORKER_TEXT: &str = "code worker smoke ok";
pub(crate) const WORKFLOW_PROMPT: &str =
    "Use workflow to compute the packaged worker smoke value without agents.";
pub(crate) const WORKFLOW_WORKER_TEXT: &str = "workflow worker smoke ok";
pub(crate) const MINIMAL_PROMPT: &str =
    "Exercise the packaged minimal agent's persistent Bash and string-replacement editor.";
pub(crate) const MINIMAL_TEXT: &str = "minimal agent smoke ok";
pub(crate) const MINIMAL_EDITOR_PATH_PREFIX: &str = "Editor path: ";
pub(crate) const MINIMAL_SYSTEM_PROMPT: &str = "You are a helpful software engineer assistant.";
pub(crate) const MINIMAL_BASH_COMMAND: &str = "counter=$(( ${counter:-0} + 1 )); export counter; printf 'COUNT=%s CWD=%s\\n' \"$counter\" \"$PWD\"; if [ \"$counter\" -eq 1 ]; then cd /tmp; fi";
pub(crate) const SNAPSHOT_PROMPT: &str = "Run the advanced packaged-runtime snapshot scenario.";
pub(crate) const SNAPSHOT_SESSION_ID: &str = "advanced-executable";
pub(crate) const SNAPSHOT_DIRECT_CHILD_PROMPT: &str =
    "Reply with exactly DIRECT_CHILD_OK and nothing else.";
pub(crate) const SNAPSHOT_WORKFLOW_CHILD_PROMPT: &str =
    "Reply with exactly WORKFLOW_CHILD_OK and nothing else.";
pub(crate) const SNAPSHOT_FINAL_TEXT: &str = "ADVANCED_EXECUTABLE_OK";
pub(crate) const SNAPSHOT_PLUGIN_CODE: &str = "\
return (ctx) => {
  harness.registerTool(ctx, harness.defineTool({
    name: 'snapshot_double',
    description: 'Double a number for executable snapshot verification.',
    parameters: { value: { type: 'number', required: true } },
    output: {
      schema: { type: 'number' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      }
    },
    async execute(args) {
      return args.value * 2
    }
  }))
}
";
pub(crate) const SNAPSHOT_WORKFLOW_SCRIPT: &str = "phase('Delegate')\nconst reply = await agent('Reply with exactly WORKFLOW_CHILD_OK and nothing else.', { label: 'workflow-child' })\nreturn { reply }";
pub(crate) const CUSTOM_CORDIS: &str = "\
- id: sdk-jsonrpc-server
  name: '@seekdeep-ai/seekdeep-sdk-jsonrpc-server'
- id: agent-core
  name: '@seekdeep-ai/seekdeep-agent-spine-demo'
  config:
    workspaceContext: false
    skills:
      enabled: false
    toolBash: false
    tools:
      mode: both
- id: sessions
  name: '@seekdeep-ai/seekdeep-session-persistence-jsonl'
  config:
    root: !!js process.env.SEEKDEEP_SESSION_ROOT
    compression: 'none'
- id: code-runtime
  name: '@seekdeep-ai/seekdeep-code-runtime-worker-thread'
- id: subagents
  name: '@seekdeep-ai/seekdeep-subagent'
- id: subagent-spawn-in-process
  name: '@seekdeep-ai/seekdeep-subagent-spawn-in-process'
  config:
    providerName: spawn
- id: subagent-tool
  name: '@seekdeep-ai/seekdeep-tool-subagent'
  config:
    provider: spawn
- id: workflow-engine
  name: '@seekdeep-ai/seekdeep-workflow-worker-thread'
  config:
    provider: spawn
- id: workflow-tool
  name: '@seekdeep-ai/seekdeep-tool-workflow'
- id: cordis-host-runner
  name: '@seekdeep-ai/seekdeep-cordis-host-runner'
- id: cordis-tool
  name: '@seekdeep-ai/seekdeep-tool-cordis'
";
