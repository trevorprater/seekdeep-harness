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
it('preserves Context branding and the dynamic brand-key contract', () => {
  const root = new Context(), Ctor = root.constructor, brand = Symbol.for('cordis.is')
  expect(Ctor.is(root)).toBe(true)
  expect(Ctor.is(root.extend({ marker: 1 }))).toBe(true)
  expect(Ctor.is(Ctor.prototype)).toBe(true)
  expect(brand in root).toBe(true)
  expect(root[Ctor.is]).toBe(true)
  for (const value of [null, undefined, false, 0, '', {}, () => {}]) expect(Ctor.is(value)).toBe(false)
  expect(Ctor.is({ [brand]: 'branded' })).toBe(true)
  const descriptor = Object.getOwnPropertyDescriptor(Ctor.prototype, brand)
  expect(descriptor).toEqual({ value: true, writable: true, enumerable: true, configurable: true })
  let receiver
  Object.defineProperty(Number.prototype, brand, { configurable: true, get() { receiver = this; return true } })
  try { expect(Ctor.is(7)).toBe(true); expect(receiver).toBe(7) } finally { delete Number.prototype[brand] }
  const original = Ctor.is[Symbol.toPrimitive], alternate = Symbol('alternate'), sentinel = new Error('brand-key failure')
  try {
    Ctor.is[Symbol.toPrimitive] = () => alternate
    expect(Ctor.is(root)).toBe(false)
    expect(Ctor.is({ [alternate]: true })).toBe(true)
    Ctor.is[Symbol.toPrimitive] = () => { throw sentinel }
    expect(Ctor.is(null)).toBe(false)
    expect(() => Ctor.is({})).toThrow(sentinel)
  } finally { Ctor.is[Symbol.toPrimitive] = original }
})

it('preserves browser event lifecycle, interception, and async entry timing', async () => {
  const root = new Context(), trace = []
  root.once('test/once', () => { trace.push('once'); root.emit('test/once') })
  root.emit('test/once')
  root.emit('test/once')
  expect(trace).toEqual(['once'])
  root.on('test/order', () => trace.push('last'))
  root.on('test/order', () => trace.push('first'), true)
  root.emit('test/order')
  expect(trace).toEqual(['once', 'first', 'last'])
  let shared
  root.on('internal/dispatch', (mode, name, args) => { if (name === 'test/waterfall') shared = args })
  root.on('test/waterfall', (value, next) => { trace.push('outer:' + value); shared[0] = 2; return next() + 1 })
  root.on('test/waterfall', (value, next) => { trace.push('inner:' + value); return next() * 2 })
  expect(root.waterfall('test/waterfall', 1, value => value + 10)).toBe(25)
  expect(trace.slice(-2)).toEqual(['outer:1', 'inner:2'])
  root.on('test/veto', () => false)
  expect(root.waterfall('test/veto', () => { throw new Error('veto ignored') })).toBe(false)
  const sentinel = () => 'intercepted'
  let intercepted
  root.on('internal/listener', function (name, listener, options) {
    if (name === 'test/intercepted') { intercepted = [this === root, typeof listener, options.prepend]; return sentinel }
  })
  expect(root.on('test/intercepted', () => { throw new Error('intercepted registration escaped') }, true)).toBe(sentinel)
  expect(intercepted).toEqual([true, 'function', true])
  root.emit('test/intercepted')
  let captured, disposals = 0, onceReceiver
  root.on('internal/listener', (_name, listener) => {
    if (_name === 'test/intercepted-once') { captured = listener; return () => { disposals++ } }
  })
  root.once('test/intercepted-once', function () { onceReceiver = this; expect(disposals).toBe(1) })
  captured.call(null)
  expect(onceReceiver).toBe(null)
  expect(disposals).toBe(1)
  const entered = []
  root.on('test/serial-entry', () => { entered.push('serial'); return false })
  const serial = root.serial('test/serial-entry')
  expect(entered).toEqual(['serial'])
  await serial
  root.on('test/parallel-entry', () => entered.push('parallel'))
  const parallel = root.parallel('test/parallel-entry')
  expect(entered).toEqual(['serial', 'parallel'])
  await parallel
  const rejection = new Error('dispatch refusal')
  root.on('internal/dispatch', (_mode, name) => { if (name === 'test/rejected') throw rejection })
  await expect(root.serial('test/rejected')).rejects.toBe(rejection)
  await expect(root.parallel('test/rejected')).rejects.toBe(rejection)
  let child, calls = 0
  const fiber = root.plugin({ name: 'event-lifecycle', apply(ctx) { child = ctx; ctx.once('test/disposed', () => calls++) } })
  await fiber
  await fiber.dispose()
  root.emit('test/disposed')
  expect(calls).toBe(0)
  expect(() => child.on('test/intercepted', () => {})).toThrow()
})

it('preserves browser event receivers, filtering, raw dispatch, and synchronous bail values', async () => {
  const root = new Context(), left = root.extend({ lane: 'left' }), right = root.extend({ lane: 'right' })
  const seen = [], payload = {}
  const receiver = { lane: 'left', [Context.filter](owner) { return this.lane === owner.lane } }
  const remove = left.on('test/explicit', function (value) { seen.push([this === receiver, value === payload]); return false })
  right.on('test/explicit', () => { throw new Error('wrong scope') })
  root.on('test/explicit', () => 0, { global: true })
  expect(root.bail(receiver, 'test/explicit', payload)).toBe(0)
  expect(seen).toEqual([[true, true]])
  const args = [receiver, 'test/explicit', payload]
  const callbacks = root.events.dispatch('emit', args)
  expect(args).toEqual([payload])
  expect(callbacks).toHaveLength(2)
  remove()
  expect(callbacks[0](...args)).toBe(false)
  expect(root.bail(receiver, 'test/explicit', payload)).toBe(0)
  expect(seen).toHaveLength(2)
  const promise = Promise.resolve(false)
  root.on('test/promise', () => promise)
  root.on('test/promise', () => { throw new Error('Promise is itself a bail value') })
  expect(root.bail('test/promise')).toBe(promise)
  const serial = []
  root.on('test/serial', async () => { serial.push(1); return false })
  root.on('test/serial', () => { serial.push(2); return '' })
  expect(await root.serial('test/serial')).toBe('')
  expect(serial).toEqual([1, 2])
  let implicit
  root.on('test/implicit', function () { implicit = this })
  root.emit('test/implicit')
  expect(implicit).toBe(null)
  const errors = [new Error('sync'), new Error('async')]
  root.on('test/errors', () => { throw errors[0] })
  root.on('test/errors', async () => { throw errors[1] })
  await expect(root.parallel('test/errors')).rejects.toMatchObject({ errors })
})

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
