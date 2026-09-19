import { createRequire } from 'node:module';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const fixture = dirname(fileURLToPath(import.meta.url));
const source = process.env.SEEKDEEP_SOURCE_ORACLE_ROOT ?? '/Users/trevor/ws/deepseek-harness';
const requireSource = createRequire(resolve(source, 'package.json'));
const tsconfigPaths = (await import(pathToFileURL(requireSource.resolve('vite-tsconfig-paths')).href)).default;
const { standardDecoratorPlugin, vitestExecArgv } = await import(pathToFileURL(resolve(source, 'vitest.shared.ts')).href);

export default {
  root: source,
  cacheDir: '/tmp/seekdeep-tools-lossless-json-vite-cache',
  plugins: [
    tsconfigPaths({ projects: [resolve(source, 'tsconfig.base.json')] }),
    standardDecoratorPlugin(),
  ],
  resolve: {
    alias: [
      ['@deepseek-ai/cordis', 'vendor/cordis/src/index.ts'],
      ['@deepseek-ai/dsh-llm', 'packages/llm/llm/src/index.ts'],
      ['@deepseek-ai/dsh-session', 'packages/core/session/src/index.ts'],
      ['@deepseek-ai/dsh-system-prompt', 'packages/core/system-prompt/src/index.ts'],
      ['@deepseek-ai/dsh-tools', 'packages/core/tools/src/index.ts'],
      ['@deepseek-ai/dsh-code-runtime-worker-thread', 'packages/code-runtime/code-runtime-worker-thread/src/index.ts'],
    ].map(([name, path]) => ({ find: new RegExp(`^${name}$`), replacement: resolve(source, path) })),
  },
  test: {
    name: 'source-tools-lossless-json',
    pool: 'forks',
    execArgv: vitestExecArgv,
    maxWorkers: 1,
    include: [resolve(fixture, 'lossless-code-json.spec.ts')],
    testTimeout: 60_000,
  },
};
