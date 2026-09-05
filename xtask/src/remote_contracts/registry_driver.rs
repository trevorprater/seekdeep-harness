//! Source test imports wired to actual built browser implementations.

pub(super) const ADAPTER: &str = r"
import { readFileSync } from 'node:fs';
import { runInThisContext } from 'node:vm';
import { Context as BrowserContext } from './cordis.mjs';
export { Service } from './cordis.mjs';
const root = __ROOT__;
const handoffs = new Map();
globalThis.window = globalThis;
globalThis.__ModuleLoader__ = { load(row) { handoffs.set(row.id, row); } };
runInThisContext(readFileSync(root + '/packages/typert/registry/lib/client.js', 'utf8'));
const plugin = handoffs.get('@seekdeep-ai/seekdeep-typert-registry').factory();
const exports = globalThis.__seekdeep_client_foundation_wasm__seekdeep_ai_seekdeep_typert_registry_wasm;
export const typertKey = exports.typertKey;
export const typertPackageKey = exports.typertPackageKey;
export const typertEndpoint = exports.typertEndpoint;
export const apply = plugin.apply;
export const inject = plugin.inject;
export default plugin;
export class Context {
  static effect = BrowserContext.effect;
  static filter = BrowserContext.filter;
  static isolate = BrowserContext.isolate;
  static intercept = BrowserContext.intercept;
  constructor() {
    const ctx = new BrowserContext();
    // Recording sink only: registry, providers, effects, and context are real WASM.
    ctx.provide('logger', { warn(...args) { console.warn(...args); } });
    return ctx;
  }
}
";

pub(super) const GATEWAY_ADDITIONAL: &str = r"
it('preserves strict and relaxed lookup through provider lifecycle and isolation', async () => {
  const root = new Context()
  root.provide('loading-service', { value: 'root' })
  const scoped = root.isolate('loading-service', 'loading-scope')
  let release, started
  const gate = new Promise(resolve => { release = resolve })
  const ready = new Promise(resolve => { started = resolve })
  const fiber = scoped.plugin({
    name: 'loading-provider',
    async apply(ctx) {
      ctx.provide('loading-service', { value: 'scoped' })
      started()
      await gate
    },
  })
  try {
    await ready
    expect(scoped.get('loading-service')).toBeUndefined()
    expect(scoped.get('loading-service', true)).toBeUndefined()
    expect(scoped.get('loading-service', false)?.value).toBe('scoped')
    expect(root.get('loading-service', false)?.value).toBe('root')
    expect(scoped.get('missing-service', false)).toBeUndefined()
    release()
    await fiber.await()
    expect(scoped.get('loading-service')?.value).toBe('scoped')
    expect(scoped.get('loading-service', false)?.value).toBe('scoped')
  } finally {
    release()
    await fiber.dispose()
  }
  expect(scoped.get('loading-service', false)).toBeUndefined()
  expect(root.get('loading-service')?.value).toBe('root')
})
it('keeps explicit service lookup separate from reflected properties', () => {
  const root = new Context()
  root.provide('property-service', { field: 42 })
  root.mixin('property-service', ['field'])
  expect(root.field).toBe(42)
  expect(root.get('field')).toBeUndefined()
  expect(root.get('field', false)).toBeUndefined()
})
it('preserves metadata descriptors receivers and isolated inheritance', () => {
  const root = new Context()
  root.provide('metadata-service', { value: 'root' })
  let reads = 0, receiver
  const getter = function () { reads++; receiver = this; return this.get('metadata-service').value }
  const symbol = Symbol('readonly'), value = Object.freeze({ source: true })
  const metadata = { label: 'child', absent: undefined }
  Object.defineProperty(metadata, 'current', { get: getter, enumerable: false, configurable: false })
  Object.defineProperty(metadata, symbol, { value, writable: false, enumerable: false, configurable: false })
  const child = root.extend(metadata)
  expect(reads).toBe(0)
  expect('current' in child).toBe(true)
  expect('absent' in child).toBe(true)
  expect(reads).toBe(0)
  expect(Object.getOwnPropertyDescriptor(child, 'current')).toEqual(Object.getOwnPropertyDescriptor(metadata, 'current'))
  expect(Object.getPrototypeOf(child)).toBe(root)
  expect(child.hasOwnProperty('label')).toBe(true)
  expect(root.isPrototypeOf(child)).toBe(true)
  expect(Object.keys(child)).toEqual(['label', 'absent'])
  expect(child.current).toBe('root')
  expect(receiver).toBe(child)
  const isolated = child.isolate('metadata-service', 'isolated')
  isolated.provide('metadata-service', { value: 'isolated' })
  expect(isolated.current).toBe('isolated')
  expect(receiver).toBe(isolated)
  expect(child.current).toBe('root')
  expect(child[symbol]).toBe(value)
  expect(Reflect.set(child, symbol, {})).toBe(false)
  Object.preventExtensions(child)
  expect(Object.getPrototypeOf(child)).toBe(root)
  expect(child.current).toBe('root')
})
it('clears retained event subscriptions when the gateway owner unloads', async () => {
  const { ctx, client } = await benchFiber(vi.fn())
  let observed = 0
  const remote = ctx.remote
  remote.$on('fixture/changed', () => { observed++ })
  await client.dispose()
  remote.$dispatch('fixture/changed', ['late'])
  expect(observed).toBe(0)
})
it('preserves immediate-remount rejection until withdrawal has settled', async () => {
  const ctx = await bench(vi.fn().mockResolvedValue({ ok: true, value: { ref: 'goal' } }))
  const contribution = { package: '@fixture/serialized', descriptors: [directDescriptor()] }
  const dispose = await ctx.remote.$mount(contribution)
  const withdrawing = dispose()
  const remounting = ctx.remote.$mount(contribution)
  await expect(remounting).rejects.toThrow('already mounted')
  await withdrawing
  const next = await ctx.remote.$mount(contribution)
  expect(ctx.typert.remotes.list()).toHaveLength(1)
  await next()
  expect(ctx.typert.remotes.list()).toEqual([])
})
it('owns pending mounts before admitting any mutation and recovers after rejection', async () => {
  const ctx = await bench(vi.fn())
  let retainedRemote: Context['remote']
  const owner = ctx.plugin({ inject: ['remote'], apply(context: Context) { retainedRemote = context.remote } })
  await owner
  await owner.dispose()
  await expect(retainedRemote!.$mount({ package: '@fixture/inactive', descriptors: [directDescriptor()] })).rejects.toThrow('inactive context')
  expect(ctx.typert.remotes.list()).toEqual([])
  const bad = ctx.remote.$mount({ package: '@fixture/bad', descriptors: [{ ...directDescriptor(), result: { mode: 'src-json' } }] })
  const good = ctx.remote.$mount({ package: '@fixture/recovery', descriptors: [directDescriptor()] })
  await expect(bad).rejects.toThrow('strict codec')
  const dispose = await good
  await dispose()
  expect(ctx.typert.remotes.list()).toEqual([])
})
";
