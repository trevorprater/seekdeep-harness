//! Source test imports wired to actual built browser implementations.

pub(super) const ADAPTER: &str = r"
import { readFileSync } from 'node:fs';
import { runInThisContext } from 'node:vm';
import { Context as BrowserContext } from './cordis.mjs';
export { Service, EventsService, RegistryService, Fiber, DisposableList, isBailed, isConstructor, Inject, CordisError, createCallable, joinPrototype, withProps, getTraceable, getPropertyDescriptor, isObject, resolveConfig, ValidationError, symbols, buildOuterStack, composeError, Logger, LoggerService, c16, c256, defaultFormatters } from './cordis.mjs';
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
    ctx.logger.exporter({ levels: { default: 3 }, export(message) { if (message.type === 'warn') console.warn(...message.args); } });
    return ctx;
  }
}
";

pub(super) const GATEWAY_ADDITIONAL: &str = r#"
it('preserves browser Fiber default runner stack capture', async () => {
  const runner = { epoch: true, execute() { throw 'runner failure' }, collect() {} }
  let failure
  try { Fiber.prototype._execute.call({}, runner) } catch (error) { failure = error }
  expect(failure.constructor).toBe(Error)
  expect(failure.message).toBe('runner failure')
  runner.execute = () => Promise.reject('rejected runner')
  await expect(Fiber.prototype._execute.call({}, runner)).rejects.toThrow('rejected runner')
  runner.getOuterStack = null
  runner.execute = () => { throw 'bad stack callback' }
  expect(() => Fiber.prototype._execute.call({}, runner)).toThrow(TypeError)
})

it('preserves browser Fiber constructor parent-effect overrides and failure ordering', async () => {
  const ctx = new (new Context()).constructor(), trace = [], original = new Error('parent effect denied')
  const extend = ctx.extend, effect = ctx.fiber.effect, plugin = () => { trace.push('run') }
  ctx.extend = function (metadata) { if (metadata.fiber) { trace.push('extend'); expect(metadata.fiber.dispose).toBeUndefined() } return extend.call(this, metadata) }
  ctx.fiber.effect = function (setup, label) { trace.push(label); expect(ctx.registry.get(plugin).fibers.length).toBe(0); throw original }
  expect(() => ctx.plugin(plugin)).toThrow(original)
  expect(trace).toEqual(['extend', 'ctx.plugin()'])
  expect(ctx.registry.get(plugin).fibers.length).toBe(0)
  ctx.fiber.effect = function (setup, label) { trace.push('effect'); return effect.call(this, setup, label) }
  const mounted = ctx.plugin(plugin)
  await mounted
  expect(trace.slice(-3)).toEqual(['extend', 'effect', 'run'])
  expect(ctx.registry.get(plugin).fibers.length).toBe(1)
  await mounted.dispose()
  expect(ctx.registry.has(plugin)).toBe(false)
  ctx.fiber.effect = effect
  await ctx.fiber.dispose()
})

it('preserves browser Registry Fiber constructors across independent Cordis copies', async () => {
  const other = await loadCordisCopy(), ctx = new other.Context(), registry = new RegistryService(ctx), trace = []
  const scope = ctx.extend({ registry })
  registry.ctx = scope
  const remove = scope.provide('cross-copy-dependency', { value: 7 })
  const mounted = registry.plugin({ inject: ['cross-copy-dependency'], apply(child) { trace.push(child.constructor === other.Context); child.provide('cross-copy-service', child['cross-copy-dependency']); return () => trace.push('cleanup') } })
  const fiber = await mounted
  expect(fiber).toBeInstanceOf(Fiber)
  expect(fiber).not.toBeInstanceOf(other.Fiber)
  expect(scope.get('cross-copy-service').value).toBe(7)
  await mounted.restart()
  expect(trace).toEqual([true, 'cleanup', true])
  await remove()
  await mounted.await()
  expect(fiber.state).toBe(0)
  expect(scope.get('cross-copy-service')).toBeUndefined()
  const again = scope.provide('cross-copy-dependency', { value: 8 })
  await mounted.await()
  expect(scope.get('cross-copy-service').value).toBe(8)
  await mounted.dispose()
  expect(fiber.uid).toBeNull()
  expect(trace.at(-1)).toBe('cleanup')
  await again()
  await ctx.fiber.dispose()
})

it('preserves browser Inject streaming writes and readonly result failures', () => {
  const trace = [], declarations = []
  declarations[Symbol.iterator] = function* () { try { trace.push('first'); yield 'first'; trace.push('second'); yield 'second' } finally { trace.push('closed') } }
  expect(() => Inject.resolve(declarations, Object.freeze({}))).toThrow(TypeError)
  expect(trace).toEqual(['first', 'closed'])
  expect(() => Inject.resolve({ name: 7 }, Object.freeze({ name: 1 }))).toThrow(TypeError)
  const parent = { inherited: 3 }, child = Object.create(parent)
  Object.defineProperty(child, symbols.checkProto, { value: true })
  expect(() => Inject.resolve(child, Object.freeze({}))).toThrow(TypeError)
})

it('preserves browser Registry prototype descriptors and iterator closure', async () => {
  const ctx = new Context(), registry = new RegistryService(ctx), original = new Error('dispose failed'), trace = []
  expect(Object.keys(registry)).toEqual(['ctx', '_counter', '_internal'])
  expect(Object.getOwnPropertyNames(RegistryService.prototype)).toEqual(['constructor', 'counter', 'size', 'resolve', 'get', 'has', 'delete', 'keys', 'values', 'entries', 'forEach', 'inject', 'plugin'])
  for (const name of ['counter', 'size']) {
    const descriptor = Object.getOwnPropertyDescriptor(RegistryService.prototype, name)
    expect(descriptor.get.name).toBe('get ' + name)
    expect([descriptor.enumerable, descriptor.configurable, descriptor.set]).toEqual([false, true, undefined])
  }
  const callback = () => {}, runtime = { callback, fibers: { [Symbol.iterator]() { return { next() { return { value: { dispose() { trace.push('dispose'); throw original } }, done: false } }, return() { trace.push('return'); throw new Error('close failed') } } } } }
  registry._internal.set(callback, runtime)
  expect(() => registry.delete(callback)).toThrow(original)
  expect(trace).toEqual(['dispose', 'return'])
  expect(registry.has(callback)).toBe(false)
  await ctx.fiber.dispose()
})

it('preserves browser Fiber structural lifecycle receivers and cleanup', async () => {
  const ctx = new Context(), trace = []
  const owner = Object.assign(Object.create(Fiber.prototype), {
    uid: 1, state: 0, ctx, context: ctx, inject: {}, runtime: null,
    _config: { value: 7 }, config: undefined, _error: undefined, inertia: undefined,
    _store: Object.create(null), store: undefined, _disposables: new DisposableList(),
    _runner: { epoch: '__INACTIVE__', getOuterStack: () => [], execute() { trace.push(['execute', this === owner, this.config]); return () => trace.push(['cleanup']) }, collect(dispose) { owner._disposables.push(dispose) } },
  })
  owner._setEpoch('')
  expect(owner.state).toBe(1)
  expect(await owner.await()).toBe(owner)
  expect(owner.state).toBe(2)
  expect(trace).toEqual([['execute', true, { value: 7 }]])
  await owner.restart()
  expect(trace).toEqual([['execute', true, { value: 7 }], ['cleanup'], ['execute', true, { value: 7 }]])
  owner._setEpoch('__INACTIVE__')
  await owner.await()
  expect(owner.state).toBe(0)
  expect(owner.store).toBeUndefined()
  expect(trace.at(-1)).toEqual(['cleanup'])
  const original = new Error('clear failed')
  owner._disposables = { clear() { throw original } }
  let failed
  expect(() => { failed = owner._unload() }).not.toThrow()
  await expect(failed).rejects.toBe(original)
  await ctx.fiber.dispose()
})

it('preserves browser Fiber structural dependency refresh', () => {
  const epochs = [], owner = { inject: { first: null, second: {} }, _store: { first: { fiber: { uid: 7 } }, second: { fiber: { uid: 9 } } }, _setEpoch(epoch) { epochs.push(epoch) } }
  Fiber.prototype._refresh.call(owner)
  delete owner._store.second
  Fiber.prototype._refresh.call(owner)
  expect(epochs).toEqual([':7:9', '__INACTIVE__'])
})

it('preserves browser Fiber constructor delegation to the parent Context', async () => {
  const ctx = new Context(), extend = ctx.extend, observed = [], values = []
  ctx.extend = function (metadata) {
    if (metadata.fiber) observed.push([metadata.fiber.ctx, metadata.fiber.context, metadata.fiber.state])
    return extend.call(this, { ...metadata, delegated: true })
  }
  const mounted = ctx.plugin(child => { values.push(child.delegated); child.provide('delegated-service', 7) })
  const fiber = await mounted
  expect(observed).toEqual([[undefined, undefined, 0]])
  expect(values).toEqual([true])
  expect(fiber.ctx.fiber).toBe(fiber)
  expect(ctx.get('delegated-service')).toBe(7)
  await mounted.dispose()
  expect(ctx.get('delegated-service')).toBeUndefined()
  ctx.extend = () => ctx
  const reused = ctx.plugin(child => { values.push(child === ctx); child.provide('parent-service', 8) })
  await reused
  expect(values).toEqual([true, true])
  expect(ctx.reflect._getImpl('parent-service').fiber).toBe(ctx.fiber)
  await reused.dispose()
  expect(ctx.get('parent-service')).toBe(8)
  await ctx.fiber.dispose()
})

it('preserves browser Fiber lifecycle method dispatch and config overrides', async () => {
  const ctx = new Context(), configs = [], mounted = ctx.plugin((_ctx, config) => { configs.push(config) }), core = await mounted, trace = []
  for (const name of ['_setEpoch', '_refresh', '_updateState', '_getState', '_resolveConfig', '_reload', '_unload', '_execute']) {
    const original = core[name]
    core[name] = function (...args) { trace.push(name); return original.apply(this, args) }
  }
  await mounted.update({ next: true })
  expect(trace).toEqual(['_resolveConfig', '_setEpoch', '_updateState', '_unload', '_refresh', '_setEpoch', '_updateState', '_reload', '_resolveConfig', '_execute', '_updateState', '_getState'])
  core._resolveConfig = config => ({ ...config, normalized: true })
  await mounted.update({ value: 7 })
  expect(configs.at(-1)).toEqual({ value: 7, normalized: true })
  await ctx.fiber.dispose()
})

it('preserves browser Fiber own fields and the shared method prototype', async () => {
  const ctx = new Context(), mounted = ctx.plugin(() => {}), fiber = await mounted
  const fields = ['parent', 'inject', 'runtime', 'uid', 'ctx', 'config', '_config', 'state', 'dispose', 'store', 'inertia', '_hooks', '_disposables', 'context', '_error', '_runner', '_store']
  expect(Object.keys(ctx.fiber)).toEqual(fields)
  expect(Object.keys(fiber)).toEqual(fields)
  expect(Object.getOwnPropertyNames(Fiber.prototype)).toEqual(['constructor', 'name', 'assertActive', '_execute', 'effect', 'getEffects', '_getState', '_updateState', '_checkImpl', '_refresh', '_setEpoch', '_resolveConfig', '_reload', '_unload', 'await', 'restart', 'update'])
  for (const owner of [ctx.fiber, fiber]) {
    for (const field of fields) {
      const descriptor = Object.getOwnPropertyDescriptor(owner, field)
      expect(descriptor).toHaveProperty('value')
      expect([descriptor.writable, descriptor.enumerable, descriptor.configurable]).toEqual([true, true, true])
    }
    expect(Object.getPrototypeOf(owner)).toBe(Fiber.prototype)
    expect(owner.context).toBe(owner.ctx)
  }
  for (const method of ['_reload', '_unload', 'await', 'restart']) {
    expect(Object.getPrototypeOf(Fiber.prototype[method])).toBe(Object.getPrototypeOf(async () => {}))
    expect(Fiber.prototype[method].length).toBe(0)
  }
  expect(Fiber.prototype.update.length).toBe(1)
  expect(fiber).toBeInstanceOf(Fiber)
  await mounted.dispose()
  expect(fiber.uid).toBeNull()
  await ctx.fiber.dispose()
})

it('preserves browser Fiber runner identity across activation, restart, and failure', async () => {
  const ctx = new (new Context()).constructor(), trace = []
  const fiber = ctx.plugin(() => { trace.push('initial'); return () => trace.push('initial cleanup') })
  const core = await fiber, runner = core._runner
  expect(Object.keys(runner)).toEqual(['epoch', 'getOuterStack', 'execute', 'collect'])
  expect(runner.epoch).toBe('')
  runner.execute = function () { trace.push(this === fiber ? 'handle restart' : 'core restart'); return () => trace.push('replacement cleanup') }
  await fiber.restart()
  expect(core._runner).toBe(runner)
  expect(trace).toEqual(['initial', 'initial cleanup', 'handle restart'])
  runner.getOuterStack = () => ['    at mutableRunnerOwner()']
  runner.execute = () => { throw 'runner failure' }
  await expect(fiber.restart()).rejects.toThrow('runner failure')
  expect(runner.epoch).toBe('__INACTIVE__')
  const error = ctx.logger.buffer.at(-1).args[0]
  expect(error.stack.endsWith('    at mutableRunnerOwner()')).toBe(true)
  expect(trace.at(-1)).toBe('replacement cleanup')
  await fiber.dispose()
  const rootRunner = ctx.fiber._runner
  rootRunner.execute = () => { trace.push('root restart') }
  await ctx.fiber.restart()
  expect(ctx.fiber._runner).toBe(rootRunner)
  expect(rootRunner.epoch).toBe('')
  expect(trace.at(-1)).toBe('root restart')
  await ctx.fiber.dispose()
})

it('preserves browser Fiber runner execution receivers and collector results', async () => {
  const owner = {}, seen = [], dispose = () => {}
  const runner = { epoch: '', getOuterStack: () => [], execute() { seen.push(this === owner); return dispose }, collect(value) { seen.push([this === runner, value === dispose]); return 7 } }
  expect(Fiber.prototype._execute.call(owner, runner)).toBe(7)
  runner.execute = () => Promise.resolve(dispose)
  expect(await Fiber.prototype._execute.call(owner, runner)).toBeUndefined()
  expect(seen).toEqual([true, [true, true], [true, true]])
  let nextCalls = 0
  runner.execute = () => ({ [Symbol.asyncIterator]() { return { next() { nextCalls++; return { done: true } } } } })
  const pending = Fiber.prototype._execute.call(owner, runner)
  runner.epoch = 'different'
  await pending
  expect(nextCalls).toBe(0)
})

it('preserves browser Fiber state helpers and owned-provider notifications', () => {
  const values = [
    { uid: null, _error: undefined, _runner: { epoch: '' } },
    { uid: 1, _error: new Error('failure'), _runner: { epoch: '' } },
    { uid: 1, _error: undefined, _runner: { epoch: '__INACTIVE__' } },
    { uid: 1, _error: undefined, _runner: { epoch: false } },
    { uid: 1, _error: undefined, _runner: { epoch: '' } },
  ]
  expect(values.map(value => Fiber.prototype._getState.call(value))).toEqual([4, 3, 0, 2, 2])
  const events = [], owner = { state: 2, ...values[2], _getState: Fiber.prototype._getState, context: { emit(name, fiber, previous) { events.push([name, fiber === owner, previous]) } } }
  owner.ctx = { reflect: { store: { own: { fiber: owner, name: 'own' }, other: { fiber: {}, name: 'other' } }, notify(names) { events.push(['notify', names]) } } }
  expect(Fiber.prototype._updateState.call(owner, () => undefined)).toBeUndefined()
  expect(owner.state).toBe(0)
  expect(events).toEqual([['internal/status', true, 2], ['notify', ['own']]])
  Fiber.prototype._updateState.call(owner, () => null)
  expect(events).toHaveLength(2)
  Fiber.prototype._updateState.call(owner, () => 3)
  expect(events.at(-1)).toEqual(['internal/status', true, 0])
  expect(events).toHaveLength(3)
})

it('preserves browser Fiber effect dispatch through the current runner method', async () => {
  const ctx = new (new Context()).constructor(), original = ctx.fiber._execute, seen = []
  ctx.fiber._execute = function (runner) { seen.push([this, Object.keys(runner), runner.epoch]); return original.call(this, runner) }
  let cleaned = 0
  const remove = ctx.effect(() => () => { cleaned++ })
  expect(seen).toHaveLength(1)
  expect(seen[0][0]).toBe(ctx.fiber)
  expect(seen[0][1]).toEqual(['execute', 'epoch', 'collect', 'getOuterStack'])
  expect(seen[0][2]).toBe(true)
  await remove()
  expect(cleaned).toBe(1)
  ctx.fiber._execute = original
  await ctx.fiber.dispose()
})

it('preserves browser reflection structural Context methods and extension overrides', async () => {
  const ctx = new Context(), prototype = ctx.constructor.prototype, calls = []
  const fake = { tag: 'fake', [symbols.isolate]: Object.create(null), [symbols.intercept]: Object.create(null), extend(metadata) { calls.push(metadata); return metadata } }
  const child = prototype.extend.call(fake, { value: 7 })
  expect(Object.getPrototypeOf(child)).toBe(fake)
  expect(child.value).toBe(7)
  const label = Symbol('structural-scope'), config = { marker: true }
  const isolated = prototype.isolate.call(fake, 'service', label)
  expect(isolated[symbols.isolate].service).toBe(label)
  expect(Object.getPrototypeOf(isolated[symbols.isolate])).toBe(fake[symbols.isolate])
  const intercepted = prototype.intercept.call(fake, 'service', config)
  expect(intercepted[symbols.intercept].service).toBe(config)
  expect(calls).toEqual([isolated, intercepted])
  const original = ctx.extend
  ctx.extend = function (metadata) { calls.push(metadata); return original.call(this, { ...metadata, tag: 'extended' }) }
  const scope = ctx.isolate('overridden-scope', label), value = {}
  expect(scope.tag).toBe('extended')
  const remove = scope.provide('overridden-scope', value)
  expect(scope.get('overridden-scope')).toBe(value)
  expect(ctx.get('overridden-scope')).toBeUndefined()
  expect(ctx.isolate('overridden-scope', label).get('overridden-scope')).toBe(value)
  await remove(); await ctx.fiber.dispose()
})

it('preserves browser reflection lazy intercept config and custom extension results', async () => {
  const ctx = new Context()
  let reads = 0
  const config = { get name() { reads++; return 'lazy-config' } }
  const scoped = ctx.intercept('logger', config)
  expect(reads).toBe(0)
  expect(scoped[symbols.intercept].logger).toBe(config)
  expect(scoped.logger().name).toBe('lazy-config')
  expect(reads).toBe(1)
  const sentinel = {}
  ctx.extend = () => sentinel
  expect(ctx.isolate('sentinel')).toBe(sentinel)
  expect(ctx.intercept('logger', config)).toBe(sentinel)
  await ctx.fiber.dispose()
})

it('preserves browser reflection independent provider stores and structural service receivers', async () => {
  const ctx = new (new Context()).constructor(), ReflectService = ctx.reflect.constructor
  const independent = new ReflectService(ctx), remove = independent.provide('independent-record', 10)
  expect(independent.get('independent-record')).toBe(10)
  expect(ctx.get('independent-record')).toBeUndefined()
  expect('independent-record' in independent.props).toBe(true)
  expect('independent-record' in ctx.reflect.props).toBe(false)
  await remove()
  const normal = ctx.provide('independent-record', 11)
  expect(ctx.get('independent-record')).toBe(11)
  await normal()
  const labels = [], scopes = Object.create(null)
  const fiber = { state: 2, store: Object.create(null), effect(setup, label) { labels.push(label); return setup() } }
  const fake = { fiber, [symbols.isolate]: scopes }
  fake.root = fake
  const structural = Object.assign(Object.create(ReflectService.prototype), { ctx: fake, props: Object.create(null), store: Object.create(null), notify() { return [] } })
  const dispose = structural.provide('structural-record', 7)
  expect(structural.get('structural-record')).toBe(7)
  expect(fiber.store['structural-record']).toBe(structural._getImpl('structural-record'))
  expect(labels).toEqual(['ctx.provide("structural-record")'])
  const pending = dispose()
  expect(pending).toBeInstanceOf(Promise)
  await pending
  expect(structural.get('structural-record')).toBeUndefined()
  expect('structural-record' in fiber.store).toBe(false)
  await ctx.fiber.dispose()
})

it('preserves browser reflection shared handlers and target-owned accessor definitions', async () => {
  const ctx = new Context(), handler = ctx.reflect.constructor.handler
  expect(Object.keys(handler)).toEqual(['get', 'set', 'has'])
  const get = handler.get
  let other
  try {
    handler.get = (target, key, receiver) => key === 'handlerMarker' ? 42 : get(target, key, receiver)
    other = new ctx.constructor()
    expect(ctx.handlerMarker).toBe(42)
    expect(other.handlerMarker).toBe(42)
  } finally { handler.get = get }
  ctx.accessor('rootDefinition', { get() { return this.tag } })
  const child = ctx.extend({ tag: 'child', reflect: { props: {}, get() { return 'override' } } })
  expect(child.rootDefinition).toBe('child')
  expect(child.unprovided).toBe('override')
  const target = { reflect: { props: { declared: { type: 'accessor', get() { return this.tag } } } } }
  expect(handler.has(target, 'declared')).toBe(true)
  expect(handler.get(target, 'declared', { tag: 'structural' })).toBe('structural')
  await ctx.fiber.dispose(); await other.fiber.dispose()
})

it('preserves browser reflection plain metadata and shadow extension layers', async () => {
  const ctx = new Context(), service = { ctx: null, [symbols.tracker]: { property: 'ctx' } }
  const child = ctx.extend({ service })
  expect(child.service).toBe(service)
  ctx.inheritedService = service
  expect(child.inheritedService.ctx).toBe(child)
  expect(child.reflect[symbols.original]).toBe(ctx.reflect[symbols.original])
  const original = ctx.extend({ marker: 'original' }), base = ctx.extend({ marker: 'base' })
  const shadow = base.extend({ [symbols.shadow]: original }), extended = shadow.extend({ value: 7 })
  expect(Object.keys(extended)).toEqual([])
  expect(Object.getOwnPropertyDescriptor(extended, symbols.shadow)?.value).toBe(original)
  expect(Object.keys(Object.getPrototypeOf(extended))).toEqual(['value'])
  expect(Object.getPrototypeOf(Object.getPrototypeOf(extended))).toBe(base)
  expect(getTraceable(extended, extended)).toBe(Object.getPrototypeOf(extended))
  function metadata() {}
  metadata.tag = 8
  const callableMetadata = ctx.extend(metadata)
  expect(callableMetadata.tag).toBe(8)
  expect(Object.getOwnPropertyDescriptor(callableMetadata, 'length')).toEqual(Object.getOwnPropertyDescriptor(metadata, 'length'))
  for (const invalid of [null, 7, 'text']) expect(() => ctx.extend(invalid)).toThrow(TypeError)
  await ctx.fiber.dispose()
})

it('preserves browser reflection Context fields, subclass identity, and method prototypes', async () => {
  const Base = (new Context()).constructor
  class Derived extends Base {}
  const ctx = new Derived(), child = ctx.extend({ tag: 1 }), fiber = ctx.plugin(() => {})
  await fiber.await()
  expect(ctx).toBeInstanceOf(Derived)
  expect(Object.keys(ctx)).toEqual(['root', 'baseUrl', 'fiber', 'reflect', 'registry', 'events', 'logger'])
  expect(Object.getOwnPropertyNames(Base.prototype)).toEqual(['constructor', 'extend', 'isolate', 'intercept'])
  expect([Base.prototype.extend.length, Base.prototype.isolate.length, Base.prototype.intercept.length]).toEqual([0, 2, 2])
  expect(Object.keys(child)).toEqual(['tag'])
  expect(Object.getPrototypeOf(child)).toBe(ctx)
  expect(Object.keys(fiber.ctx)).toEqual(['fiber'])
  expect(Object.getOwnPropertyDescriptor(fiber.ctx, 'fiber')).toEqual({ value: Object.getPrototypeOf(fiber), writable: true, enumerable: true, configurable: true })
  expect(ctx[Symbol.for('nodejs.util.inspect.custom')]()).toBe('Context <root>')
  await ctx.fiber.dispose()
})

it('preserves browser reflection shared prototypes and independent constructor definitions', async () => {
  const ctx = new (new Context()).constructor(), other = new ctx.constructor()
  const first = ctx.reflect[symbols.original], second = other.reflect[symbols.original]
  const ReflectService = first.constructor
  expect(ReflectService.name).toBe('ReflectService')
  expect(Object.getPrototypeOf(first)).toBe(Object.getPrototypeOf(second))
  expect(Object.getOwnPropertyNames(ReflectService.prototype)).toEqual(['constructor', 'get', '_getImpl', 'set', 'provide', 'notify', 'accessor', 'mixin', 'trace', 'bind'])
  class DerivedReflect extends ReflectService {}
  const independent = new DerivedReflect(ctx)
  expect(independent).toBeInstanceOf(DerivedReflect)
  expect(Object.keys(independent)).toEqual(['ctx', 'store', 'props'])
  expect(independent.ctx).toBe(ctx)
  expect(Object.getPrototypeOf(independent.store)).toBeNull()
  expect(Object.getPrototypeOf(independent.props)).toBeNull()
  expect(independent.props).not.toBe(first.props)
  expect(Object.keys(independent.props)).toEqual(Object.keys(first.props))
  independent.accessor('separate-definition', { get() { return 7 } })
  expect('separate-definition' in independent.props).toBe(true)
  expect('separate-definition' in ctx).toBe(false)
  await ctx.fiber.dispose(); await other.fiber.dispose()
  expect(Object.keys(independent.props)).toEqual([])
  expect(typeof ctx.get).toBe('function')
})

it('preserves browser reflection provider cleanup rejection identity and Promise timing', async () => {
  const ctx = new (new Context()).constructor(), original = new Error('withdrawal failed')
  const remove = ctx.provide('failed-withdrawal', 1)
  const reflect = ctx.reflect[symbols.original], notify = reflect.notify
  reflect.notify = () => { throw original }
  let result
  expect(() => { result = remove() }).not.toThrow()
  expect(result).toBeInstanceOf(Promise)
  await expect(result).rejects.toBe(original)
  expect(ctx.get('failed-withdrawal')).toBeUndefined()
  expect(remove()).toBeUndefined()
  reflect.notify = notify
  await ctx.fiber.dispose()
})

it('preserves browser reflection provider effect hooks, notification overrides, and diagnostics', async () => {
  const ctx = new (new Context()).constructor(), calls = []
  const effect = ctx.fiber.effect
  ctx.fiber.effect = function (...args) { calls.push(['effect', args[1]]); return effect.apply(this, args) }
  const reflect = ctx.reflect[symbols.original], notify = reflect.notify
  reflect.notify = function (...args) { calls.push(['notify', args[0]]); return notify.apply(this, args) }
  const remove = ctx.provide('provider-contract', 7)
  expect(typeof remove.then).toBe('function')
  expect(ctx.fiber.getEffects()).toEqual([{ label: 'ctx.provide("provider-contract")', children: [] }])
  expect(ctx.get('provider-contract')).toBe(7)
  await remove()
  expect(calls).toEqual([['effect', 'ctx.provide("provider-contract")'], ['notify', ['provider-contract']], ['notify', ['provider-contract']]])
  expect(ctx.get('provider-contract')).toBeUndefined()
  const skipped = {}, previous = calls.length
  ctx.fiber.effect = () => skipped
  expect(ctx.provide('skipped-provider', 9)).toBe(skipped)
  expect('skipped-provider' in ctx).toBe(false)
  expect(calls.length).toBe(previous)
  ctx.fiber.effect = effect
  await ctx.fiber.dispose()
})

it('preserves browser reflection notification filters, mutable Fiber methods, and service events', async () => {
  const ctx = new Context(), calls = [], delivered = []
  ctx.provide('notification-value', 7)
  const left = ctx.extend({ lane: 'left' }), right = ctx.extend({ lane: 'right' })
  left.on('internal/service', function (name, value) { delivered.push(['left', name, value]) })
  right.on('internal/service', function (name, value) { delivered.push(['right', name, value]) })
  function fiber(context, inject) { return { ctx: context, inject, _checkImpl(name) { calls.push(['check', context.lane, name]) }, _refresh() { calls.push(['refresh', context.lane]) } } }
  const leftFiber = fiber(left, { 'notification-value': true }), rightFiber = fiber(right, { 'notification-value': true })
  const metadata = ctx.extend({ registry: { values() { return [{ fibers: [leftFiber, rightFiber] }] } } })
  const selected = metadata.reflect.notify(['notification-value'], (context, name) => context.lane === 'right')
  expect(selected).toEqual([rightFiber])
  expect(calls).toEqual([['check', 'right', 'notification-value'], ['refresh', 'right']])
  expect(delivered).toEqual([['right', 'notification-value', 7]])
  const original = new Error('filter failure')
  expect(() => metadata.reflect.notify(['notification-value'], () => { throw original })).toThrow(original)
  expect(delivered).toHaveLength(1)
  await ctx.fiber.dispose()
})

it('preserves browser reflection real dependency notifications with custom scope selection', async () => {
  const ctx = new Context(), trace = [], scopes = ['left', 'right'].map(lane => ctx.isolate('filtered-service').extend({ lane }))
  const values = scopes.map(scope => ({ ctx: scope, enabled: false }))
  values.forEach(value => Object.defineProperty(value, symbols.tracker, { value: { property: 'ctx' } }))
  const removers = scopes.map((scope, index) => scope.reflect.provide('filtered-service', values[index], function () { return this.enabled }))
  const fibers = scopes.map(scope => scope.inject(['filtered-service'], child => { trace.push(child.lane); return () => trace.push('remove:' + child.lane) }))
  await Promise.all(fibers.map(fiber => fiber.await()))
  expect(trace).toEqual([])
  values.forEach(value => { value.enabled = true })
  const selected = ctx.reflect.notify(['filtered-service'], context => context.lane === 'right')
  expect(selected).toEqual([Object.getPrototypeOf(fibers[1])])
  await Promise.all(selected.map(fiber => fiber.await()))
  expect(trace).toEqual(['right'])
  expect(fibers[0].state).toBe(0)
  const left = scopes[0].reflect.notify(['filtered-service'])
  await Promise.all(left.map(fiber => fiber.await()))
  expect(trace).toEqual(['right', 'left'])
  await Promise.all(removers.map(remove => remove()))
  expect(trace.slice(2).sort()).toEqual(['remove:left', 'remove:right'])
  await ctx.fiber.dispose()
})

it('preserves browser reflection accessor receivers, caller errors, and reversible definitions', async () => {
  const ctx = new Context(), child = ctx.extend({ tag: 'child' }), calls = []
  let current = 1
  const remove = ctx.accessor('computed', {
    get(receiver, error) { calls.push(['get', this, receiver, error]); return current },
    set(value, receiver, error) { calls.push(['set', this, receiver, error]); current = value; return true },
  })
  expect('computed' in child).toBe(true)
  expect(child.get('computed')).toBeUndefined()
  expect(child.computed).toBe(1)
  child.computed = 7
  expect(child.computed).toBe(7)
  expect(calls.map(call => call.slice(0, 3))).toEqual([['get', child, undefined], ['set', child, undefined], ['get', child, undefined]])
  expect(calls.every(call => call[3] instanceof Error)).toBe(true)
  expect(calls[0][3].message).toBe('cannot get property "computed" without inject')
  expect(calls[1][3].message).toBe('cannot set property "computed" without provide')
  expect(calls[0][3]).not.toBe(calls[2][3])
  const receiver = {}, associated = withProps(child, { [symbols.receiver]: receiver })
  expect(associated.computed).toBe(7)
  expect(calls.at(-1)[2]).toBe(receiver)
  await remove()
  expect('computed' in ctx).toBe(false)
  await ctx.fiber.dispose()
})

it('preserves browser reflection error carriers and exact enhancement boundaries', async () => {
  const ctx = new Context(), unrelated = new Error('unrelated'), seen = []
  ctx.accessor('carrier', { get(receiver, error) { seen.push(error); error.message = 'accessor denied'; error.stack = 'Header\n    at trap()\n    at caller()'; throw error } })
  let caught
  try { void ctx.carrier } catch (error) { caught = error }
  expect(caught).toBe(seen[0])
  expect(caught.stack).toBe('Error: accessor denied\n    at caller()')
  ctx.accessor('foreign', { get() { throw unrelated } })
  const previous = unrelated.stack
  expect(() => ctx.foreign).toThrow(unrelated)
  expect(unrelated.stack).toBe(previous)
  ctx.accessor('setCarrier', { get() {}, set(value, receiver, error) { error.stack = 'Header\n    at trap()\n    at setterCaller()'; throw error } })
  try { ctx.setCarrier = 1 } catch (error) { caught = error }
  expect(caught.stack).toBe('Error: cannot set property "setCarrier" without provide\n    at setterCaller()')
  await ctx.fiber.dispose()
})

it('preserves browser reflection get and set waterfalls with shared errors and continuations', async () => {
  const ctx = new Context(), seen = []
  ctx.provide('intercepted', 3)
  let pluginContext
  const fiber = ctx.plugin(c => { pluginContext = c })
  await fiber.await()
  const removeGet = ctx.on('internal/get', function (context, name, error, next) {
    seen.push(['get', context, name, error])
    if (name === 'virtualGet') return 41
    return next()
  })
  expect(pluginContext.virtualGet).toBe(41)
  let denied
  function reflectedCaller() { return pluginContext.unprovided }
  try { reflectedCaller() } catch (error) { denied = error }
  expect(denied).toBe(seen.at(-1)[3])
  expect(denied.message).toBe('cannot get property "unprovided" without inject')
  expect(denied.stack.split('\n')[1]).toContain('reflectedCaller')
  expect(seen.every(call => call[1] === pluginContext)).toBe(true)
  const before = seen.length
  expect(ctx.intercepted).toBe(3)
  expect(seen.length).toBe(before)
  const removeSet = ctx.on('internal/set', function (context, name, value, error, next) {
    seen.push(['set', context, name, value, error])
    if (value === 8) return true
    return next()
  })
  ctx.intercepted = 8
  expect(ctx.intercepted).toBe(3)
  ctx.intercepted = 9
  expect(ctx.intercepted).toBe(9)
  expect(seen.at(-1).slice(0, 4)).toEqual(['set', ctx, 'intercepted', 9])
  expect(seen.at(-1)[4]).toBeInstanceOf(Error)
  await removeSet(); await removeGet(); await ctx.fiber.dispose()
})

it('preserves browser reflection read-only accessors, declaration conflicts, and dynamic props', async () => {
  const ctx = new Context(), remove = ctx.accessor('fixed', { get() { return 4 } })
  expect(Reflect.set(ctx, 'fixed', 5)).toBe(false)
  expect(() => ctx.set('fixed', 5)).toThrow('cannot set property "fixed" without provide')
  expect(() => ctx.provide('fixed', 5)).toThrow('property "fixed" is already declared as accessor')
  expect(() => ctx.accessor('fixed', { get() {} })).toThrow('property "fixed" is already declared as accessor')
  ctx.reflect.props.fixed.get = function () { return this.tag }
  expect(ctx.extend({ tag: 'live' }).fixed).toBe('live')
  await remove()
  const unprovide = ctx.provide('persistent', 1)
  await unprovide()
  expect(() => ctx.accessor('persistent', { get() {} })).toThrow('property "persistent" is already declared as service')
  await ctx.fiber.dispose()
})

it('preserves browser reflection mixin binding, mapped setters, null values, and rollback', async () => {
  const ctx = new Context(), service = { value: 2, add(n) { return this.value + n } }
  ctx.provide('math', service)
  const remove = ctx.mixin('math', { value: 'number', add: 'plus' })
  expect(ctx.number).toBe(2)
  const plus = ctx.plus
  expect(plus(3)).toBe(5)
  ctx.number = 7
  expect(service.value).toBe(7)
  expect(plus(3)).toBe(10)
  ctx.set('math', null)
  expect(ctx.number).toBeNull()
  await remove()
  expect('number' in ctx).toBe(false)
  expect('plus' in ctx).toBe(false)
  ctx.accessor('taken', { get() {} })
  expect(() => ctx.mixin('math', { value: 'partial', add: 'taken' })).toThrow('already declared as accessor')
  expect('partial' in ctx).toBe(false)
  await ctx.fiber.dispose()
})

it('preserves browser reflection special properties and root metadata writes', async () => {
  const ctx = new Context(), fiber = ctx.plugin(() => {})
  await fiber.await()
  const child = fiber.ctx, key = Symbol('raw')
  for (const property of [key, '_private', 'prototype', 'then', '0', '-1', 'NaN']) {
    expect(child[property]).toBeUndefined()
    expect(Reflect.set(child, property, 7)).toBe(true)
    expect(child[property]).toBe(7)
  }
  ctx.metadataValue = 4
  expect(ctx.metadataValue).toBe(4)
  expect(() => { child.metadataValue = 5 }).toThrow('cannot set property "metadataValue" without provide')
  expect(child.metadataValue).toBe(4)
  delete child.then
  await ctx.fiber.dispose()
})

it('preserves browser stacks for Fiber setup throws and rejected setup values', async () => {
  const ctx = new Context()
  for (const reason of [17, null, undefined, 'setup value', { toString() { return 'object reason' } }]) {
    let error
    try { ctx.effect(() => { throw reason }) } catch (caught) { error = caught }
    expect(error).toBeInstanceOf(Error)
    expect(error.message).toBe(reason === undefined ? '' : String(reason))
    const effect = ctx.effect(() => Promise.reject(reason))
    await expect(Promise.resolve(effect)).rejects.toBeInstanceOf(Error)
    await expect(effect()).rejects.toBeInstanceOf(Error)
  }
  await ctx.fiber.dispose()
})

it('preserves browser stacks supplied to direct Fiber startup', async () => {
  const ctx = new (new Context()).constructor(), frames = ['    at suppliedOwner()', '    at suppliedEntry()']
  let calls = 0
  const runtime = { name: 'StackOwner', fibers: new DisposableList(), callback() { throw 'startup value' } }
  const fiber = new Fiber(ctx, undefined, {}, runtime, () => { calls++; return frames })
  let error
  try { await fiber.await() } catch (caught) { error = caught }
  expect(error).toBeInstanceOf(Error)
  expect(error.message).toBe('startup value')
  expect(error.stack).toBe('Error: startup value\n' + frames.join('\n'))
  expect(ctx.logger.buffer.at(-1).args[0]).toBe(error)
  expect(calls).toBe(1)
  await fiber.dispose()
  await ctx.fiber.dispose()
})

it('preserves browser stacks for owner teardown and registered caller frames', async () => {
  const ctx = new (new Context()).constructor(), frames = ['    at disposalOwner()']
  const runtime = { name: 'CleanupStack', fibers: new DisposableList(), callback() { return () => { throw 23 } } }
  const fiber = new Fiber(ctx, undefined, {}, runtime, () => frames)
  await fiber.await()
  await fiber.dispose()
  const error = ctx.logger.buffer.at(-1).args[0]
  expect(error).toBeInstanceOf(Error)
  expect(error.stack).toBe('Error: 23\n    at disposalOwner()')
  function registerFailure() { return ctx.plugin({ apply() { throw 'registered failure' } }) }
  const registered = registerFailure()
  let failure
  try { await registered.await() } catch (error) { failure = error }
  expect(failure).toBeInstanceOf(Error)
  expect(failure.stack).toContain('registerFailure')
  await ctx.fiber.dispose()
})

it('preserves browser stacks appended to errors created during startup and cleanup', async () => {
  const ctx = new (new Context()).constructor(), frames = ['    at registeredOwner()', '    at callerEntry()']
  const failing = { name: 'CreatedFailure', fibers: new DisposableList(), callback() { throw new Error('created startup') } }
  const fiber = new Fiber(ctx, undefined, {}, failing, () => frames)
  let error
  try { await fiber.await() } catch (caught) { error = caught }
  expect(error.message).toBe('created startup')
  expect(error.stack.endsWith(frames.join('\n'))).toBe(true)
  await fiber.dispose()
  const cleanup = { name: 'CreatedCleanup', fibers: new DisposableList(), callback() { return () => { throw new Error('created cleanup') } } }
  const active = new Fiber(ctx, undefined, {}, cleanup, () => frames)
  await active.await(); await active.dispose()
  const logged = ctx.logger.buffer.at(-1).args[0]
  expect(logged.message).toBe('created cleanup')
  expect(logged.stack.endsWith(frames.join('\n'))).toBe(true)
  await ctx.fiber.dispose()
})

it('preserves browser stacks on invalid effect results and failed iterators', async () => {
  const ctx = new Context()
  function invalidResult() { return ctx.effect(() => 17) }
  function invalidIterator() { return ctx.effect(() => ({ [Symbol.iterator]() { return { next() { return null } } } })) }
  for (const [execute, label] of [[invalidResult, 'invalidResult'], [invalidIterator, 'invalidIterator']]) {
    let error
    try { execute() } catch (caught) { error = caught }
    expect(error).toBeInstanceOf(TypeError)
    expect(error.stack).toContain(label)
  }
  await ctx.fiber.dispose()
})

it('preserves browser stacks and TypeError for non-callable class initialization hooks', async () => {
  const ctx = new (new Context()).constructor(), frames = ['    at initializationOwner()']
  class InvalidHook { constructor() { this[symbols.initHooks] = [17] } }
  class InvalidInit { constructor() { this[symbols.init] = 17 } }
  for (const callback of [InvalidHook, InvalidInit]) {
    const runtime = { name: 'InvalidInitializer', callback, fibers: new DisposableList() }
    const fiber = new Fiber(ctx, undefined, {}, runtime, () => frames)
    let error
    try { await fiber.await() } catch (caught) { error = caught }
    expect(error).toBeInstanceOf(TypeError)
    expect(error.stack.endsWith(frames.join('\n'))).toBe(true)
    await fiber.dispose()
  }
  await ctx.fiber.dispose()
})

it('preserves browser logger defaults, message identity, clock, and buffering', async () => {
  const ctx = new (new Context()).constructor(), payload = { marker: 1 }, now = Date.now
  try {
    Date.now = () => 1234
    expect(typeof ctx.logger).toBe('function')
    expect(ctx.logger.name).toBe('logger')
    expect(ctx.logger instanceof LoggerService).toBe(false)
    expect(Object.keys(ctx.logger)).toEqual(['bufferSize', 'buffer', 'ctx', '_snMessage', '_snExporter', 'exporters'])
    expect([...ctx.logger.exporters.keys()]).toEqual([1])
    expect(ctx.fiber.getEffects()).toEqual([])
    expect(ctx.get('logger')).toBeUndefined()
    const logger = ctx.logger('topic')
    expect(logger).toBeInstanceOf(Logger)
    logger.info('message', payload)
    logger.error('error')
    logger.warn('suppressed warn')
    logger.debug('suppressed debug')
    expect(ctx.logger._snMessage).toBe(4)
    expect(ctx.logger.buffer.map(message => [message.sn, message.ts, message.name, message.type, message.level])).toEqual([[1, 1234, 'topic', 'info', 1], [2, 1234, 'topic', 'error', 0]])
    expect(ctx.logger.buffer[0].args[1]).toBe(payload)
    expect(ctx.logger.buffer[0].fiber.deref()).toBe(ctx.fiber)
    ctx.logger.bufferSize = 2
    logger.info('third')
    expect(ctx.logger.buffer.map(message => message.args[0])).toEqual(['error', 'third'])
    ctx.logger.bufferSize = 0
    logger.info('zero limit')
    expect(ctx.logger.buffer).toHaveLength(3)
  } finally { Date.now = now; await ctx.fiber.dispose() }
})

it('preserves browser logger scoped names, levels, and shadow-aware Fiber metadata', async () => {
  const ctx = new (new Context()).constructor()
  let origin
  const fiber = ctx.plugin({ name: 'HTTPServer_Test', apply(ctx) { origin = ctx; ctx.logger.info('plugin') } })
  await fiber
  expect(ctx.logger.buffer.at(-1).name).toBe('http-server-test')
  expect(ctx.logger.buffer.at(-1).fiber.deref()).toBe(await fiber)
  const scope = ctx.intercept('logger', { name: 'configured', level: 3 })
  scope.logger.debug('visible debug')
  expect(ctx.logger.buffer.at(-1).name).toBe('configured')
  expect(ctx.logger.buffer.at(-1).level).toBe(3)
  scope.logger('ExplicitName').info('explicit')
  expect(ctx.logger.buffer.at(-1).name).toBe('ExplicitName')
  const service = { ctx: origin, [symbols.tracker]: { property: 'ctx' }, log() { this.ctx.logger.info('shadow') } }
  ctx.reflect.trace(service).log()
  expect(ctx.logger.buffer.at(-1).name).toBe('http-server-test')
  expect(ctx.logger.buffer.at(-1).fiber.deref()).toBe(await fiber)
  await ctx.fiber.dispose()
})

it('preserves browser logger exporter disposal through the current exporter counter', async () => {
  const ctx = new (new Context()).constructor(), received = []
  const first = ctx.logger.exporter({ export(message) { received.push(['first', message.args[0]]) } })
  const second = ctx.logger.exporter({ export(message) { received.push(['second', message.args[0]]) } })
  expect([...ctx.logger.exporters.keys()]).toEqual([1, 2, 3])
  first()
  expect([...ctx.logger.exporters.keys()]).toEqual([1, 2])
  second()
  ctx.logger.info('retained')
  expect(received).toEqual([['first', 'retained']])
  await ctx.fiber.dispose()
  expect([...ctx.logger.exporters.keys()]).toEqual([1, 2])
})

it('preserves browser logger meta overrides, exporter mutation, causes, and aggregates', () => {
  const messages = [], service = { _snMessage: 0, exporters: new Map() }, now = Date.now
  service.exporters.set(1, { levels: { default: 3 }, export(message) { messages.push(message); message.args.push('shared') } })
  service.exporters.set(2, { levels: { default: 3 }, export(message) { messages.push(message) } })
  try {
    Date.now = () => 17
    const logger = new Logger({ name: 'named', meta: { name: 'meta', sn: 99, ts: 88, level: 7, type: 'custom', args: 'ignored' } }, service)
    logger.warn('value')
    expect(service._snMessage).toBe(1)
    expect(messages[0]).not.toBe(messages[1])
    expect(messages[0].args).toBe(messages[1].args)
    expect(messages[1]).toEqual({ name: 'meta', sn: 99, ts: 88, level: 7, type: 'custom', args: ['value', 'shared'] })
    messages.length = 0
    service.exporters.delete(1)
    const plain = new Logger({ name: 'plain' }, service), inner = new Error('inner'), outer = new Error('outer', { cause: inner })
    plain.error(outer)
    expect(messages.map(message => message.args[0])).toEqual([inner, outer])
    messages.length = 0
    plain.error(new AggregateError([inner, outer], 'aggregate'))
    expect(messages.map(message => message.args[0])).toEqual([inner, inner, outer])
  } finally { Date.now = now }
})

it('preserves browser logger formatting, UTF-16 hashes, and mutable formatter tables', () => {
  expect(c16).toEqual([6, 2, 3, 4, 5, 1])
  expect(c256).toHaveLength(75)
  for (const [name, short, long] of [['', 6, 20], ['root', 6, 41], ['HTTPServer_Test', 2, 148], ['🚀', 1, 149], ['a'.repeat(200), 3, 112]]) {
    expect(Logger.code(name, 1)).toBe(short)
    expect(Logger.code(name, 2)).toBe(long)
  }
  expect(Logger.code('root', 0)).toBeUndefined()
  const args = ['%s %d %i %f %o %% %q', Symbol('s'), 3.8, '-2.9', '1.5', { a: 1 }, 'tail']
  expect(Logger.format({}, { name: 'test', args })).toBe('Symbol(s) 3 -2 1.5 {"a":1} % %q tail')
  expect(args).toHaveLength(7)
  expect(Logger.format({ maxLength: 3 }, { name: 'test', args: ['abcdef\r\nxy'] })).toBe('abc...\nxy')
  expect(Logger.format({}, { name: 'test', args: [] })).toBe('undefined')
  expect(Logger.color({ colors: 1 }, 2, 'value', ';1')).toBe('\u001b[32mvalue\u001b[0m')
  expect(Logger.color({ colors: 3 }, 149, 'value', ';1')).toBe('\u001b[38;5;149;1mvalue\u001b[0m')
  const original = defaultFormatters.s
  try {
    defaultFormatters.s = value => '[' + value + ']'
    expect(Logger.format({}, { name: 'test', args: ['%s', 'changed'] })).toBe('[changed]')
  } finally { defaultFormatters.s = original }
})

it('preserves browser logger original Fiber startup and cleanup failures', async () => {
  const ctx = new (new Context()).constructor(), startup = new Error('startup failed'), cleanup = new Error('cleanup failed')
  const failed = ctx.plugin({ name: 'StartupFailure', apply() { throw startup } })
  await expect(failed.await()).rejects.toBe(startup)
  expect(ctx.logger.buffer.at(-1).args[0]).toBe(startup)
  expect(ctx.logger.buffer.at(-1).name).toBe('startup-failure')
  await failed.dispose()
  const active = ctx.plugin({ name: 'CleanupFailure', apply() { return () => { throw cleanup } } })
  await active
  await active.dispose()
  expect(ctx.logger.buffer.at(-1).args[0]).toBe(cleanup)
  expect(ctx.logger.buffer.at(-1).name).toBe('cleanup-failure')
  await ctx.fiber.dispose()
})

it('preserves browser logger exporter failures without committing a startup error', async () => {
  const ctx = new (new Context()).constructor(), failure = new Error('export failed'), original = new Error('startup')
  const log = ctx.logger.error
  ctx.logger.error = () => { throw failure }
  const fiber = ctx.plugin({ apply() { throw original } })
  await expect(fiber.await()).rejects.toBe(failure)
  expect(fiber.state).toBe(1)
  expect(fiber._error).toBeUndefined()
  ctx.logger.error = log
  await ctx.fiber.dispose()
})

it('preserves browser logger availability-check ownership and original failures', async () => {
  const ctx = new (new Context()).constructor(), failure = new Error('availability failed')
  const provider = ctx.plugin({ name: 'PredicateOwner', apply(ctx) {
    ctx.reflect.provide('predicate-service', {}, () => { throw failure })
  } })
  await provider
  const consumer = ctx.plugin({ inject: ['predicate-service'], apply() { throw new Error('unavailable consumer ran') } })
  await consumer.await()
  expect(consumer.state).toBe(0)
  expect(ctx.logger.buffer.at(-1).args[0]).toBe(failure)
  expect(ctx.logger.buffer.at(-1).name).toBe('predicate-owner')
  expect(ctx.logger.buffer.at(-1).fiber.deref()).toBe(await provider)
  await ctx.fiber.dispose()
})

it('preserves browser stacks caller offsets and lazy stack reads', () => {
  function capture() { return buildOuterStack() }
  function caller() { return capture()() }
  expect(caller()[0]).toContain('caller')
  function shifted() { return buildOuterStack(1) }
  function intermediate() { return shifted() }
  function outer() { return intermediate()() }
  expect(outer()[0]).toContain('outer')
  const Original = globalThis.Error
  let reads = 0, frames
  try {
    globalThis.Error = new Proxy(Original, { construct(target, args) {
      const error = Reflect.construct(target, args)
      Object.defineProperty(error, 'stack', { get() { reads++; return 'Error\nfirst\nsecond\nthird\nfourth' } })
      return error
    } })
    frames = buildOuterStack()
    expect(reads).toBe(0)
    expect(frames()).toEqual(['third', 'fourth'])
    expect(frames()).toEqual(['third', 'fourth'])
    expect(reads).toBe(2)
    expect(buildOuterStack('1')()).toEqual([])
  } finally { globalThis.Error = Original }
})

it('preserves browser stacks result identity and synchronous error composition', async () => {
  const value = {}, reason = new Error('failure'), supplied = ['    at outer()', '    at entry()']
  expect(composeError(() => value)).toBe(value)
  const promise = Promise.resolve(value), composed = composeError(() => promise)
  expect(composed).not.toBe(promise)
  expect(await composed).toBe(value)
  reason.stack = 'Error: failure\n    at keep()\n    at wrapper (<anonymous>)\n    at bridge()\n    at marker()\n    at removed()'
  let error, outerCalls = 0
  try {
    composeError(info => { info.error = { stack: 'Error\nunused\n    at marker()' }; throw reason }, () => { outerCalls++; return supplied })
  } catch (caught) { error = caught }
  expect(error).toBe(reason)
  expect(reason.stack).toBe('Error: failure\n    at keep()\n' + supplied.join('\n'))
  expect(outerCalls).toBe(1)
  const missing = new Error('unchanged'); missing.stack = 'Error: unchanged\n    at elsewhere()'
  expect(() => composeError(() => { throw missing }, () => { throw new Error('outer should not be read') })).toThrow(missing)
})

it('preserves browser stacks malformed reasons, async failures, and thenable precedence', async () => {
  let wrapped
  try { composeError(() => { throw 17 }, () => ['    at supplied()']) } catch (error) { wrapped = error }
  expect(wrapped).toBeInstanceOf(Error)
  expect(wrapped.message).toBe('17')
  expect(wrapped.stack).toBe('Error: 17\n    at supplied()')
  const reason = new Error('async'), trace = []
  reason.stack = 'Error: async\n    at retained()\n    at marker()\n    at removed()'
  const result = composeError(info => {
    info.error = { stack: 'Error\nunused\n    at marker()' }; info.offset = 0
    return Promise.reject(reason)
  }, () => ['    at asyncOuter()'])
  await expect(result).rejects.toBe(reason)
  expect(reason.stack).toBe('Error: async\n    at retained()\n    at asyncOuter()')
  const returned = {}, thenable = { then(fulfilled, rejected) { trace.push([this, fulfilled, typeof rejected]); return returned } }
  expect(composeError(() => thenable)).toBe(returned)
  expect(trace).toEqual([[thenable, undefined, 'function']])
  const getterFailure = new Error('stack getter failed')
  expect(() => composeError(info => { info.error = { get stack() { throw getterFailure } }; throw reason })).toThrow(getterFailure)
})

it('preserves browser symbols table shape and dynamic effect metadata keys', async () => {
  const names = ['shadow', 'receiver', 'original', 'metadata', 'initHooks', 'checkProto', 'effect', 'filter', 'isolate', 'intercept', 'init', 'check', 'config', 'invoke', 'extend', 'tracker', 'resolveConfig']
  expect(Object.keys(symbols)).toEqual(names)
  for (const name of names) expect(Object.getOwnPropertyDescriptor(symbols, name)).toEqual({ value: Symbol.for('cordis.' + name), enumerable: true, configurable: true, writable: true })
  expect(Object.isFrozen(symbols)).toBe(false)
  const original = symbols.effect, changed = Symbol('changed effect'), events = []
  symbols.effect = changed
  const ctx = new Context()
  try {
    expect(Context.effect).toBe(original)
    const dispose = ctx.effect(() => () => events.push('cleanup'), 'changed')
    expect(dispose[changed]).toEqual({ label: 'changed', children: [] })
    expect(dispose[Context.effect]).toBeUndefined()
    expect(ctx.fiber.getEffects().at(-1)).toBe(dispose[changed])
    symbols.effect = original
    expect(ctx.fiber.getEffects()).toEqual([])
    dispose()
    expect(events).toEqual(['cleanup'])
    await ctx.fiber.dispose()
  } finally { symbols.effect = original }
})

it('preserves browser symbols getter failures and current tracing keys', () => {
  const original = { tracker: symbols.tracker, original: symbols.original }, failure = new Error('symbol getter failed')
  symbols.tracker = Symbol('new tracker'); symbols.original = Symbol('new original')
  try {
    const ctx = {}, target = { ctx: {}, [symbols.tracker]: { property: 'ctx' } }
    const traced = getTraceable(ctx, target)
    expect(traced.ctx).toBe(ctx)
    expect(traced[symbols.original]).toBe(target)
    expect(traced[original.original]).toBeUndefined()
    Object.defineProperty(symbols, 'tracker', { configurable: true, get() { throw failure } })
    expect(() => getTraceable(ctx, {})).toThrow(failure)
    expect(getTraceable(ctx, 1)).toBe(1)
  } finally {
    Object.defineProperty(symbols, 'tracker', { value: original.tracker, configurable: true, writable: true, enumerable: true })
    symbols.original = original.original
  }
})

it('preserves browser symbols and Context filter as separate mutable metadata', async () => {
  const ctx = new Context(), Ctor = ctx.constructor
  const symbolFilter = symbols.filter, classFilter = Ctor.filter, tableKey = Symbol('table filter'), classKey = Symbol('class filter'), calls = []
  symbols.filter = tableKey
  try {
    ctx.on('filtered', () => calls.push('event'))
    ctx.emit({ [classFilter]: () => true, [tableKey]: () => false }, 'filtered')
    expect(calls).toEqual(['event'])
    Ctor.filter = classKey
    ctx.emit({ [classFilter]: () => false, [classKey]: () => true }, 'filtered')
    expect(calls).toEqual(['event', 'event'])
    await ctx.fiber.dispose()
  } finally { symbols.filter = symbolFilter; Ctor.filter = classFilter }
})

it('preserves browser symbols and Service invoke metadata independently', async () => {
  const tableKey = symbols.invoke, classKey = Service.invoke, changed = Symbol('alternate invoke'), ctx = new Context()
  try {
    class First extends Service { static provide = 'first-invoke'; [classKey]() { return 1 } }
    const first = new First(ctx)
    expect(first()).toBe(1)
    Service.invoke = changed
    expect(typeof first[Service.extend]()).toBe('object')
    Service.invoke = classKey
    symbols.invoke = changed
    class Second extends Service { static provide = 'second-invoke'; [changed]() { return 2 } }
    const second = new Second(ctx)
    expect(typeof second).toBe('function')
    expect(second()).toBe(2)
    expect(typeof second[Service.extend]()).toBe('object')
    await ctx.fiber.dispose()
  } finally { symbols.invoke = tableKey; Service.invoke = classKey }
})

it('preserves browser symbols for decorator metadata and class initialization', async () => {
  const names = ['metadata', 'initHooks', 'checkProto', 'init'], original = Object.fromEntries(names.map(name => [name, symbols[name]]))
  for (const name of names) symbols[name] = Symbol('changed ' + name)
  const ctx = new Context(), initializers = [], events = []
  try {
    class Plugin {
      constructor(ctx) { this.ctx = ctx; for (const init of initializers) init.call(this) }
      method() { events.push('method') }
      [symbols.init]() { events.push('init') }
    }
    Inject('symbol-dep')(Plugin, { kind: 'class' })
    Inject('symbol-dep')(Plugin.prototype.method, { kind: 'method', addInitializer(value) { initializers.push(value) } })
    expect(Plugin.inject[symbols.checkProto]).toBe(true)
    expect(Plugin.prototype.method[symbols.metadata].inject).toEqual({ 'symbol-dep': undefined })
    ctx.provide('symbol-dep', {})
    const plugin = ctx.plugin(Plugin)
    await plugin
    await Promise.all([...ctx.registry.values()].flatMap(runtime => [...runtime.fibers]).map(fiber => fiber.await()))
    expect(events).toEqual(['init', 'method'])
    await ctx.fiber.dispose()
  } finally { Object.assign(symbols, original) }
})

it('preserves browser symbols during reflected lookup and Context intercept selection', async () => {
  const ctx = new Context(), original = symbols.isolate, changed = Symbol('isolation key'), label = Symbol('other scope'), failure = new Error('isolation key failed')
  const isolated = ctx.isolate('lookup-key', label), one = { value: 1 }, two = { value: 2 }
  ctx.provide('lookup-key', one)
  isolated.provide('lookup-key', two)
  try {
    symbols.isolate = changed
    expect(() => ctx.get('lookup-key')).toThrow(TypeError)
    ctx[changed] = { 'lookup-key': label }
    expect(ctx.get('lookup-key')).toBe(two)
    expect(ctx.reflect._getImpl('lookup-key').value).toBe(two)
    Object.defineProperty(symbols, 'isolate', { configurable: true, get() { throw failure } })
    expect(() => ctx.get('lookup-key')).toThrow(failure)
  } finally {
    Object.defineProperty(symbols, 'isolate', { value: original, configurable: true, enumerable: true, writable: true })
  }
  const Ctor = ctx.constructor, interceptKey = Ctor.intercept, alternative = Symbol('class intercept'), value = { marker: 1 }
  try {
    Ctor.intercept = alternative
    ctx[alternative] = Object.assign(Object.create({}), { 'lookup-key': value })
    const service = { ctx, name: 'lookup-key' }
    expect(Service.prototype[Service.resolveConfig].call(service)).toEqual(value)
  } finally { Ctor.intercept = interceptKey }
  await ctx.fiber.dispose()
})

it('preserves browser Service static names, availability checks, and constructor receivers', async () => {
  expect(Reflect.ownKeys(Service)).toEqual(['length', 'name', 'prototype', 'init', 'check', 'config', 'invoke', 'extend', 'tracker', 'resolveConfig', Symbol.hasInstance])
  const ctx = new Context(), publications = [], original = new Error('publication rejected')
  const fake = { reflect: { provide(...args) { publications.push(args) } } }
  let checked
  class Named extends Service {
    static provide = 'named-service'
    get [Service.check]() { checked = this; return this.predicate }
    predicate() { return true }
  }
  const instance = new Named(fake)
  expect(instance.name).toBe('named-service')
  expect(checked).toBe(instance)
  expect(publications).toEqual([['named-service', instance, instance.predicate]])
  expect(Object.keys(instance)).toEqual(['ctx', 'name'])
  const broken = { reflect: { provide() { throw original } } }
  expect(() => new Named(broken)).toThrow(original)
  let ready = false, runs = 0
  class Checked extends Service { static provide = 'checked-service'; [Service.check]() { return ready } }
  const provider = ctx.plugin(Checked)
  await provider
  const consumer = ctx.plugin({ inject: ['checked-service'], apply() { runs++ } })
  expect(consumer.state).toBe(0)
  ready = true
  ctx.reflect.notify(['checked-service'])
  await consumer.await()
  expect(runs).toBe(1)
  await provider.dispose()
  await consumer.await()
  expect(consumer.state).toBe(0)
  await ctx.fiber.dispose()
})

it('preserves browser Service callable instances, extension, and isolation filtering', async () => {
  const ctx = new Context()
  class Callable extends Service {
    static provide = 'callable-service'
    constructor(ctx) { super(ctx); this.base = 5 }
    [Service.invoke](value) { return [this.ctx, this.base + value] }
  }
  const callable = new Callable(ctx)
  expect(typeof callable).toBe('function')
  expect(callable).toBeInstanceOf(Callable)
  expect(callable).toBeInstanceOf(Service)
  expect(callable).toBeInstanceOf(Function)
  expect(callable.constructor).toBe(Callable)
  expect(callable.name).toBe('callable-service')
  expect(callable(2)).toEqual([ctx, 7])
  const scoped = ctx.extend({ marker: 'caller' })
  expect(scoped.get('callable-service')(3)).toEqual([scoped, 8])
  const extended = callable[Service.extend]({ base: 20, ctx: scoped })
  expect(Object.getPrototypeOf(extended)).toBe(callable)
  expect(extended(1)).toEqual([scoped, 21])
  expect(callable(1)).toEqual([ctx, 6])
  expect(extended).toBeInstanceOf(Callable)
  expect(callable[Context.filter](ctx)).toBe(true)
  expect(callable[Context.filter](ctx.isolate('callable-service'))).toBe(false)
  await ctx.fiber.dispose()
  expect(ctx.get('callable-service')).toBeUndefined()
})

it('preserves browser Service callable check receivers and cloned-constructor recognition', () => {
  const publications = [], ctx = { reflect: { props: {}, provide(...args) { publications.push(args) } } }
  let checked
  const predicate = () => true
  class Callable extends Service {
    static provide = 'callable-check'
    get [Service.check]() { checked = this; return predicate }
    [Service.invoke]() { return this.ctx }
  }
  const instance = new Callable(ctx)
  expect(checked).not.toBe(instance)
  expect(checked.ctx).toBe(ctx)
  expect(checked.name).toBeUndefined()
  expect(publications).toEqual([['callable-check', instance, predicate]])
  expect(instance()).toBe(ctx)
  const constructor = new Proxy(Callable, {})
  expect({ constructor }).toBeInstanceOf(Callable)
  expect({ constructor }).toBeInstanceOf(Service)
  for (const value of [null, undefined, false, 0, 1, 'value', {}, () => {}]) expect(value instanceof Service).toBe(false)
})

it('preserves browser Service inherited config order and custom merge receivers', async () => {
  const ctx = new Context(), base = { order: 'base', base: true }, one = { order: 'one', fn: () => 1 }, two = { order: 'two', two: true }, head = { order: 'head', head: true }
  const scope = ctx.intercept('configured-service', one).intercept('configured-service', two)
  class Configured extends Service { static provide = 'configured-service' }
  const service = new Configured(scope)
  expect(service[Service.resolveConfig](base, head)).toEqual({ base: true, fn: one.fn, two: true, head: true, order: 'head' })
  const merge = { merge(...values) { expect(this).toBe(merge); return values } }
  service.Config = merge
  expect(service[Service.resolveConfig](base, head)).toEqual([base, one, two, head])
  const extended = service[Service.extend]({ label: 7 })
  expect(Object.getPrototypeOf(extended)).toBe(service)
  expect(extended.label).toBe(7)
  expect(service.label).toBeUndefined()
  await ctx.fiber.dispose()
})

it('preserves browser Service helpers and descriptor-aware property overlays', () => {
  for (const value of [null, undefined, false, 0, '', NaN]) expect(isObject(value)).toBe(value)
  expect(isObject(1)).toBe(false)
  expect(isObject({})).toBe(true)
  expect(isObject(() => {})).toBe(true)
  const target = { value: 1 }, props = { value: 2, constructor: 'ignored' }
  const proxy = withProps(target, props)
  expect(proxy.value).toBe(2)
  expect(proxy.constructor).toBe(Object)
  expect(Object.keys(proxy)).toEqual(['value'])
  proxy.value = 3
  expect(target.value).toBe(3)
  expect(props.value).toBe(2)
  expect(withProps(target, null)).toBe(target)
  const readonly = Object.defineProperty({}, 'value', { value: 4, writable: false })
  expect(Reflect.set(withProps(target, readonly), 'value', 5)).toBe(false)
  const prototype = Object.create(Object.prototype)
  Object.defineProperty(prototype, 'secret', { get() { return this.marker }, enumerable: false })
  const joined = joinPrototype(prototype, Function.prototype)
  expect(Object.getPrototypeOf(joined)).toBe(Function.prototype)
  expect(getPropertyDescriptor(joined, 'secret')).toEqual(Object.getOwnPropertyDescriptor(prototype, 'secret'))
  const ctx = { reflect: { props: {} }, extend(value) { return Object.assign(Object.create(this), value) } }
  const callable = createCallable('helper', { ctx, [Symbol.for('cordis.invoke')]() { return this.ctx } }, { property: 'ctx' })
  expect(callable()).toBe(ctx)
  expect(callable.name).toBe('helper')
})

it('preserves browser Service live tracker metadata and nested primitive failures', () => {
  const reads = [], ctx = { reflect: { props: {} }, 'svc.field': 7 }
  let property = 'owner', associate
  const tracker = {
    get property() { reads.push('property'); return property },
    get associate() { reads.push('associate'); return associate },
    get noShadow() { reads.push('mode'); return false },
  }
  const target = { field: 1, [Symbol.for('cordis.tracker')]: tracker }
  const traced = getTraceable(ctx, target)
  expect(reads).toEqual([])
  expect(traced.field).toBe(1)
  expect(reads).toEqual(['property', 'associate', 'mode'])
  property = 'field'
  expect(traced.field).toBe(ctx)
  property = 'owner'; associate = 'svc'
  ctx.reflect.props['svc.field'] = { type: 'service' }
  expect(traced.field).toBe(7)
  delete ctx.reflect.props['svc.field']
  expect(traced.field).toBe(1)
  const key = Symbol.for('cordis.tracker'), previous = Object.getOwnPropertyDescriptor(Number.prototype, key)
  try {
    Object.defineProperty(Number.prototype, key, { configurable: true, value: {} })
    expect(() => traced.field).toThrow(TypeError)
  } finally {
    if (previous) Object.defineProperty(Number.prototype, key, previous)
    else delete Number.prototype[key]
  }
})

it('preserves browser Service public config resolution and schema getter timing', () => {
  const config = { original: true }, resolved = { resolved: true }, reads = []
  expect(resolveConfig({}, config)).toBe(config)
  const standard = { validate(value) { expect(this).toBe(standard); expect(value).toBe(config); return { value: resolved } } }
  standard.validate.call = () => { throw new Error('mutable call property was used') }
  const runtime = { get Config() { reads.push('Config'); return { '~standard': standard } } }
  expect(resolveConfig(runtime, config)).toBe(resolved)
  expect(reads).toEqual(['Config', 'Config'])
  const original = new Error('validator failed')
  expect(() => resolveConfig({ Config: { '~standard': { validate() { throw original } } } }, config)).toThrow(original)
  expect(() => resolveConfig({ Config: { '~standard': { validate: () => Promise.resolve({ value: 1 }) } } }, config)).toThrow('Async config validation is not supported')
  let count = 0, error
  const issues = { get issues() { return ++count === 1 ? [] : [{ message: 'bad value', path: ['field', 1] }] } }
  try { resolveConfig({ Config: { '~standard': { validate: () => issues } } }, config) } catch (reason) { error = reason }
  expect(count).toBe(2)
  expect(error).toBeInstanceOf(ValidationError)
  expect(error.message).toBe('invalid config:\n  - bad value (at field.1)')
})

it('preserves browser Inject class inheritance and decorator failure behavior', () => {
  const config = { marker: () => 1 }
  class Parent {}
  class Child extends Parent {}
  expect(Inject('parent-dep', config)(Parent, { kind: 'class' })).toBeUndefined()
  Inject('child-dep')(Child, { kind: 'class' })
  expect(Object.getPrototypeOf(Child.inject)).toBe(Parent.inject)
  expect(Object.keys(Parent.inject)).toEqual(['parent-dep'])
  expect(Object.keys(Child.inject)).toEqual(['child-dep'])
  expect(Inject.resolve(Child.inject)).toEqual({ 'parent-dep': config, 'child-dep': null })
  expect(Object.getOwnPropertyDescriptor(Child, 'inject')).toEqual({ value: Child.inject, enumerable: false, configurable: false, writable: true })
  expect(Child.inject[Symbol.for('cordis.checkProto')]).toBe(true)
  expect(() => Inject('x')({}, { kind: 'field' })).toThrow('@Inject() can only be used on class or class methods')
})

it('preserves browser Inject method dependencies, contextual receivers, and cleanup', async () => {
  const ctx = new Context(), initializers = [], events = [], intercept = { marker: () => 1 }
  class Decorated extends Service {
    static provide = 'decorated-service'
    constructor(ctx) { super(ctx); for (const initialize of initializers) initialize.call(this) }
    connect() {
      const owner = this.ctx, version = owner['method-dep'].version
      expect(this).toBeInstanceOf(Decorated)
      expect(owner[Context.intercept]['method-dep']).toBe(intercept)
      events.push(['run', version])
      owner.on('method-event', () => events.push(['event', version]))
      return () => events.push(['cleanup', version])
    }
  }
  Inject('method-dep', intercept)(Decorated.prototype.connect, { kind: 'method', addInitializer(value) { initializers.push(value) } })
  const parent = ctx.plugin(Decorated)
  await parent
  const runtime = [...ctx.registry.values()].find(runtime => runtime.callback !== Decorated)
  const child = [...runtime.fibers][0]
  expect(child.state).toBe(0)
  expect(events).toEqual([])
  const first = ctx.provide('method-dep', { version: 1 })
  await child.await()
  ctx.emit('method-event')
  await first()
  await child.await()
  expect(events).toEqual([['run', 1], ['event', 1], ['cleanup', 1]])
  const second = ctx.provide('method-dep', { version: 2 })
  await child.await()
  await parent.dispose()
  ctx.emit('method-event')
  expect(events).toEqual([['run', 1], ['event', 1], ['cleanup', 1], ['run', 2], ['cleanup', 2]])
  expect(ctx.registry.size).toBe(0)
  await second()
  await ctx.fiber.dispose()
})

it('preserves browser Inject repeated method decorators and untracked receivers', async () => {
  const ctx = new Context(), initializers = [], events = []
  let instance
  class Plain {
    constructor(ctx) { instance = this; this.ctx = ctx; for (const initialize of initializers) initialize.call(this) }
    connect() { expect(this).toBe(instance); events.push('run'); return () => events.push('cleanup') }
  }
  const context = { kind: 'method', addInitializer(value) { initializers.push(value) } }
  Inject('repeat-a')(Plain.prototype.connect, context)
  Inject('repeat-b')(Plain.prototype.connect, context)
  expect(Plain.prototype.connect[Symbol.for('cordis.metadata')].inject).toEqual({ 'repeat-a': undefined, 'repeat-b': undefined })
  const parent = ctx.plugin(Plain)
  await parent
  const children = [...ctx.registry.values()].filter(runtime => runtime.callback !== Plain).flatMap(runtime => [...runtime.fibers])
  expect(children).toHaveLength(2)
  const a = ctx.provide('repeat-a', {})
  await Promise.all(children.map(child => child.await()))
  expect(events).toEqual([])
  const b = ctx.provide('repeat-b', {})
  await Promise.all(children.map(child => child.await()))
  expect(events).toEqual(['run', 'run'])
  expect(instance.ctx).toBe(parent.ctx)
  await b()
  await Promise.all(children.map(child => child.await()))
  expect(events).toEqual(['run', 'run', 'cleanup', 'cleanup'])
  await a()
  await ctx.fiber.dispose()
})

it('preserves browser Fiber direct root construction and independent effect ownership', async () => {
  const ctx = new Context(), config = { marker: 1 }, inject = {}, events = []
  class Derived extends Fiber {}
  const fiber = new Derived(ctx, config, inject, null, () => [])
  expect(fiber).toBeInstanceOf(Fiber)
  expect(fiber).toBeInstanceOf(Derived)
  expect(Object.getPrototypeOf(fiber)).toBe(Derived.prototype)
  expect(fiber.constructor).toBe(Derived)
  expect(fiber.parent).toBe(ctx)
  expect(fiber.ctx).toBe(ctx)
  expect(fiber.runtime).toBeNull()
  expect(fiber.inject).toBe(inject)
  expect(fiber._config).toBe(config)
  expect(fiber.config).toBeUndefined()
  expect(fiber.uid).toBe(0)
  expect(fiber.state).toBe(2)
  expect(fiber.name).toBe('root')
  expect(fiber.then).toBeUndefined()
  expect(fiber._disposables).toBeInstanceOf(DisposableList)
  expect(await fiber.await()).toBe(fiber)
  fiber.effect(function () { expect(this).toBe(fiber); return () => events.push('disposed') })
  await ctx.fiber.dispose()
  expect(events).toEqual([])
  await fiber.dispose()
  expect(events).toEqual(['disposed'])
  expect(fiber.state).toBe(2)
  expect(fiber.config).toBe(config)
  expect(fiber.inertia).toBeUndefined()
  await fiber.update({ marker: 2 })
  expect(fiber.config).toEqual({ marker: 2 })
  fiber.uid = null
  expect(() => fiber.effect(() => {})).toThrow(CordisError)
})

it('preserves browser Fiber direct runtime construction without registry insertion', async () => {
  const ctx = new Context(), events = [], config = { marker: 1 }, inject = {}
  const runtime = { name: 'manual', fibers: new DisposableList(), callback: (owner, value) => {
    events.push([owner.fiber, value]); return () => events.push('disposed')
  } }
  const fiber = new Fiber(ctx, config, inject, runtime, () => [])
  expect(fiber).toBeInstanceOf(Fiber)
  expect(Object.getPrototypeOf(fiber)).toBe(Fiber.prototype)
  expect(fiber.then).toBeUndefined()
  expect(fiber.runtime).toBe(runtime)
  expect(fiber.inject).toBe(inject)
  expect(fiber.uid).toBe(1)
  expect(ctx.registry.size).toBe(0)
  expect([...runtime.fibers]).toEqual([fiber])
  await fiber.await()
  expect(events).toEqual([[fiber, config]])
  expect(fiber.ctx.fiber).toBe(fiber)
  await fiber.dispose()
  expect(events.at(-1)).toBe('disposed')
  expect(fiber.uid).toBeNull()
  expect([...runtime.fibers]).toEqual([fiber])
  await ctx.fiber.dispose()
})

it('preserves browser Fiber manual-runtime dependency snapshots outside registry traversal', async () => {
  const ctx = new Context(), events = []
  const remove = ctx.provide('manual-dep', { version: 1 })
  const runtime = { name: 'manual dependency', fibers: new DisposableList(), callback: owner => {
    events.push(owner['manual-dep'].version); return () => events.push('disposed')
  } }
  const fiber = new Fiber(ctx, undefined, { 'manual-dep': null }, runtime, () => [])
  await fiber.await()
  await remove()
  await fiber.await()
  expect(fiber.state).toBe(2)
  expect(fiber.ctx['manual-dep'].version).toBe(1)
  expect(events).toEqual([1])
  await fiber.dispose()
  expect(events).toEqual([1, 'disposed'])
  await ctx.fiber.dispose()
})

it('preserves browser Fiber injected intercept identity and ancestor prototypes', async () => {
  const ctx = new Context(), inherited = { transform: value => value + 1 }, local = { transform: value => value + 2 }
  ctx.provide('intercept-dep', {})
  const scope = ctx.intercept('intercept-dep', inherited)
  expect(scope[Context.intercept]['intercept-dep']).toBe(inherited)
  let ownView, inheritedView
  const own = scope.plugin({ inject: { 'intercept-dep': local }, apply(ctx) { ownView = ctx[Context.intercept] } })
  const fallback = scope.plugin({ inject: ['intercept-dep'], apply(ctx) { inheritedView = ctx[Context.intercept] } })
  await own; await fallback
  expect(ownView['intercept-dep']).toBe(local)
  expect(Object.getPrototypeOf(ownView)).toBe(scope[Context.intercept])
  expect(inheritedView['intercept-dep']).toBe(inherited)
  expect(Object.getPrototypeOf(inheritedView)).toBe(scope[Context.intercept])
  expect(Object.keys(inheritedView)).toEqual([])
  expect(ctx[Context.intercept]['intercept-dep']).toBeUndefined()
  await ctx.fiber.dispose()
})

it('preserves browser Fiber symbol isolation labels and reflected scope keys', async () => {
  const ctx = new Context(), shared = Symbol('shared'), other = Symbol('shared')
  const first = ctx.isolate('symbol-scope', shared), second = ctx.isolate('symbol-scope', shared), separate = ctx.isolate('symbol-scope', other)
  expect(first[Context.isolate]['symbol-scope']).toBe(shared)
  expect(Object.getPrototypeOf(first[Context.isolate])).toBe(ctx[Context.isolate])
  const value = { marker: 17 }, remove = first.provide('symbol-scope', value)
  expect(second.get('symbol-scope')).toBe(value)
  expect(separate.get('symbol-scope')).toBeUndefined()
  expect(ctx.get('symbol-scope')).toBeUndefined()
  expect(ctx.reflect.store[shared].value).toBe(value)
  expect(typeof ctx[Context.isolate]['symbol-scope']).toBe('symbol')
  expect(ctx[Context.isolate]['symbol-scope']).not.toBe(shared)
  const anonymous = ctx.isolate('symbol-scope')
  expect(typeof anonymous[Context.isolate]['symbol-scope']).toBe('symbol')
  expect(anonymous[Context.isolate]['symbol-scope']).not.toBe(shared)
  await remove()
  expect(second.get('symbol-scope')).toBeUndefined()
  expect(ctx.reflect.store[shared]).toBeUndefined()
  await ctx.fiber.dispose()
})

it('preserves browser Fiber DisposableList fields, borrowed methods, and current-map removal', () => {
  const list = new DisposableList(), first = {}, second = {}
  expect(Object.keys(list)).toEqual(['sn', 'map', 'weak'])
  const remove = list.push(first)
  list.push(first)
  expect(list.delete(first)).toBe(true)
  expect(list.delete(first)).toBe(false)
  expect([...list]).toEqual([first])
  list.map = new Map([[1, second]])
  expect(remove()).toBe(true)
  expect(list.length).toBe(0)
  expect(() => list.push(1)).toThrow(TypeError)
  expect(list.sn).toBe(3)
  expect(list.clear()).toEqual([1])
  const fake = { sn: 0, map: new Map(), weak: new WeakMap(), [Symbol.iterator]: DisposableList.prototype[Symbol.iterator] }
  const cleanup = DisposableList.prototype.push.call(fake, first)
  expect(Object.getOwnPropertyDescriptor(DisposableList.prototype, 'length').get.call(fake)).toBe(1)
  expect(DisposableList.prototype[Symbol.for('nodejs.util.inspect.custom')].call(fake)).toEqual([first])
  expect(cleanup()).toBe(true)
})

it('preserves browser Fiber shared callback runtimes, invocation receivers, and registry cleanup', async () => {
  const ctx = new Context(), calls = []
  const callback = { apply(ctx, config) { calls.push([this, ctx, config]) } }.apply
  const schema = { '~standard': { validate: value => ({ value: { resolved: value } }) } }
  const first = ctx.plugin({ name: 'first', apply: callback, Config: schema }, 1)
  const second = ctx.plugin({ name: 'second', apply: callback, Config: { '~standard': { validate() { throw new Error('second descriptor replaced shared Config') } } } }, 2)
  const one = await first, two = await second
  const runtime = ctx.registry.get(callback)
  expect(runtime).toBe(one.runtime)
  expect(runtime).toBe(two.runtime)
  expect(runtime.name).toBe('first')
  expect(runtime.Config).toBe(schema)
  expect(calls).toEqual([[runtime, one.ctx, { resolved: 1 }], [runtime, two.ctx, { resolved: 2 }]])
  expect([...runtime.fibers]).toEqual([one, two])
  expect(ctx.registry.size).toBe(1)
  expect([...ctx.registry.keys()]).toEqual([callback])
  expect([...ctx.registry.values()]).toEqual([runtime])
  expect([...ctx.registry.entries()]).toEqual([[callback, runtime]])
  const visited = []
  ctx.registry.forEach((value, key, map) => visited.push([value, key, map]))
  expect(visited).toEqual([[runtime, callback, ctx.registry._internal]])
  await first.dispose()
  expect([...runtime.fibers]).toEqual([two])
  expect(ctx.registry.delete(callback)).toBe(runtime)
  expect(two.uid).toBeNull()
  expect(ctx.registry.has(callback)).toBe(false)
  await two.await()
  expect([...runtime.fibers]).toEqual([two])
  await ctx.fiber.dispose()
})

it('preserves browser Fiber class construction, initialization hooks, and effects', async () => {
  const ctx = new Context(), events = []
  let instance
  class Plugin {
    constructor(owner, config) {
      instance = this
      this.owner = owner
      events.push(['construct', config, new.target === Plugin])
      this[Symbol.for('cordis.initHooks')] = [function () { events.push(['hook', this]) }]
    }
    *[Service.init]() {
      events.push(['init', this === instance])
      yield () => events.push(['cleanup'])
    }
  }
  expect(isConstructor(Plugin)).toBe(true)
  expect(isConstructor(function Ordinary() {})).toBe(true)
  expect(isConstructor(() => {})).toBe(false)
  expect(isConstructor(async () => {})).toBe(false)
  expect(isConstructor(function* () {})).toBe(false)
  expect(isConstructor(async function* () {})).toBe(false)
  const handle = ctx.plugin(Plugin, 42)
  await handle
  expect(instance.owner).toBe(handle.ctx)
  expect(events).toEqual([['construct', 42, true], ['hook', undefined], ['init', true]])
  await handle.dispose()
  expect(events.at(-1)).toEqual(['cleanup'])
  await ctx.fiber.dispose()
})

it('preserves browser Fiber class hook failures and iterator cleanup', async () => {
  const ctx = new Context(), events = [], original = new Error('initialization failed')
  class Plugin {
    constructor() {
      this[Symbol.for('cordis.initHooks')] = (function* () {
        try { yield () => { events.push('hook'); throw original } }
        finally { events.push('closed') }
      })()
    }
    [Service.init]() { events.push('unreachable') }
  }
  const handle = ctx.plugin(Plugin)
  await expect(handle.await()).rejects.toBe(original)
  expect(events).toEqual(['hook', 'closed'])
  await handle.dispose()
  const returned = { [Service.init]() { events.push(this === returned ? 'returned init' : 'wrong receiver') } }
  const replacement = ctx.plugin(class { constructor() { return returned } })
  await replacement
  expect(events.at(-1)).toBe('returned init')
  await replacement.dispose()
  await ctx.fiber.dispose()
})

it('preserves browser Fiber registry counters, descriptor errors, and inject resolution', async () => {
  const ctx = new Context()
  expect(ctx.registry instanceof RegistryService).toBe(true)
  expect(ctx.registry.counter).toBe(1)
  const child = ctx.plugin({ apply() {} })
  expect(child.uid).toBe(2)
  await child
  expect(ctx.registry.counter).toBe(3)
  expect(ctx.registry.resolve({ get apply() { throw new Error('hidden descriptor getter') } })).toBeUndefined()
  for (const value of [null, undefined, 1, 'x', {}, { apply: false }]) {
    expect(() => ctx.plugin(value)).toThrow(`invalid plugin, expect function or object with an "apply" method, received ${typeof value}`)
  }
  const parent = { inherited: 1 }, injected = Object.create(parent)
  Object.defineProperty(injected, Symbol.for('cordis.checkProto'), { value: true })
  injected.local = undefined
  expect(Inject.resolve(injected)).toEqual({ inherited: 1, local: null })
  const result = {}
  expect(Inject.resolve(['one', 'two'], result)).toBe(result)
  expect(result).toEqual({ one: null, two: null })
  await ctx.fiber.dispose()
})

it('preserves browser Fiber CordisError constructors, codes, and inactive operations', async () => {
  expect(CordisError.Code).toEqual({ INACTIVE_EFFECT: 'cannot create effect on inactive context' })
  const error = new CordisError('INACTIVE_EFFECT')
  expect(error).toBeInstanceOf(Error)
  expect(error.message).toBe('cannot create effect on inactive context')
  expect(error.name).toBe('Error')
  expect(Object.keys(error)).toEqual(['code'])
  expect(new CordisError('unknown').message).toBe('')
  expect(new CordisError('INACTIVE_EFFECT', '').message).toBe('')
  const ctx = new Context(), child = ctx.plugin({ apply() {} })
  const core = await child
  await child.dispose()
  for (const operation of [() => core.assertActive(), () => core.effect(() => {}), () => core.ctx.effect(() => {}), () => core.update({})]) {
    let error
    try { operation() } catch (reason) { error = reason }
    expect(error).toBeInstanceOf(CordisError)
    expect(error.code).toBe('INACTIVE_EFFECT')
    expect(error.message).toBe(CordisError.Code.INACTIVE_EFFECT)
  }
  await expect(core.restart()).rejects.toBeInstanceOf(CordisError)
  const message = CordisError.Code.INACTIVE_EFFECT
  CordisError.Code.INACTIVE_EFFECT = 'custom diagnostic'
  try {
    expect(() => core.effect(() => {})).toThrow('custom diagnostic')
    expect(new CordisError('INACTIVE_EFFECT').message).toBe('custom diagnostic')
  } finally { CordisError.Code.INACTIVE_EFFECT = message }
  await ctx.fiber.dispose()
})

it('preserves browser Fiber empty root diagnostics and persistent constructor services', async () => {
  const decorated = new Context()
  const ctx = new decorated.constructor()
  expect(ctx.fiber.getEffects()).toEqual([])
  expect(ctx.fiber._disposables.length).toBe(0)
  await ctx.fiber.dispose()
  const events = []
  const handle = ctx.plugin({ apply(ctx) {
    ctx.on('internal/update', function (config) { events.push(config); return 'veto' })
  } }, 0)
  const core = await handle
  expect(core.update(1)).toBe('veto')
  expect(events).toEqual([1])
  expect(core.config).toBe(0)
  await ctx.fiber.dispose()
  expect(ctx.fiber.getEffects()).toEqual([])
  await decorated.fiber.dispose()
})

it('preserves browser Fiber synchronous effect shapes, receivers, and metadata', async () => {
  const ctx = new Context(), events = []
  const baseline = ctx.fiber.getEffects().length
  const dispose = ctx.effect(function* () {
    expect(this).toBe(ctx.fiber)
    expect(ctx.fiber.getEffects().at(-1)).toEqual({ label: 'shape', children: [] })
    events.push('setup')
    yield () => events.push('first')
    yield undefined
    yield null
    return () => events.push('last')
  }, 'shape')
  expect(events).toEqual(['setup'])
  expect(typeof dispose.then).toBe('function')
  expect(dispose.name).toBe('')
  expect(dispose.length).toBe(0)
  expect(() => new dispose()).toThrow(TypeError)
  expect(Object.getPrototypeOf(dispose.then)).toBe(Object.getPrototypeOf(async () => {}))
  expect(ctx.fiber.getEffects()).toHaveLength(baseline + 1)
  const metadata = dispose[Context.effect]
  expect(ctx.fiber.getEffects().at(-1)).toBe(metadata)
  expect(Object.getOwnPropertyDescriptor(dispose, Context.effect)).toEqual({ value: metadata, writable: true, enumerable: false, configurable: false })
  expect(dispose()).toBeUndefined()
  expect(events).toEqual(['setup', 'last', 'first'])
  expect(dispose()).toBeUndefined()
  expect(ctx.fiber.getEffects()).toHaveLength(baseline)
  const plain = ctx.effect(() => {})
  expect(plain[Context.effect]).toEqual({ label: 'anonymous', children: [] })
  plain()
  const nullable = ctx.effect(() => {}, null)
  expect(nullable[Context.effect].label).toBeNull()
  nullable()
  const callable = () => events.push('callable')
  callable.then = () => { throw new Error('functions must be collected directly') }
  ctx.effect(() => callable)()
  expect(events.at(-1)).toBe('callable')
  for (const invalid of [false, 0, '', {}, ['bad'], { [Symbol.iterator]: null }]) {
    expect(() => ctx.effect(() => invalid)).toThrow(TypeError)
    expect(ctx.fiber.getEffects()).toHaveLength(baseline)
  }
  await ctx.fiber.dispose()
})

it('preserves browser Fiber effect setup waits and single-shot pending disposal', async () => {
  const ctx = new Context(), events = []
  let finishSetup, finishCleanup
  const setup = new Promise(resolve => { finishSetup = resolve })
  const cleanup = new Promise(resolve => { finishCleanup = resolve })
  const dispose = ctx.effect(() => setup, 'pending')
  const task = dispose()
  expect(task).toBeInstanceOf(Promise)
  expect(dispose()).toBeUndefined()
  expect(ctx.fiber.getEffects().at(-1).label).toBe('pending')
  finishSetup(() => { events.push('cleanup'); return cleanup })
  const readyDispose = await dispose
  expect(await dispose).toBe(readyDispose)
  expect(readyDispose.name).toBe('disposeAsync')
  expect(typeof readyDispose).toBe('function')
  expect(readyDispose()).toBeUndefined()
  expect(events).toEqual(['cleanup'])
  const teardown = ctx.fiber.dispose()
  let done = false
  teardown.then(() => { done = true })
  await Promise.resolve()
  expect(done).toBe(false)
  finishCleanup()
  await task
  await teardown
  expect(events).toEqual(['cleanup'])
})

it('preserves browser Fiber awaitable setup and nested effect ownership', async () => {
  const ctx = new Context(), events = []
  const baseline = ctx.fiber.getEffects().length
  let finish
  const cleanup = new Promise(resolve => { finish = resolve })
  let inner
  const outer = ctx.effect(function* () {
    yield ctx.effect(() => () => events.push('first'), 'first')
    inner = ctx.effect(() => () => { events.push('last'); return cleanup }, 'last')
    yield inner
  }, 'outer')
  expect(ctx.fiber.getEffects().slice(baseline)).toEqual([
    { label: 'outer', children: [{ label: 'first', children: [] }, { label: 'last', children: [] }] },
  ])
  const started = inner()
  const task = outer()
  expect(events).toEqual(['last'])
  expect(ctx.fiber.getEffects().slice(baseline).map(x => x.label)).toEqual(['outer'])
  finish()
  await started
  await task
  expect(events).toEqual(['last', 'first'])
  expect(ctx.fiber.getEffects()).toHaveLength(baseline)
  const dispose = ctx.effect(async () => () => events.push('awaited'), 'awaited')
  const readyDispose = await dispose
  expect(readyDispose()).toBeUndefined()
  expect(dispose()).toBeUndefined()
  expect(events.at(-1)).toBe('awaited')
  await ctx.fiber.dispose()
})

it('preserves browser Fiber partial effect rollback and original setup failures', async () => {
  const ctx = new Context(), events = [], original = new Error('setup failed')
  const baseline = ctx.fiber.getEffects().length
  expect(() => ctx.effect(function* () {
    yield () => events.push('first')
    yield () => events.push('last')
    throw original
  }, 'failed')).toThrow(original)
  expect(events).toEqual(['last', 'first'])
  expect(ctx.fiber.getEffects()).toHaveLength(baseline)
  let reject
  const gate = new Promise((_, failed) => { reject = failed })
  const dispose = ctx.effect(async function* () {
    yield () => events.push('async rollback')
    await gate
  }, 'async failed')
  await Promise.resolve()
  await Promise.resolve()
  reject(original)
  await expect(Promise.resolve(dispose)).rejects.toBe(original)
  for (let i = 0; i < 8; ++i) await Promise.resolve()
  expect(events).toEqual(['last', 'first', 'async rollback'])
  expect(ctx.fiber.getEffects()).toHaveLength(baseline)
  await expect(dispose()).rejects.toBe(original)
  await ctx.fiber.dispose()
})

it('preserves browser Fiber async iterable cancellation and late collected cleanup', async () => {
  const ctx = new Context(), events = []
  let next
  const gate = new Promise(resolve => { next = resolve })
  const dispose = ctx.effect(() => ({
    [Symbol.asyncIterator]() {
      events.push('iterator')
      return {
        next() { events.push('next'); return gate },
        return() { events.push('return'); return { done: true } },
      }
    },
  }), 'stream')
  expect(events).toEqual(['iterator'])
  await Promise.resolve()
  expect(events).toEqual(['iterator', 'next'])
  const task = dispose()
  next({ value: () => events.push('late cleanup'), done: false })
  await task
  expect(events).toEqual(['iterator', 'next', 'late cleanup'])
  await ctx.fiber.dispose()
})

it('preserves browser Fiber disposal reentered synchronously from effect setup', async () => {
  const ctx = new Context(), events = []
  let task
  const dispose = ctx.effect(function () {
    const owned = [...this._disposables].at(-1)
    task = owned()
    events.push('setup finished')
    return () => events.push('cleanup')
  }, 'reentrant')
  expect(task).toBeInstanceOf(Promise)
  expect(dispose()).toBeUndefined()
  expect(events).toEqual(['setup finished'])
  await task
  expect(events).toEqual(['setup finished', 'cleanup'])
  await ctx.fiber.dispose()
})

it('preserves browser Fiber custom setup thenables and primitive iterator records', async () => {
  const ctx = new Context(), events = []
  const effect = ctx.effect(() => ({ then(collect) {
    events.push(['setup', arguments.length])
    collect(() => { events.push(['cleanup']) })
    return { then(fulfilled, rejected) {
      events.push(['composed', arguments.length, typeof fulfilled, typeof rejected])
      return Promise.resolve()
    } }
  } }))
  const dispose = await effect
  dispose()
  let step = 0
  ctx.effect(() => ({ [Symbol.iterator]() { return {
    next() { return ++step === 1 ? 7 : { value: () => events.push(['primitive cleanup']), done: true } },
  } } }))()
  expect(events).toEqual([['setup', 1], ['composed', 2, 'undefined', 'function'], ['cleanup'], ['primitive cleanup']])
  await ctx.fiber.dispose()
})

it('preserves browser Fiber plugin iterable effects and concurrent structural cleanup', async () => {
  const ctx = new Context(), events = []
  let finish
  const gate = new Promise(resolve => { finish = resolve })
  const handle = ctx.plugin({ *apply(ctx) {
    yield ctx.effect(() => () => events.push('nested'), 'nested')
    yield () => events.push('first')
    return () => { events.push('last'); return gate }
  } })
  const core = await handle
  expect(core.getEffects()).toEqual([{ label: 'nested', children: [] }, { label: 'nested', children: [] }])
  const task = handle.dispose()
  for (let i = 0; i < 6; ++i) await Promise.resolve()
  expect(events).toEqual(['last', 'first', 'nested'])
  finish()
  await task
  expect(core.getEffects()).toEqual([])
  const failed = ctx.plugin({ apply() { return Promise.resolve([() => {}]) } })
  await expect(failed.await()).rejects.toThrow('Invalid effect')
  await failed.dispose()
  await ctx.fiber.dispose()
})

it('preserves browser Fiber plugin async iterator epochs and yielded failure rollback', async () => {
  const ctx = new Context(), events = [], original = new Error('startup stream failed')
  let finish
  const gate = new Promise(resolve => { finish = resolve })
  const handle = ctx.plugin({ async *apply() {
    events.push('start')
    yield () => events.push('first')
    await gate
    yield () => events.push('late')
    events.push('unreachable')
  } })
  for (let i = 0; i < 8; ++i) await Promise.resolve()
  const task = handle.dispose()
  finish()
  await task
  expect(events).toEqual(['start', 'late', 'first'])
  const failed = ctx.plugin({ *apply() {
    yield () => events.push('rollback')
    throw original
  } })
  await expect(failed.await()).rejects.toBe(original)
  expect(events.at(-1)).toBe('rollback')
  await failed.dispose()
  await ctx.fiber.dispose()
})

it('preserves browser Fiber parent ownership while a child disposer is pending', async () => {
  const ctx = new Context(), events = []
  let finish
  const gate = new Promise(resolve => { finish = resolve })
  const child = ctx.plugin({ apply: () => () => { events.push('cleanup'); return gate } })
  await child
  expect(child.dispose[Context.effect]).toEqual({ label: 'ctx.plugin()', children: [] })
  const first = child.dispose()
  expect(child.dispose()).toBeUndefined()
  expect(ctx.fiber.getEffects().some(x => x.label === 'ctx.plugin()')).toBe(true)
  const owner = ctx.fiber.dispose()
  let settled = false
  owner.then(() => { settled = true })
  for (let i = 0; i < 12; ++i) await Promise.resolve()
  expect(events).toEqual(['cleanup'])
  expect(settled).toBe(false)
  finish()
  await first
  await owner
  expect(ctx.fiber.getEffects()).toEqual([])
})

it('preserves browser Fiber status observer failure timing and recovery', async () => {
  const root = new Context(), original = new Error('status observer failed')
  const handle = root.plugin({ apply() {} })
  const core = await handle
  let failed = false
  const remove = root.on('internal/status', fiber => {
    if (fiber === core && fiber.state === 5 && !failed) { failed = true; throw original }
  })
  let update
  expect(() => { update = core.update({}) }).not.toThrow()
  expect(update).toBeInstanceOf(Promise)
  await expect(update).rejects.toBe(original)
  await core.await()
  expect(core.state).toBe(0)
  remove()
  await core.restart()
  expect(core.state).toBe(2)
  await handle.dispose()
  const other = new Context(), startup = new Error('active observer failed')
  other.on('internal/status', fiber => { if (fiber.state === 2) throw startup })
  const pending = other.plugin({ apply() {} })
  await expect(Promise.resolve(pending)).rejects.toBe(startup)
  const active = Object.getPrototypeOf(pending)
  expect(active.state).toBe(2)
  expect(active._error).toBeUndefined()
  expect(await active.await()).toBe(active)
  await pending.dispose()
})

it('preserves browser Fiber overlapping handle and underlying updates', async () => {
  const root = new Context(), trace = []
  let release, generation = 0
  const gate = new Promise(resolve => { release = resolve })
  const handle = root.plugin({ name: 'overlap', apply(ctx, config) {
    const current = generation++
    trace.push(['apply', config.revision])
    ctx.provide('overlap-value', config.revision)
    ctx.effect(() => async () => { if (!current) await gate; trace.push(['cleanup', config.revision]) })
  } }, { revision: 0 })
  await handle
  const core = Object.getPrototypeOf(handle)
  const first = handle.update({ revision: 1 }).then(() => trace.push(['first', 'ok']), error => trace.push(['first', error.message]))
  const second = core.update({ revision: 2 }).then(() => trace.push(['second', 'ok']))
  await new Promise(resolve => setTimeout(resolve, 0))
  trace.push(['overlap', handle.state, core.state, root.get('overlap-value')])
  release()
  await Promise.all([first, second])
  trace.push(['settled', handle.state, core.state, root.get('overlap-value') ?? null])
  expect(trace).toEqual([
    ['apply', 0], ['apply', 2], ['second', 'ok'], ['overlap', 5, 2, 2],
    ['cleanup', 0], ['apply', 1], ['cleanup', 2],
    ['first', 'service "overlap-value" has been registered at <overlap>'],
    ['settled', 3, 2, null],
  ])
  await handle.dispose()
})

it('preserves browser Fiber publication failure identity and rollback', async () => {
  const root = new Context(), original = new Error('publication refused')
  let published, cleaned = 0, applied = 0
  root.on('internal/plugin', fiber => {
    if (fiber.uid === null) return
    published = fiber
    fiber.ctx.effect(() => () => { cleaned++ })
    throw original
  })
  let failure
  try { root.plugin({ apply() { applied++ } }) } catch (error) { failure = error }
  expect(failure).toBe(original)
  expect(published.uid).toBe(null)
  await published.await()
  expect(cleaned).toBe(1)
  expect(applied).toBe(0)
})

it('preserves browser Fiber publication before dependency admission', async () => {
  const root = new Context(), observed = [], runs = []
  root.on('internal/plugin', function (fiber) {
    if (fiber.uid === null) { observed.push(['disposed', fiber.state]); return }
    observed.push(['created', fiber.state, this === null, fiber.ctx.fiber === fiber])
    fiber.inject['late-service'] = null
  })
  const handle = root.plugin({ apply(ctx) { runs.push(ctx['late-service']) } })
  expect(observed).toEqual([['created', 0, true, true]])
  expect(handle.state).toBe(0)
  expect(runs).toEqual([])
  const remove = root.provide('late-service', { value: 17 })
  await handle
  expect(runs).toEqual([{ value: 17 }])
  await handle.dispose()
  expect(observed.at(-1)).toEqual(['disposed', 2])
  remove()
})

it('preserves browser Fiber initial admission and disposal before startup', async () => {
  const root = new Context(), calls = []
  const handle = root.plugin({ apply() { calls.push('apply') } })
  const core = Object.getPrototypeOf(handle)
  expect(core.state).toBe(1)
  expect(core.inertia).toBeInstanceOf(Promise)
  const disposal = handle.dispose()
  expect(core.uid).toBe(null)
  expect(core.state).toBe(1)
  expect(handle.dispose()).toBeUndefined()
  await disposal
  expect(calls).toEqual([])
  expect(core.state).toBe(4)
  expect(core.inertia).toBeUndefined()
})

it('preserves browser Fiber implementation records, dependency snapshots, and cleanup self-access', async () => {
  const root = new Context(), first = { revision: 1 }, second = { revision: 2 }, cleaned = []
  let providerContext, consumerContext
  const provider = root.plugin({ name: 'record-provider', apply(ctx) { providerContext = ctx; ctx.provide('record-value', first) } })
  await provider
  const consumer = root.plugin({ inject: ['record-value'], apply(ctx) {
    consumerContext = ctx
    ctx.effect(() => () => cleaned.push(ctx['record-value']))
  } })
  await consumer
  const providerFiber = providerContext.fiber, consumerFiber = consumerContext.fiber
  const record = root.reflect._getImpl('record-value')
  expect(record.name).toBe('record-value')
  expect(record.value).toBe(first)
  expect(record.fiber).toBe(providerFiber)
  expect(record.check).toBeUndefined()
  expect(Object.getPrototypeOf(root.reflect.store)).toBe(null)
  expect(Object.getPrototypeOf(consumerFiber._store)).toBe(null)
  expect(Object.getPrototypeOf(consumerFiber.store)).toBe(Object.prototype)
  expect(providerFiber.store['record-value']).toBe(record)
  expect(consumerFiber._store['record-value']).toBe(record)
  expect(consumerFiber.store['record-value']).toBe(record)
  record.value = second
  expect(consumerContext['record-value']).toBe(second)
  expect(root.get('record-value')).toBe(second)
  await provider.dispose()
  expect(cleaned).toEqual([second])
  expect(consumerFiber.store).toBeUndefined()
  expect(consumerFiber._store['record-value']).toBeUndefined()
  expect(root.reflect._getImpl('record-value', false)).toBeUndefined()
  expect(root.reflect.props['record-value']).toEqual({ type: 'service' })
  await consumer.dispose()
  const dispose = root.provide('one-disposal', {})
  const firstDisposal = dispose()
  expect(firstDisposal).toBeInstanceOf(Promise)
  expect(dispose()).toBeUndefined()
  expect(root.get('one-disposal')).toBeUndefined()
  await firstDisposal
})

it('preserves browser Fiber failed-initialization store views during handle recovery', async () => {
  const root = new Context(), original = new Error('initial failure')
  const handle = root.plugin({ name: 'store-failure', apply(ctx, config) {
    ctx.provide('failed-store-value', config)
    if (config.fail) throw original
  } }, { fail: true })
  await expect(Promise.resolve(handle)).rejects.toBe(original)
  const core = Object.getPrototypeOf(handle)
  expect(core.store).toBeUndefined()
  expect(handle.update({ fail: false })).toBeUndefined()
  await expect(handle.await()).rejects.toThrow("Cannot set properties of undefined (setting 'failed-store-value')")
  expect(handle.state).toBe(3)
  expect(core.state).toBe(3)
  expect(handle._error).not.toBe(original)
  expect(core._error).toBe(original)
  expect(root.get('failed-store-value')).toBeUndefined()
  expect(root.get('failed-store-value', false)).toEqual({ fail: false })
  await handle.dispose()
})

it('preserves browser Fiber checked implementation snapshots without hiding explicit lookup', async () => {
  const root = new Context(), checks = [], lifecycle = []
  const value = { enabled: false, ctx: root, [Symbol.for('cordis.tracker')]: { property: 'ctx', noShadow: true } }
  const remove = root.reflect.provide('checked-store', value, function () { checks.push(this.ctx); return this.enabled })
  const handle = root.plugin({ inject: ['checked-store'], apply(ctx) {
    lifecycle.push('mounted')
    ctx.effect(() => () => lifecycle.push('disposed'))
  } })
  await handle
  const core = handle.ctx.fiber
  expect(core.state).toBe(0)
  expect(core._store['checked-store']).toBeUndefined()
  expect(root.get('checked-store').enabled).toBe(false)
  value.enabled = true
  const affected = root.reflect.notify(['checked-store'])
  expect(affected).toHaveLength(1)
  expect(affected[0]).toBe(core)
  await Promise.all(affected.map(fiber => fiber.await()))
  expect(core.state).toBe(2)
  expect(core.store['checked-store']).toBe(root.reflect._getImpl('checked-store'))
  expect(checks.every(ctx => ctx.fiber === core)).toBe(true)
  value.enabled = false
  await Promise.all(root.reflect.notify(['checked-store']).map(fiber => fiber.await()))
  expect(core.state).toBe(0)
  expect(core.store).toBeUndefined()
  expect(core._store['checked-store']).toBeUndefined()
  expect(root.get('checked-store').enabled).toBe(false)
  expect(lifecycle).toEqual(['mounted', 'disposed'])
  await handle.dispose()
  await remove()
})

it('preserves browser Fiber receiver shadows, waits, status events, and service visibility', async () => {
  const root = new Context(), statuses = [], snapshots = []
  let handle, core, releaseDispose, releaseApply, generation = 0
  const disposal = new Promise(resolve => { releaseDispose = resolve })
  const application = new Promise(resolve => { releaseApply = resolve })
  const snapshot = () => snapshots.push([handle.state, core.state, Object.hasOwn(handle, 'state'), !!handle.inertia, !!core.inertia, root.get('shadow-value')?.generation ?? null])
  handle = root.plugin({ async apply(ctx) {
    const current = generation++
    ctx.provide('shadow-value', { generation: current })
    if (current) await application
    ctx.effect(() => async () => { if (!current) await disposal })
  } })
  await handle
  core = Object.getPrototypeOf(handle)
  root.on('internal/status', (fiber, old) => statuses.push([fiber === handle ? 'handle' : fiber === core ? 'core' : 'other', fiber.state, old]))
  snapshot()
  const update = handle.update({ revision: 1 })
  snapshot()
  let coreWaited = false
  const waiting = core.await().then(() => { coreWaited = true })
  await new Promise(resolve => setTimeout(resolve, 0))
  snapshot()
  expect(coreWaited).toBe(true)
  releaseDispose()
  await new Promise(resolve => setTimeout(resolve, 0))
  snapshot()
  releaseApply()
  await update
  await waiting
  snapshot()
  await handle.dispose()
  snapshot()
  expect(snapshots).toEqual([
    [2, 2, false, false, false, 0],
    [5, 2, true, true, false, 0],
    [5, 2, true, true, false, null],
    [1, 2, true, true, false, 1],
    [2, 2, true, false, false, 1],
    [2, 4, true, false, false, null],
  ])
  expect(statuses).toEqual([['handle', 5, 2], ['handle', 1, 5], ['handle', 2, 1], ['core', 5, 2], ['core', 4, 5]])
})

it('preserves browser Fiber effect admission after a handle captures its teardown snapshot', async () => {
  const root = new Context(), trace = []
  let child, release, generation = 0
  const gate = new Promise(resolve => { release = resolve })
  const handle = root.plugin({ apply(ctx) {
    child = ctx
    if (!generation++) ctx.effect(() => async () => { await gate; trace.push('old cleanup') })
  } })
  await handle
  const restarting = handle.restart()
  const dispose = child.effect(() => { trace.push('new setup'); return () => trace.push('new cleanup') })
  expect(() => handle.effect(() => () => {})).toThrow('inactive context')
  await new Promise(resolve => setTimeout(resolve, 0))
  expect(trace).toEqual(['new setup'])
  release()
  await restarting
  expect(trace).toEqual(['new setup', 'old cleanup'])
  dispose()
  expect(trace).toEqual(['new setup', 'old cleanup', 'new cleanup'])
  await handle.dispose()
  expect(trace).toEqual(['new setup', 'old cleanup', 'new cleanup'])
})

it('preserves independent EventsService instances, subclasses, and owning Fiber cleanup', async () => {
  const root = new Context(), log = [], constructors = []
  expect(root.events).toBeInstanceOf(EventsService)
  expect(Object.getPrototypeOf(root.events)).toBe(EventsService.prototype)
  for (const [name, length] of Object.entries({ dispatch: 2, parallel: 0, emit: 0, serial: 0, bail: 0, waterfall: 0, register: 4, unregister: 2, on: 3, once: 3 })) {
    const method = EventsService.prototype[name]
    expect(method.name).toBe(name)
    expect(method.length).toBe(length)
    expect(() => Reflect.construct(method, [])).toThrow(TypeError)
  }
  expect(Object.getPrototypeOf(EventsService.prototype.serial)).toBe(Object.getPrototypeOf(async () => {}))
  expect(Object.getPrototypeOf(EventsService.prototype.parallel)).toBe(Object.getPrototypeOf(async () => {}))
  for (const value of [undefined, null, false]) expect(isBailed(value)).toBe(false)
  for (const value of [true, 0, '', NaN, {}, Promise.resolve(false)]) expect(isBailed(value)).toBe(true)
  class Observed extends EventsService {
    on(...args) { constructors.push(args[0]); return super.on(...args) }
  }
  const bus = new Observed(root)
  expect(Object.getPrototypeOf(bus)).toBe(Observed.prototype)
  expect(constructors).toEqual(['internal/listener', 'internal/update'])
  const inherited = Object.create(EventsService.prototype)
  Object.defineProperty(inherited, 'ctx', { set() { throw new Error('inherited ctx setter') } })
  Object.defineProperty(inherited, '_hooks', { set() { throw new Error('inherited hooks setter') } })
  function NewTarget() {}
  NewTarget.prototype = inherited
  let prototypeReads = 0
  const target = new Proxy(NewTarget, { get(object, key, receiver) {
    if (key === 'prototype') prototypeReads++
    return Reflect.get(object, key, receiver)
  } })
  const constructed = Reflect.construct(EventsService, [root], target)
  expect(prototypeReads).toBe(1)
  expect(Object.getPrototypeOf(constructed)).toBe(inherited)
  expect(Object.hasOwn(constructed, 'ctx')).toBe(true)
  expect(Object.hasOwn(constructed, '_hooks')).toBe(true)
  expect(bus._hooks).not.toBe(root.events._hooks)
  const remove = bus.on('independent', value => log.push(value))
  root.emit('independent', 'root')
  bus.emit('independent', 'bus')
  expect(log).toEqual(['bus'])
  let disposed = 0
  const unregister = bus.unregister
  bus.unregister = function (...args) { disposed++; return unregister.apply(this, args) }
  remove()
  expect(disposed).toBe(1)
  let scoped
  const fiber = root.plugin({ apply(ctx) {
    scoped = new EventsService(ctx)
    scoped.on('owned', () => log.push('owned'))
  } })
  await fiber
  scoped.emit('owned')
  await fiber.dispose()
  scoped.emit('owned')
  expect(log).toEqual(['bus', 'owned'])
  expect(scoped._hooks.owned).toHaveLength(0)
  expect(() => new EventsService(scoped.ctx)).toThrow()
})

it('preserves EventService dispatch overrides and borrowed methods', async () => {
  const root = new Context(), bus = new EventsService(root), trace = []
  bus.dispatch = function (mode, args) {
    trace.push([this === bus, mode, [...args]])
    return [() => 17]
  }
  expect(bus.emit('emit', 1)).toBeUndefined()
  expect(bus.bail('bail', 2)).toBe(17)
  expect(await bus.serial('serial', 3)).toBe(17)
  expect(await bus.parallel('parallel', 4)).toBeUndefined()
  expect(bus.waterfall('waterfall', () => 9)).toBe(17)
  expect(trace.map(entry => entry.slice(0, 2))).toEqual([[true, 'emit'], [true, 'bail'], [true, 'serial'], [true, 'emit'], [true, 'waterfall']])
  const callbacks = [() => 19], args = ['borrowed']
  expect(EventsService.prototype.bail.call({ dispatch() { return callbacks } }, ...args)).toBe(19)
  expect(callbacks).toHaveLength(1)
  expect(await EventsService.prototype.serial.call({ dispatch() { return callbacks } }, ...args)).toBe(19)
  expect(callbacks).toHaveLength(1)
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'dispatch')
  Object.defineProperty(globalThis, 'dispatch', { configurable: true, value() { throw new Error('global receiver leaked') } })
  try {
    const detached = EventsService.prototype.emit
    expect(() => detached('detached')).toThrow(TypeError)
  } finally {
    if (previous) Object.defineProperty(globalThis, 'dispatch', previous)
    else delete globalThis.dispatch
  }
  const original = Promise.resolve(1)
  let mapped
  bus.dispatch = () => ({ map(mapper) {
    expect(Object.getPrototypeOf(mapper)).toBe(Object.getPrototypeOf(async () => {}))
    mapped = mapper(() => original)
    return [mapped]
  } })
  await bus.parallel('custom-map')
  expect(mapped).not.toBe(original)
})

it('preserves EventService registration overrides and structural Context owners', () => {
  const labels = [], owner = {
    fiber: { assertActive() {}, effect(setup, label) { labels.push(label); return setup() } },
    reflect: { bind(callback) { return callback } },
  }
  const bus = new EventsService(owner), log = []
  const remove = bus.on('custom-owner', value => log.push(value))
  expect(labels).toEqual(['ctx.on("internal/listener")', 'ctx.on("internal/update")', 'ctx.on("custom-owner")'])
  bus.emit('custom-owner', 3)
  expect(log).toEqual([3])
  expect(remove()).toBe(true)
  const sentinel = () => 1
  let registered
  bus.register = (...args) => { registered = args; return sentinel }
  expect(bus.on('override', () => {}, true)).toBe(sentinel)
  expect(registered[0]).toBe('ctx.on("override")')
  expect(registered[3]).toEqual({ prepend: true })
  bus.bail = () => sentinel
  registered = undefined
  expect(bus.on('intercepted', () => {})).toBe(sentinel)
  expect(registered).toBeUndefined()
  let listener, once = 0
  bus.on = (_name, callback) => { listener = callback; return () => { once++ } }
  bus.once('once', { apply(receiver, values) { log.push([receiver, ...values]) } })
  listener.call(owner, 5)
  expect(once).toBe(1)
  expect(log.at(-1)).toEqual([owner, 5])
})

it('preserves EventService iterator closure on bail and serial failure', async () => {
  const bus = new EventsService(new Context()), closed = []
  bus.dispatch = () => (function* () {
    try { yield () => false; yield () => 23; yield () => { throw new Error('after bail') } }
    finally { closed.push('closed') }
  })()
  expect(bus.bail('iterator')).toBe(23)
  expect(closed).toEqual(['closed'])
  expect(await bus.serial('iterator')).toBe(23)
  expect(closed).toEqual(['closed', 'closed'])
  const original = new Error('callback failed'), cleanup = new Error('cleanup failed')
  bus.dispatch = () => (function* () {
    try { yield () => { throw original } }
    finally { throw cleanup }
  })()
  expect(() => bus.bail('throws')).toThrow(original)
  await expect(bus.serial('throws')).rejects.toBe(original)
  bus.dispatch = () => (function* () {
    try { yield () => 1 }
    finally { throw cleanup }
  })()
  expect(() => bus.bail('close-failure')).toThrow(cleanup)
  await expect(bus.serial('close-failure')).rejects.toBe(cleanup)
})

it('preserves EventService per-Fiber update callback call methods and nullish fallback', () => {
  const bus = new EventsService(new Context()), config = {}, calls = []
  const listener = () => { throw new Error('ignored custom call') }
  listener.call = function (receiver, value, noSave, next) {
    calls.push([receiver, value, noSave])
    return next()
  }
  const fiber = { _hooks: { 'internal/update': [listener] } }
  expect(bus.waterfall(fiber, 'internal/update', config, true, () => 23)).toBe(23)
  expect(calls).toEqual([[fiber, config, true]])
  fiber._hooks['internal/update'] = [null]
  expect(bus.waterfall(fiber, 'internal/update', config, false, () => 19)).toBe(19)
})

it('preserves browser event hook-table identity, mutation, and detached-list disposal', async () => {
  const root = new Context(), calls = []
  expect(root.events).not.toBe(root)
  expect(root.events.ctx).toBe(root)
  const table = root.events._hooks
  expect(Object.getPrototypeOf(table)).toBe(Object.prototype)
  expect(Object.keys(table['internal/listener'][0])).toEqual(['ctx', 'callback', 'prepend'])
  expect(table['internal/listener'][0].prepend).toBeUndefined()
  expect(Object.keys(table['internal/update'][0])).toEqual(['ctx', 'callback', 'global', 'prepend'])
  expect(root.extend({}).events._hooks).toBe(table)
  let child, remove
  const fiber = root.plugin({ apply(ctx) {
    child = ctx
    remove = ctx.on('test/hook-table', value => calls.push(['owned', value]), { global: true, marker: 17 })
  } })
  await fiber
  const list = table['test/hook-table'], hook = list[0]
  expect(hook.ctx).toBe(child)
  expect(hook.global).toBe(true)
  expect(hook.marker).toBe(17)
  expect(typeof hook.callback).toBe('function')
  const callback = hook.callback
  hook.callback = value => calls.push(['replacement', value])
  root.emit('test/hook-table', 1)
  hook.callback = callback
  list.unshift({ ctx: root, callback: value => calls.push(['inserted', value]) })
  root.emit('test/hook-table', 2)
  expect(calls).toEqual([['replacement', 1], ['inserted', 2], ['owned', 2]])
  const replacement = [{ ctx: root, callback: value => calls.push(['new-list', value]) }]
  table['test/hook-table'] = replacement
  remove()
  expect(list).toHaveLength(1)
  expect(table['test/hook-table']).toBe(replacement)
  root.emit('test/hook-table', 3)
  await fiber.dispose()
  expect(table['test/hook-table']).toBe(replacement)
  expect(calls.at(-1)).toEqual(['new-list', 3])
})

it('preserves browser event explicit registration and callback-based unregistration', async () => {
  const root = new Context(), calls = [], list = []
  root.events._hooks['test/explicit-hooks'] = list
  const first = value => calls.push(['first', value])
  const second = value => calls.push(['second', value])
  let remove
  const fiber = root.plugin({ apply(ctx) {
    remove = ctx.events.register('explicit hook', list, first, { marker: 23 })
    ctx.events.register('prepended hook', list, second, { prepend: true })
  } })
  await fiber
  expect(list.map(hook => hook.callback)).toEqual([second, first])
  expect(list[1].marker).toBe(23)
  root.emit('test/explicit-hooks', 1)
  expect(calls).toEqual([['second', 1], ['first', 1]])
  expect(root.events.unregister(list, first)).toBe(true)
  expect(root.events.unregister(list, first)).toBeUndefined()
  remove()
  expect(list.map(hook => hook.callback)).toEqual([second])
  await fiber.dispose()
  expect(list).toHaveLength(0)
  expect(root.events._hooks['test/explicit-hooks']).toBe(list)
})

it('preserves browser event service table replacement and option property evaluation', async () => {
  const root = new Context(), child = root.extend({}), order = [], calls = [], marker = Symbol('marker')
  const table = {}
  child.events._hooks = table
  expect(root.events._hooks).toBe(table)
  const options = {
    get prepend() { order.push('prepend'); return true },
    get global() { order.push('global'); return true },
    get [marker]() { order.push('marker'); return 19 },
  }
  const dispose = child.on('test/options', value => calls.push(value), options)
  expect(order).toEqual(['prepend', 'prepend', 'global', 'marker'])
  expect(table['test/options'][0][marker]).toBe(19)
  expect(table['test/options'][0].ctx).toBe(child)
  root.emit('test/options', 1)
  dispose()
  expect(calls).toEqual([1])
  expect(table['test/options']).toHaveLength(0)
})

it('preserves browser event callback binding failure timing and custom bind methods', () => {
  const root = new Context(), calls = []
  const dispose = root.on('test/non-callable', {})
  expect(() => root.emit('test/non-callable')).toThrow('hook.callback.bind is not a function')
  dispose()
  expect(() => root.on('test/primitive', 123)).toThrow(TypeError)
  const receiver = {}
  const bind = actualReceiver => {
    return value => calls.push([actualReceiver, value])
  }
  bind.call = () => { throw new Error('invoked a replaced call property') }
  const custom = root.on('test/custom-bind', { bind })
  root.emit(receiver, 'test/custom-bind', 7)
  expect(calls).toEqual([[receiver, 7]])
  custom()
  const callback = value => calls.push(value)
  callback.apply = () => { throw new Error('invoked a replaced apply property') }
  const direct = root.on('test/direct-call', callback)
  root.emit('test/direct-call', 9)
  expect(calls.at(-1)).toBe(9)
  direct()
})

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
"#;
