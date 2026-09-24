import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { registerHooks } from 'node:module';
import { pathToFileURL } from 'node:url';

const source = process.argv[2];
registerHooks({
  resolve(specifier, context, nextResolve) {
    const entry = {
      '@deepseek-ai/cordis': 'vendor/cordis/src/index.ts',
      '@deepseek-ai/cordis-plugin-loader': 'vendor/loader/src/index.ts',
    }[specifier];
    return entry
      ? { url: pathToFileURL(path.join(source, entry)).href, shortCircuit: true }
      : nextResolve(specifier, context);
  },
});
const { Context } = await import(pathToFileURL(path.join(source, 'vendor/cordis/src/index.ts')));
const { Loader } = await import(pathToFileURL(path.join(source, 'vendor/loader/src/index.ts')));
const { TimerService } = await import(pathToFileURL(path.join(source, 'vendor/timer/src/index.ts')));
const { default: Hmr } = await import(pathToFileURL(path.join(source, 'vendor/hmr/src/index.ts')));
const directory = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), 'seekdeep-source-cjs-hmr-')));
const module = path.join(directory, 'plugin.cjs');
const dependency = path.join(directory, 'dep.cjs');
const baseUrl = pathToFileURL(directory + path.sep).href;
const ctx = new Context().extend({ baseUrl });
try {
  await fs.writeFile(module, "const dep = require('./dep.cjs'); module.exports = ctx => ctx.provide('hmrValue', dep.value);\n");
  await fs.writeFile(dependency, "module.exports = { value: 'old' };\n");
  await ctx.plugin(Loader, { baseUrl }).await();
  ctx.loader.internal.version = typeof ctx.loader.internal.resolve === 'function' ? 'v1' : 'v2';
  await ctx.plugin(TimerService).await();
  await ctx.plugin(Hmr, { root: [], ignored: [], debounce: 0 }).await();
  await ctx.loader.root.update([{ id: 'cjs', name: './plugin.cjs' }]);
  await ctx.loader.await();
  const initial = ctx.get('hmrValue');
  const events = [];
  ctx.on('hmr/change', url => events.push(['change', path.basename(new URL(url).pathname)]));
  const reloaded = Promise.withResolvers();
  ctx.on('hmr/reload', reloads => {
    events.push(['reload', [...reloads].map(([, reload]) => path.basename(new URL(reload.filename).pathname))]);
    reloaded.resolve();
  });
  await fs.writeFile(dependency, "module.exports = { value: 'new' };\n");
  ctx.hmr.watcher.emit('change', 'dep.cjs');
  const afterDependency = ctx.get('hmrValue');
  await fs.writeFile(module, "const dep = require('./dep.cjs'); module.exports = ctx => ctx.provide('hmrValue', dep.value + ':changed');\n");
  ctx.hmr.watcher.emit('change', 'plugin.cjs');
  const deadline = setTimeout(() => reloaded.reject(new Error('source HMR did not reload the CommonJS entry')), 5000);
  try { await reloaded.promise; } finally { clearTimeout(deadline); }
  await ctx.loader.await();
  process.stdout.write(JSON.stringify({ initial, afterDependency, afterPlugin: ctx.get('hmrValue'), events }) + '\n');
} finally {
  if (ctx.loader) await ctx.loader.root.stop();
  await ctx.fiber.dispose();
  await fs.rm(directory, { recursive: true, force: true });
}
