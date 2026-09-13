'use strict';

const { readFileSync } = require('node:fs');
const { createRequire } = require('node:module');
const { join } = require('node:path');
const { runInNewContext } = require('node:vm');
const filename = join(__dirname, 'seekdeep_code_runtime_node.js');
const compiled = { exports: {} };
const enqueue = Promise.prototype.then.bind(Promise.resolve());

runInNewContext(readFileSync(filename, 'utf8'), {
  module: compiled,
  exports: compiled.exports,
  require: createRequire(filename),
  __filename: filename,
  __dirname,
  TextEncoder,
  TextDecoder,
  queueMicrotask: callback => { enqueue(callback); },
}, { filename });

module.exports = compiled.exports;
