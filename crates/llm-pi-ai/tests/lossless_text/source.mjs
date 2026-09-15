import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [root, inputPath] = process.argv.slice(2);
const input = JSON.parse(fs.readFileSync(inputPath, 'utf8'));
const { toPiContext } = await import(pathToFileURL(path.join(root, 'packages/llm/llm-pi-ai/src/context.ts')));
const attachments = input.images ? {async readImage(ref) { return {ref, data: new Uint8Array([1])}; }} : undefined;
const context = await toPiContext({ messages: input.messages }, attachments);
for (const message of context.messages) {
  if (message.role === 'assistant' && message.api === 'dsh-foreign') message.api = 'seekdeep-foreign';
}
if (input.op === 'context') {
  process.stdout.write(JSON.stringify(context));
} else {
  const packageRoot = fs.realpathSync(path.join(root, 'packages/llm/llm-pi-ai/node_modules/@earendil-works/pi-ai'));
  assert.equal(JSON.parse(fs.readFileSync(path.join(packageRoot, 'package.json'), 'utf8')).version, '0.82.1');
  let networkCalls = 0;
  globalThis.fetch = async () => { networkCalls++; throw Error('source fixture must stop before network access'); };
  const { stream } = await import(pathToFileURL(path.join(packageRoot, 'dist/api', input.module + '.js')));
  let body;
  const events = stream(input.model, context, {
    apiKey: 'fixture-key',
    cacheRetention: 'none',
    maxRetries: 0,
    onPayload(payload) {
      body = payload;
      if (input.module === 'google-generative-ai' && payload.contents?.length === 0) return;
      throw Error('source payload captured');
    },
  });
  const outcome = await events.result();
  assert.ok(body, 'source converter must reach its payload hook');
  if (process.env.SEEKDEEP_PI_AI_TEXT_TRACE) {
    fs.appendFileSync(process.env.SEEKDEEP_PI_AI_TEXT_TRACE, JSON.stringify({module: input.module, messages: input.messages, body}) + '\n');
  }
  process.stdout.write(JSON.stringify({ context, body, errorMessage: outcome.errorMessage, networkCalls }));
}
