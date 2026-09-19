import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { Worker } from 'node:worker_threads';

const request = JSON.parse(readFileSync(0, 'utf8'));
const { decodeWorkerJson, encodeWorkerJson } = await import(
  pathToFileURL(join(dirname(request.sourceWorker), 'worker-json.ts')).href
);
const worker = new Worker(request.sourceWorker, {
  workerData: {
    code: request.program,
    namespaces: [{
      global: 'host',
      names: ['echo', 'native', 'reject'],
      errorClass: { name: 'BindingError', memberNameProperty: 'member' },
    }],
    maxOutputBytes: 1_000_000,
  },
  env: {},
  execArgv: [],
  resourceLimits: { maxOldGenerationSizeMb: 512 },
  stdout: true,
  stderr: true,
});
const calls = [];
const logs = [];
const stray = [];
worker.stdout.on('data', chunk => stray.push(chunk.toString('utf8')));
worker.stderr.on('data', chunk => stray.push(chunk.toString('utf8')));
const completion = await new Promise((resolve, reject) => {
  const timer = setTimeout(() => reject(new Error('source lossless JSON fixture timed out')), 60_000);
  worker.on('message', message => {
    if (message.type === 'log') logs.push(message.text);
    if (message.type === 'call') {
      calls.push(message.args);
      const argument = decodeWorkerJson(message.args);
      if (argument === undefined) {
        reject(new Error('source rejected its own binding arguments'));
        return;
      }
      if (message.name === 'reject') {
        worker.postMessage({ type: 'reply', id: message.id, ok: false, message: JSON.parse(request.nativeError) });
      } else {
        const value = message.name === 'native' ? JSON.parse(request.nativeValue) : argument;
        worker.postMessage({ type: 'reply', id: message.id, ok: true, value: encodeWorkerJson(value) });
      }
    }
    if (message.type === 'done') {
      clearTimeout(timer);
      resolve(message);
    }
  });
  worker.on('error', error => { clearTimeout(timer); reject(error); });
  worker.on('exit', code => { clearTimeout(timer); reject(new Error(`source worker exited with ${code}`)); });
});
await new Promise(resolve => setImmediate(resolve));
await worker.terminate();
process.stdout.write(JSON.stringify({ ...completion, calls, logs: [...logs, ...stray] }) + '\n');
