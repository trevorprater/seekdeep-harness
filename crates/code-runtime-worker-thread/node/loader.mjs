import * as workers from 'node:worker_threads';
import { inspect } from 'node:util';
import { createConnection } from 'node:net';
import { fileURLToPath } from 'node:url';
import runtime from './wasm-runtime.cjs';

runtime.start({ ...workers, inspect, createConnection, process, global: globalThis, filename: fileURLToPath(import.meta.url) });
