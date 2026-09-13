import { readFileSync, writeFileSync } from 'node:fs';
import { it } from 'vitest';
import { Context } from '@deepseek-ai/cordis';
import { CallId } from '@deepseek-ai/dsh-llm';
import { Session, SessionId } from '@deepseek-ai/dsh-session';
import SystemPrompt from '@deepseek-ai/dsh-system-prompt';
import ToolRuntime, { defineTool } from '@deepseek-ai/dsh-tools';
import { WorkerThreadCodeRuntime } from '@deepseek-ai/dsh-code-runtime-worker-thread';
import type { Agent } from '@deepseek-ai/dsh-agent';

function serializeObservation(value: unknown): string {
  const output: string[] = [];
  const pending: ({ value: unknown } | { text: string })[] = [{ value }];
  while (pending.length > 0) {
    const item = pending.pop()!;
    if ('text' in item) {
      output.push(item.text);
      continue;
    }
    if (Array.isArray(item.value)) {
      output.push('[');
      pending.push({ text: ']' });
      for (let index = item.value.length - 1; index >= 0; index--) {
        pending.push({ value: item.value[index] });
        if (index > 0) pending.push({ text: ',' });
      }
    } else if (item.value !== null && typeof item.value === 'object') {
      const entries = Object.entries(item.value).filter(([, value]) => value !== undefined);
      output.push('{');
      pending.push({ text: '}' });
      for (let index = entries.length - 1; index >= 0; index--) {
        const [key, value] = entries[index]!;
        pending.push({ value });
        pending.push({ text: `${index > 0 ? ',' : ''}${JSON.stringify(key)}:` });
      }
    } else {
      const encoded = JSON.stringify(item.value);
      if (encoded === undefined) throw new Error('oracle observation contains a non-JSON scalar');
      output.push(encoded);
    }
  }
  return output.join('');
}

it('retains JavaScript string code units through mounted tools and worker dispatch', async () => {
  const requestPath = process.env.SEEKDEEP_TOOLS_JSON_REQUEST;
  const outputPath = process.env.SEEKDEEP_TOOLS_JSON_OUTPUT;
  if (!requestPath || !outputPath) throw new Error('source tool oracle requires input and output paths');
  const invocations = JSON.parse(readFileSync(requestPath, 'utf8')) as { name: string; arguments: unknown }[];
  const ctx = new Context();
  await ctx.plugin(SystemPrompt);
  await ctx.plugin(ToolRuntime, { mode: 'both' });
  const worker = await ctx.plugin(WorkerThreadCodeRuntime, {
    computeMs: 10_000,
    maxWallMs: 20_000,
    maxOutputBytes: 1_000_000,
    maxOldGenerationSizeMb: 512,
  });
  let calls: unknown[] = [];
  ctx.tools.register(defineTool({
    name: 'echo',
    description: 'Return the supplied JSON payload.',
    parameters: { payload: { type: 'json', required: true }, meta: { type: 'json' } },
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }],
      presentationMeta: (args, value) => args.meta === undefined ? { source: 'echo', value } : args.meta,
    },
    execute(args) {
      calls.push(args);
      return Promise.resolve(args.payload);
    },
  }));
  ctx.tools.register(defineTool({
    name: 'reject',
    description: 'Reject with a string containing an unpaired code unit.',
    parameters: {},
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }],
    },
    async execute() {
      throw new Error('\ud800');
    },
  }));
  ctx.tools.register(defineTool({
    name: 'strict',
    description: 'Accept an object without additional properties.',
    parameters: {
      payload: { type: 'object', properties: {}, additionalProperties: false, required: true },
    },
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }],
    },
    execute(args) { return Promise.resolve(args.payload); },
  }));
  ctx.tools.register(defineTool({
    name: 'strict_output',
    description: 'Return a value outside the output declaration.',
    parameters: {},
    output: {
      schema: { type: 'object', properties: {}, additionalProperties: false },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }],
    },
    execute() { return Promise.resolve({ '\ud800': 1 }); },
  }));
  const results: unknown[] = [];
  try {
    for (const [index, invocation] of invocations.entries()) {
      calls = [];
      const session = Session.create(SessionId(`lossless-${index}`));
      const agent = { session } as Agent;
      const result = await ctx.tools.execute({
        callId: CallId('lossless-call'),
        name: invocation.name,
        arguments: invocation.arguments,
        signal: new AbortController().signal,
        agent,
      });
      results.push({
        isError: result.isError,
        value: result.isError ? undefined : result.value,
        errorMessage: result.isError ? result.error.message : undefined,
        errorCode: result.isError ? result.error.info?.code : undefined,
        meta: result.meta,
        content: result.content,
        calls,
        events: session.events.map(({ type, data }) => ({ type, data })),
      });
    }
    writeFileSync(outputPath, serializeObservation(results));
  } finally {
    await worker.dispose();
  }
});
