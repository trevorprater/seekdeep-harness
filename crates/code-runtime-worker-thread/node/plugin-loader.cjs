'use strict';

const { createConnection } = require('node:net');
const { pathToFileURL, fileURLToPath } = require('node:url');
const { createRequire } = require('node:module');
const { relative } = require('node:path');
const { watch } = require('chokidar');
const { getOrInitializeCascadedLoader } = require('internal/modules/esm/loader');
const runtime = require('./wasm-runtime.cjs');

runtime.start_loader({
  createConnection, pathToFileURL, fileURLToPath, createRequire, relative, watch,
  internal: getOrInitializeCascadedLoader(), require,
  process, global: globalThis, filename: __filename,
});
