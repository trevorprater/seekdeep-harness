//! Observe the pinned source's three admissible child/worker start schedules.

use std::{path::Path, process::Command};

const ORACLE: &str = r#"
import { Context } from '__SOURCE__/vendor/cordis/src/index.ts';
import { SessionId } from '__SOURCE__/packages/core/session/src/index.ts';
import AgentLoop from '__SOURCE__/packages/core/agent-loop/src/index.ts';
import { mountAgentLoopTestDependencies } from '__SOURCE__/packages/test-support/agent-loop-testkit/src/index.ts';
import SubagentRuntime from '__SOURCE__/packages/subagent/subagent/src/index.ts';
import * as spawn from '__SOURCE__/packages/subagent/subagent-spawn-in-process/src/index.ts';
import { MockAdapter, textResponse } from '__SOURCE__/packages/core/agent-loop/tests/mock-adapter.ts';
import WorkerThreadWorkflowEngine from '__SOURCE__/packages/workflow/workflow-worker-thread/src/index.ts';

const traces = {};
for (const mode of ['immediate', 'metadata', 'stream']) {
  const ctx = new Context();
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  class Adapter extends MockAdapter {
    async resolveModel(provider, model) {
      if (mode === 'metadata') await gate;
      return super.resolveModel(provider, model);
    }
    async * stream(options) {
      if (mode === 'stream') await gate;
      yield * super.stream(options);
    }
  }
  await mountAgentLoopTestDependencies(ctx);
  await ctx.plugin(AgentLoop, { agents: [] });
  await ctx.plugin(SubagentRuntime);
  await ctx.plugin(spawn, { providerName: 'spawn' });
  await ctx.plugin(WorkerThreadWorkflowEngine, {});
  ctx.llm.registerAdapter(['mock'], new Adapter([textResponse('child completed')]));
  const parent = ctx.agentLoop.create(SessionId('parent'), { provider:'mock', model:'mock' });
  const order = [];
  ctx.on('session/event', (session,event) => {
    if (session.header.parentSession !== undefined && ['request/context','assistant/message','turn/end'].includes(event.type)) order.push(event.type);
  });
  ctx.on('workflow/agent-start', () => { order.push('workflow/agent-start'); release(); });
  ctx.on('workflow/agent-end', () => order.push('workflow/agent-end'));
  const run = ctx.workflowEngine.start({meta:{name:'order',description:'observe child and worker ordering'},script:"return await agent('answer')",parent});
  let timeout;
  try {
    const result = await Promise.race([run.result, new Promise((_, reject) => {
      timeout = setTimeout(() => reject(new Error('source workflow ordering probe timed out')), 15000);
    })]);
    if (result.value !== 'child completed' || result.stopReason !== 'completed') throw new Error(JSON.stringify(result));
    traces[mode] = order;
  } finally {
    clearTimeout(timeout);
    release();
    await run.dispose();
    await ctx.fiber.dispose();
  }
}
console.log(JSON.stringify(traces));
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = std::env::args_os()
        .nth(1)
        .ok_or("usage: workflow_order_source_parity <pinned-source>")?;
    let source = Path::new(&source).canonicalize()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("source pin absent")?;
    let head = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != pin {
        return Err("oracle differs from SOURCE_SNAPSHOT".into());
    }
    let temporary = tempfile::tempdir()?;
    let script = temporary.path().join("probe.mjs");
    std::fs::write(
        &script,
        ORACLE.replace("__SOURCE__", &source.to_string_lossy()),
    )?;
    let output = Command::new("node")
        .arg("--import")
        .arg(source.join("node_modules/tsx/dist/loader.mjs"))
        .arg(&script)
        .env("TSX_TSCONFIG_PATH", source.join("tsconfig.base.json"))
        .current_dir(&source)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let traces: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    for (mode, expected) in [
        (
            "immediate",
            [
                "request/context",
                "assistant/message",
                "turn/end",
                "workflow/agent-start",
                "workflow/agent-end",
            ],
        ),
        (
            "metadata",
            [
                "workflow/agent-start",
                "request/context",
                "assistant/message",
                "turn/end",
                "workflow/agent-end",
            ],
        ),
        (
            "stream",
            [
                "request/context",
                "workflow/agent-start",
                "assistant/message",
                "turn/end",
                "workflow/agent-end",
            ],
        ),
    ] {
        if traces[mode] != serde_json::json!(expected) {
            return Err(format!("source {mode} schedule differs: {}", traces[mode]).into());
        }
    }
    println!("pinned source admits all three workflow start schedules: {traces}");
    Ok(())
}
