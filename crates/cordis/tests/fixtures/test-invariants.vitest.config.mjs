import { createRequire } from 'node:module';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const fixture = dirname(fileURLToPath(import.meta.url));
const target = resolve(fixture, '../../../..');
const source = process.env.SEEKDEEP_SOURCE_ORACLE_ROOT ?? '/Users/trevor/ws/deepseek-harness';
const requireSource = createRequire(resolve(source, 'package.json'));
const tsconfigPaths = (await import(pathToFileURL(requireSource.resolve('vite-tsconfig-paths')).href)).default;
const { standardDecoratorPlugin, vitestExecArgv } = await import(pathToFileURL(resolve(source, 'vitest.shared.ts')).href);
const setup = resolve(source, 'scripts/test-invariants.ts');
const compiled = process.env.SEEKDEEP_TEST_INVARIANT_RUNTIME !== 'source';

export default {
  root: source,
  cacheDir: '/tmp/seekdeep-test-invariant-vite-cache',
  plugins: [
    {
      name: 'compiled-rust-test-invariant-host',
      enforce: 'pre',
      load(id) {
        if (!compiled || id !== setup) return;
        return `
import { expect } from 'vitest';
import { RegistryService, ValidationError, installTestInvariantHost, createTestInvariantAttachmentStore,
  TEST_INVARIANT_READY_SERVICE, usesManualInvariantTree,
  testInvariantCompanionPaths as selectPaths } from '@deepseek-ai/cordis';
import InvariantRegistry from '@deepseek-ai/dsh-invariants';
import { AttachmentStore } from '@deepseek-ai/dsh-attachment';
export { TEST_INVARIANT_READY_SERVICE, usesManualInvariantTree };
export const testInvariantCompanions = import.meta.glob('../packages/*/*/src/invariant.ts');
export const testInvariantCompanionPaths = path => selectPaths(path, testInvariantCompanions);
installTestInvariantHost(RegistryService.prototype, InvariantRegistry,
  createTestInvariantAttachmentStore(AttachmentStore), ValidationError,
  () => expect.getState().testPath ?? '', testInvariantCompanions);
`;
      },
    },
    tsconfigPaths({ projects: [resolve(source, 'tsconfig.base.json')] }),
    standardDecoratorPlugin(),
  ],
  resolve: {
    alias: compiled ? [{ find: /^@deepseek-ai\/cordis$/, replacement: resolve(target, 'vendor/cordis/lib/index.js') }] : [],
  },
  test: {
    name: compiled ? 'compiled-rust-test-invariant-host' : 'source-test-invariant-host',
    pool: 'forks',
    execArgv: vitestExecArgv,
    maxWorkers: 1,
    setupFiles: [resolve(fixture, 'wasm-file-fetch.mjs'), setup],
    include: ['scripts/test-invariants.spec.ts'],
    testTimeout: 20_000,
  },
};
