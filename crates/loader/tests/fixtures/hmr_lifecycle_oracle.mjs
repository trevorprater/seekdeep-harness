import fs from 'node:fs';
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

function plugin(generation, failure, asyncDispose) {
  return `
import { appendFileSync } from 'node:fs';
const record = kind => appendFileSync(new URL('trace.jsonl', import.meta.url), JSON.stringify({kind}) + '\\n');
export const name = ${JSON.stringify(generation)};
export const generation = ${JSON.stringify(generation)};
export function apply(ctx, config) {
  record('apply:' + generation + ':' + config.id);
  ctx.provide('probe_' + config.id, generation);
  ctx.effect(() => ${asyncDispose ? 'async ' : ''}() => {
    record('dispose-start:' + generation + ':' + config.id);
    ${asyncDispose ? 'await Promise.resolve();' : ''}
    record('dispose-end:' + generation + ':' + config.id);
  });
  if (${JSON.stringify(failure)} === 'body' && config.id === 'second') throw new Error('second replacement apply rejected');
}
apply.generation = generation;
`;
}

async function scenario(failure, asyncDispose) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'seekdeep-source-hmr-lifecycle-')));
  const filename = path.join(root, 'plugin.mjs');
  const tracefile = path.join(root, 'trace.jsonl');
  const record = value => fs.appendFileSync(tracefile, JSON.stringify(value) + '\n');
  const trace = () => fs.readFileSync(tracefile, 'utf8').split('\n').filter(Boolean).map(JSON.parse);
  const baseUrl = pathToFileURL(root + path.sep).href;
  const ctx = new Context().extend({ baseUrl });
  try {
    await ctx.plugin(Loader, { baseUrl }).await();
    ctx.loader.internal.version = typeof ctx.loader.internal.resolve === 'function' ? 'v1' : 'v2';
    await ctx.plugin(TimerService).await();
    await ctx.plugin(Hmr, { root: [], ignored: [], debounce: 0 }).await();
    let rejectRegistration = false;
    let firstUid;
    ctx.on('internal/plugin', fiber => {
      const id = fiber._config?.id;
      if (!['first', 'second'].includes(id)) return;
      firstUid ??= fiber.uid;
      record({ kind:'publication', id, generation:fiber.runtime.callback.generation, uid:fiber.uid === null ? null : fiber.uid - firstUid, state:fiber.state,
        strict:ctx.get('probe_' + id, true) ?? null, loose:ctx.get('probe_' + id, false) ?? null });
      if (fiber.uid && id === 'second' && rejectRegistration) {
        rejectRegistration = false;
        record({kind:'registration-failure:second'});
        throw new Error('second replacement registration rejected');
      }
    }, { global: true });
    ctx.on('hmr/reload', reloads => {
      record({ kind:'event:hmr/reload',
        oldFibers:[...reloads].flatMap(([, reload]) => [...reload.runtime.fibers].map(fiber => ({id:fiber.entry.id, uid:fiber.uid === null ? null : fiber.uid - firstUid, state:fiber.state}))),
        entries:[...ctx.loader.entries()].map(entry => ({id:entry.id, uid:entry.fiber.uid === null ? null : entry.fiber.uid - firstUid, state:entry.fiber.state})),
        providers:['first','second'].map(id => ({id, strict:ctx.get('probe_' + id, true) ?? null, loose:ctx.get('probe_' + id, false) ?? null})),
      });
    });
    fs.writeFileSync(filename, plugin('old', 'none', asyncDispose));
    await ctx.loader.root.update([
      {id:'first',name:'./plugin.mjs',config:{id:'first'}},
      {id:'second',name:'./plugin.mjs',config:{id:'second'}},
    ]);
    await ctx.loader.await();
    const initial = trace();
    fs.writeFileSync(tracefile, '');
    fs.writeFileSync(filename, plugin('new', failure, asyncDispose));
    rejectRegistration = failure === 'registration';
    ctx.hmr.stashed.add(pathToFileURL(filename).href);
    await ctx.hmr.partialReload();
    let settlementError = null;
    try { await ctx.loader.await(); } catch (error) { settlementError = error.message; }
    const settled = trace();
    const entries = [...ctx.loader.entries()].map(entry => ({
      id:entry.id, uid:entry.fiber.uid === null ? null : entry.fiber.uid - firstUid, state:entry.fiber.state, disabled:entry.options.disabled ?? false,
      generation:entry.fiber.runtime.callback.generation,
    }));
    return {failure, asyncDispose, initial, settled, entries, settlementError};
  } finally {
    if (ctx.loader) await ctx.loader.root.stop();
    await ctx.fiber.dispose();
    fs.rmSync(root, {recursive:true,force:true});
  }
}

const reports = [];
for (const asyncDispose of [false, true]) {
  for (const failure of ['none', 'body', 'registration']) reports.push(await scenario(failure, asyncDispose));
}
process.stdout.write(JSON.stringify(reports) + '\n');
