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
it('preserves concurrent Fiber teardown start order and joined completion', async () => {
  const root = new Context(), started = [], completed = []
  let release, releaseDependency
  const gate = new Promise(resolve => { release = resolve })
  const dependency = new Promise(resolve => { releaseDependency = resolve })
  const fiber = root.plugin({ apply(ctx) {
    for (const name of ['first', 'second']) ctx.effect(() => async () => {
      started.push(name)
      if (name === 'first') { releaseDependency(); await gate }
      else await dependency
      completed.push(name)
    })
  } })
  await fiber
  let settled = false
  const first = fiber.dispose().then(() => { settled = true })
  const second = fiber.dispose()
  try {
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(started).toEqual(['second', 'first'])
    expect(completed).toEqual(['second'])
    expect(settled).toBe(false)
  } finally {
    release()
    releaseDependency()
    await Promise.all([first, second])
  }
  expect(completed).toEqual(['second', 'first'])
  expect(settled).toBe(true)
  expect(fiber.state).toBe(4)
})

it('preserves concurrent Fiber teardown before restarting the next config', async () => {
  const root = new Context(), applied = [], started = []
  let release
  const gate = new Promise(resolve => { release = resolve })
  const fiber = await root.plugin({ apply(ctx, config) {
    applied.push(config.version)
    for (const name of ['first', 'second']) ctx.effect(() => async () => {
      started.push([config.version, name]); await gate
    })
  } }, { version: 1 })
  const restart = fiber.update({ version: 2 })
  try {
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(started).toEqual([[1, 'second'], [1, 'first']])
    expect(applied).toEqual([1])
  } finally { release(); await restart }
  expect(applied).toEqual([1, 2])
  await fiber.dispose()
})

it('preserves symbol event registration, dispatch failure, identity, and disposal', async () => {
  const root = new Context(), event = Symbol('event'), other = Symbol('event'), values = [], intercepted = []
  root.on('internal/listener', (name) => { if (typeof name === 'symbol') intercepted.push(name) })
  const fiber = root.plugin({ apply(ctx) {
    ctx.on(event, value => values.push(['event', value]))
    ctx.on(other, value => values.push(['other', value]))
  } })
  await fiber
  expect(intercepted).toEqual([event, other])
  expect(() => root.emit(event, 1)).toThrow('name.startsWith is not a function')
  expect(values).toEqual([])
  const descriptor = Object.getOwnPropertyDescriptor(Symbol.prototype, 'startsWith')
  try {
    Object.defineProperty(Symbol.prototype, 'startsWith', { configurable: true, value() { return false } })
    root.emit(event, 1)
    root.emit(other, 2)
    expect(values).toEqual([['event', 1], ['other', 2]])
    const once = root.once(event, value => values.push(['once', value]))
    root.emit(event, 3); root.emit(event, 4)
    expect(values.slice(2)).toEqual([['event', 3], ['once', 3], ['event', 4]])
    await once()
    await fiber.dispose()
    root.emit(event, 5); root.emit(other, 6)
    expect(values.length).toBe(5)
  } finally {
    if (descriptor) Object.defineProperty(Symbol.prototype, 'startsWith', descriptor)
    else delete Symbol.prototype.startsWith
  }
})

it('preserves per-Fiber update routing, veto, config identity, and restart ownership', async () => {
  const root = new Context(), log = [], fibers = new Map()
  const mount = label => root.plugin({ name: label, apply(ctx, config) {
    log.push([label, 'apply', config])
    ctx.on('internal/update', function (nextConfig, noSave, next) {
      if (this !== fibers.get(label)) throw new Error('update receiver differs from owning fiber')
      log.push([label, 'update', nextConfig, noSave])
      if (nextConfig.veto) return 'vetoed'
      return next()
    })
    ctx.effect(() => () => log.push([label, 'dispose']))
  } }, { value: 1 })
  const first = mount('first'), second = mount('second')
  fibers.set('first', first); fibers.set('second', second)
  await first; await second
  log.length = 0
  const veto = { value: 2, veto: true }
  expect(first.update(veto, true)).toBe('vetoed')
  expect(first._config).toBe(veto)
  expect(first.config).toEqual({ value: 1 })
  expect(log).toEqual([['first', 'update', veto, true]])
  log.length = 0
  const nextConfig = { value: 3 }
  await first.update(nextConfig)
  expect(first.config).toBe(nextConfig)
  expect(log).toEqual([['first', 'update', nextConfig, false], ['first', 'dispose'], ['first', 'apply', nextConfig]])
  log.length = 0
  const again = { value: 4 }
  await first.update(again)
  expect(log).toEqual([['first', 'update', again, false], ['first', 'update', again, false], ['first', 'dispose'], ['first', 'apply', again]])
  const hooks = first._hooks['internal/update']
  expect(hooks.length).toBe(3)
  expect(hooks.delete([...hooks][0])).toBe(true)
  expect(hooks.length).toBe(2)
  const disposeHook = first.ctx.on('internal/update', () => 'manual')
  expect(disposeHook()).toBe(true)
  expect(disposeHook()).toBe(false)
  expect(() => first.ctx.on('internal/update', () => {}, true)).toThrow(TypeError)
  log.length = 0
  await first.dispose(); await second.dispose()
  expect(hooks.length).toBe(2)
  expect(() => first.update({ value: 4 })).toThrow()
})

it('preserves per-Fiber update admission, failed restart identity, and recovery', async () => {
  const root = new Context(), failure = new Error('restart failure'), applied = []
  const fiber = await root.plugin({ apply(ctx, config) {
    if (config.fail) throw failure
    applied.push(config)
  } }, { version: 1 })
  const config = { fail: true }
  const restart = fiber.update(config)
  expect(fiber.state).toBe(5)
  await expect(restart).rejects.toBe(failure)
  expect(fiber._config).toBe(config)
  expect(fiber.state).toBe(3)
  await expect(fiber.await()).rejects.toBe(failure)
  expect(fiber.update({ version: 2 })).toBe(undefined)
  await fiber.await()
  expect(fiber.state).toBe(2)
  expect(applied).toEqual([{ version: 1 }, { version: 2 }])
  await fiber.dispose()
})

it('preserves per-Fiber update global ordering and synchronous or Promise veto results', async () => {
  const root = new Context(), log = [], promise = Promise.resolve('held')
  const fiber = root.plugin({ apply(ctx) {
    ctx.on('internal/update', function (config, noSave, next) {
      if (this !== fiber) throw new Error('local update receiver changed')
      log.push(['local', noSave])
      return config.hold ? promise : next()
    })
  } }, {})
  await fiber
  const before = root.on('internal/update', function (config, noSave, next) {
    if (this !== fiber) throw new Error('global update receiver changed')
    log.push(['before', noSave]); return next()
  }, { global: true, prepend: true })
  const after = root.on('internal/update', (config, noSave, next) => { log.push(['after', noSave]); return next() }, { global: true })
  expect(fiber.update({ hold: true }, null)).toBe(promise)
  expect(log).toEqual([['before', null], ['local', null]])
  log.length = 0
  expect(root.waterfall(fiber, 'internal/update', {}, false, () => 'terminal')).toBe('terminal')
  expect(log).toEqual([['before', false], ['local', false], ['after', false]])
  await before(); await after(); await fiber.dispose()
})

it('preserves per-Fiber update validation before hooks and deferred pending config', async () => {
  const root = new Context(), applied = [], updates = []
  const schema = { '~standard': { version: 1, vendor: 'fixture', validate(value) {
    if (value.async) return Promise.resolve({ value })
    if (value.reject) return { issues: [{ message: 'bad value', path: ['value'] }] }
    return { value: { ...value, normalized: true } }
  } } }
  const fiber = root.plugin({ Config: schema, apply(ctx, config) {
    applied.push(config)
    ctx.on('internal/update', (config, noSave, next) => { updates.push(config); return next() })
  } }, { value: 1 })
  await fiber
  expect(applied).toEqual([{ value: 1, normalized: true }])
  const invalid = { reject: true }
  expect(() => fiber.update(invalid)).toThrow('invalid config:\n  - bad value (at value)')
  try { fiber.update(invalid) } catch (error) {
    expect(error).toBeInstanceOf(TypeError)
    expect(error.name).toBe('ValidationError')
    expect(error[Symbol.for('ValidationError')]).toBe(true)
  }
  expect(fiber._config).toBe(invalid)
  expect(fiber.config).toEqual({ value: 1, normalized: true })
  expect(updates).toEqual([])
  expect(() => fiber.update({ async: true })).toThrow('Async config validation is not supported')
  await fiber.update({ value: 2 })
  expect(updates).toEqual([{ value: 2, normalized: true }])
  expect(applied).toEqual([{ value: 1, normalized: true }, { value: 2, normalized: true }])
  await fiber.dispose()
  const opaque = { method() { return 7 } }, pending = root.plugin({ inject: ['late'], apply(ctx, config) { applied.push(config) } }, { value: 0 })
  expect(pending.update(opaque)).toBe(undefined)
  expect(pending._config).toBe(opaque)
  root.provide('late', {})
  await pending
  expect(Object.getPrototypeOf(pending)).toBe(pending.ctx.fiber)
  expect(await pending).toBe(pending.ctx.fiber)
  expect(applied.at(-1)).toEqual({ value: 0 })
  await pending.update(opaque)
  expect(applied.at(-1)).toBe(opaque)
  await pending.dispose()
})

it('preserves callback service tracing, method receivers, and effect ownership', async () => {
  const root = new Context(), origin = root.extend({ label: 'origin' }), tracker = Symbol.for('cordis.tracker'), shadow = Symbol.for('cordis.shadow'), original = Symbol.for('cordis.original')
  class NamedService extends Service { constructor(ctx) { super(ctx, 'traced-named') } }
  const named = new NamedService(root)
  expect(Service.tracker).toBe(tracker)
  expect(Object.getOwnPropertyDescriptor(named, tracker)).toEqual({ value: { property: 'ctx', associate: 'traced-named' }, writable: true, enumerable: false, configurable: false })
  const service = { ctx: origin, [tracker]: { property: 'ctx' }, read() { return this.ctx }, attach(log) { this.ctx.effect(() => () => log.push('disposed')) }, get contextView() { return this.ctx } }
  const listenerContext = root.extend({ label: 'listener' })
  const traced = listenerContext.reflect.trace(service)
  expect(traced.ctx).toBe(listenerContext)
  expect(traced[original]).toBe(service)
  expect(traced.read()).toBe(listenerContext)
  expect(traced.contextView[shadow]).toBe(origin)
  expect(Object.getPrototypeOf(traced.contextView)).toBe(listenerContext)
  expect(Reflect.set(traced, 'ctx', root)).toBe(false)
  expect(Reflect.set(traced, original, root)).toBe(false)
  const external = { ctx: root }
  expect(traced.read.call(external)).toBe(root)
  const child = { ctx: origin, [tracker]: { property: 'ctx' } }
  service.child = child
  service.returnChild = () => child
  expect(traced.child.ctx).toBe(listenerContext)
  expect(traced.returnChild().ctx).toBe(listenerContext)
  const identityAware = { ctx: origin, [tracker]: { property: 'ctx', noShadow: true }, method() { return this.ctx }, get origin() { return this.ctx[shadow] } }
  const identityTrace = listenerContext.reflect.trace(identityAware)
  expect(identityTrace.method).toBe(identityAware.method)
  expect(identityTrace.origin).toBe(origin)
  const shadowCaller = listenerContext.extend({ [shadow]: origin })
  expect(shadowCaller.reflect.trace(service).ctx).toBe(listenerContext)
  expect(shadowCaller.reflect.trace(identityAware).ctx).toBe(shadowCaller)
  const callable = function () { return 'raw' }
  callable.ctx = origin
  callable[tracker] = { property: 'ctx' }
  callable[Symbol.for('cordis.invoke')] = function () { return this.ctx }
  expect(listenerContext.reflect.trace(callable)()).toBe(listenerContext)
  root.provide('trace-space.value', 17)
  const associated = { ctx: origin, [tracker]: { property: 'ctx', associate: 'trace-space' } }
  const associatedTrace = listenerContext.reflect.trace(associated)
  expect(associatedTrace.value).toBe(17)
  associatedTrace.value = 23
  expect(root.get('trace-space.value')).toBe(23)
  const plain = {}, received = []
  const bound = listenerContext.reflect.bind(function (argument, untouched) { received.push(this.ctx, argument.ctx, untouched); return argument.read() })
  expect(bound.call(service, service, plain)).toBe(listenerContext)
  expect(received).toEqual([listenerContext, listenerContext, plain])
  function Receiver(argument) { this.argument = argument }
  const Constructed = listenerContext.reflect.bind(Receiver)
  const instance = new Constructed(service)
  expect(instance).toBeInstanceOf(Receiver)
  expect(instance.argument.ctx).toBe(listenerContext)
  const log = []
  let registered
  const fiber = root.plugin({ name: 'traced-listener', apply(ctx) {
    registered = ctx
    ctx.on('trace/callback', function (argument) { expect(this.ctx).toBe(ctx); argument.attach(log); return argument.read() })
  } })
  await fiber
  expect(root.bail(service, 'trace/callback', service)).toBe(registered)
  await fiber.dispose()
  expect(log).toEqual(['disposed'])
  expect(root.bail(service, 'trace/callback', service)).toBe(undefined)
})

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
