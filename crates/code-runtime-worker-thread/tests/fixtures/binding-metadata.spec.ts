import { readFileSync, writeFileSync } from 'node:fs';
import { it } from 'vitest';
import { Context } from '@deepseek-ai/cordis';
import { WorkerThreadCodeRuntime } from '@deepseek-ai/dsh-code-runtime-worker-thread';

it('preserves generic binding member and error-property metadata', async () => {
  const request = process.env.SEEKDEEP_BINDING_METADATA_INPUT;
  const output = process.env.SEEKDEEP_BINDING_METADATA_OUTPUT;
  if (!request || !output) throw new Error('metadata oracle paths are required');
  const cases = JSON.parse(readFileSync(request, 'utf8'));
  const ctx = new Context();
  const fiber = await ctx.plugin(WorkerThreadCodeRuntime, {
    computeMs: 10_000, maxWallMs: 20_000, maxOutputBytes: 1_000_000, maxOldGenerationSizeMb: 512,
  });
  const observations = [];
  try {
    for (const scenario of cases) {
      const calls: unknown[] = [];
      const functions = Object.create(null);
      for (const name of scenario.names) {
        Object.defineProperty(functions, name, {
          enumerable: true, configurable: true, writable: true,
          value: async (args: unknown) => {
            calls.push({ name, args });
            if (Object.hasOwn(scenario, 'resolution')) return scenario.resolution;
            throw new Error('failure \ud800');
          },
        });
      }
      const namespace = {
        global: scenario.global,
        functions,
        ...(scenario.className === null ? {} : {
          errorClass: { name: scenario.className, memberNameProperty: scenario.property },
        }),
      };
      try {
        const result = await ctx.codeRuntime.run({ program: scenario.program, bindings: [namespace] });
        observations.push({ name: scenario.name, result, calls });
      } catch (error) {
        observations.push({ name: scenario.name, rejected: error.message, calls });
      }
    }
    writeFileSync(output, JSON.stringify(observations));
  } finally {
    await fiber.dispose();
  }
});
