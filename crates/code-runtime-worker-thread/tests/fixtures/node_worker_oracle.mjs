import { readFileSync } from 'node:fs';
import { Worker } from 'node:worker_threads';
import { stripTypeScriptTypes } from 'node:module';

const request = JSON.parse(readFileSync(0, 'utf8'));
const prefix = 'async function __dsh_program__() {\n';
const suffix = '\n}';
const code = stripTypeScriptTypes(prefix + request.program + suffix).slice(prefix.length, -suffix.length);
const worker = new Worker(request.sourceWorker, {
  workerData: { code, namespaces: [], maxOutputBytes: 1_000_000 },
  env: {},
  execArgv: [],
  resourceLimits: { maxOldGenerationSizeMb: 512 },
  stdout: true,
  stderr: true,
});
const logs = [];
const stray = [];
worker.stdout.on('data', chunk => stray.push(chunk.toString('utf8')));
worker.stderr.on('data', chunk => stray.push(chunk.toString('utf8')));
const message = await new Promise((resolve, reject) => {
  worker.on('message', message => {
    if (message.type === 'log') logs.push(message.text);
    if (message.type === 'done') resolve(message);
  });
  worker.on('error', reject);
  worker.on('exit', code => reject(new Error(`source worker exited with ${code}`)));
});
await new Promise(resolve => setImmediate(resolve));
await worker.terminate();
process.stdout.write(JSON.stringify({ ...message, logs: [...logs, ...stray] }) + '\n');
