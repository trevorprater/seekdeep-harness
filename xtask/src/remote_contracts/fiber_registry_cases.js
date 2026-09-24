it('preserves browser FiberState numeric reverse entries and mutable descriptors', async () => {
  const namespace = await loadCordisCopy(), states = namespace['Fiber' + 'State']
  const names = ['PENDING', 'LOADING', 'ACTIVE', 'FAILED', 'DISPOSED', 'UNLOADING']
  expect(Object.getPrototypeOf(states)).toBe(Object.prototype)
  expect(Reflect.ownKeys(states)).toEqual(['0', '1', '2', '3', '4', '5', ...names])
  for (const [number, name] of names.entries()) {
    expect(Object.getOwnPropertyDescriptor(states, name)).toEqual({ value: number, writable: true, enumerable: true, configurable: true })
    expect(Object.getOwnPropertyDescriptor(states, number)).toEqual({ value: name, writable: true, enumerable: true, configurable: true })
  }
  try {
    states.ACTIVE = 20
    states[2] = 'REPLACED'
    expect([states.ACTIVE, states[2]]).toEqual([20, 'REPLACED'])
    const root = new namespace.Context()
    expect(root.fiber.state).toBe(2)
    await root.fiber.dispose()
  } finally {
    states.ACTIVE = 2
    states[2] = 'ACTIVE'
  }
})

it('preserves browser Fiber validation issue mapping, sparse arrays, and field evaluation order', () => {
  const sparse = new Array(3)
  sparse[2] = { message: 'last issue' }
  expect(new ValidationError(sparse).message).toBe('invalid config:\n\n\n  - last issue')
  const trace = [], issue = {
    get path() { trace.push('path'); return { join(separator) { trace.push(['join', separator]); return 'field.2' } } },
    get message() { trace.push('message'); return { [Symbol.toPrimitive](hint) { trace.push(hint); return 'invalid field' } } },
  }
  expect(new ValidationError([issue]).message).toBe('invalid config:\n  - invalid field (at field.2)')
  expect(trace).toEqual(['path', 'message', 'string', 'path', ['join', '.']])
  let mapped
  const custom = { map(callback) {
    expect(this).toBe(custom)
    mapped = callback({ message: 'mapped' })
    return { join(separator) { expect(separator).toBe('\n'); return { [Symbol.toPrimitive](hint) { expect(hint).toBe('default'); return 'custom issues' } } } }
  } }
  expect(new ValidationError(custom).message).toBe('invalid config:\ncustom issues')
  expect(mapped).toBe('  - mapped')
  expect(() => new ValidationError([{ message: Symbol('invalid') }])).toThrow(TypeError)
  const failure = new Error('issue path failed')
  expect(() => new ValidationError([{ get path() { throw failure }, get message() { throw new Error('message read too early') } }])).toThrow(failure)
})

it('preserves browser Fiber settled startup failures after disposal', async () => {
  const ctx = new Context(), startupFailure = new Error('retained startup failure'), cleanupFailure = new Error('reported cleanup failure')
  const failed = ctx.plugin(() => { throw startupFailure })
  await expect(failed.await()).rejects.toBe(startupFailure)
  await failed.dispose()
  await expect(failed.await()).rejects.toBe(startupFailure)
  const cleanup = ctx.plugin(() => () => { throw cleanupFailure })
  await cleanup.await()
  await cleanup.dispose()
  await cleanup.await()
  expect(ctx.logger.buffer.some(message => message.args[0] === cleanupFailure)).toBe(true)
  await ctx.fiber.dispose()
})

it('preserves browser Fiber dependency snapshot spreading and strict state writes', async () => {
  const ctx = new Context(), fiber = new Fiber(ctx, {}, {}, null, () => [])
  for (const store of [null, undefined, 7, true]) {
    fiber._store = store
    const pending = fiber._reload()
    expect(fiber.store).toEqual({})
    await pending
  }
  const key = Symbol('dependency'), proto = { value: 'prototype' }, store = {
    get first() { delete this.second; return 'first' },
    second: 'deleted',
    ['__proto__']: proto,
    [key]: 'symbol dependency',
  }
  fiber._store = store
  await fiber._reload()
  expect(Reflect.ownKeys(fiber.store)).toEqual(['first', '__proto__', key])
  expect(Object.getPrototypeOf(fiber.store)).toBe(Object.prototype)
  expect(Object.getOwnPropertyDescriptor(fiber.store, '__proto__')).toEqual({ value: proto, enumerable: true, writable: true, configurable: true })
  const owner = Object.create(Fiber.prototype)
  Object.defineProperty(owner, '_config', { value: 'retained', writable: false })
  owner.uid = 1
  owner.state = 2
  expect(() => owner.update('next')).toThrow(TypeError)
  Object.defineProperty(fiber, 'store', { value: {}, writable: false })
  await expect(fiber._reload()).rejects.toThrow(TypeError)
  await ctx.fiber.dispose()
})

it('preserves browser Fiber live dependency stores and numeric uid coercion', () => {
  const first = {}, second = {}, owner = { _store: first }, implementation = {
    value: {},
    check() { owner._store = second; return true },
  }
  owner.ctx = { reflect: { _getImpl(name, strict) { expect([name, strict]).toEqual(['dependency', true]); return implementation } } }
  expect(Fiber.prototype._checkImpl.call(owner, 'dependency')).toBeUndefined()
  expect(first).toEqual({})
  expect(second.dependency).toBe(implementation)
  const epochs = [], hints = [], reads = []
  const one = { fiber: { uid: { [Symbol.toPrimitive](hint) { hints.push(hint); return 7 } } } }
  const two = { fiber: { uid: 9 } }
  const refresh = { inject: { first: null, second: null }, get _store() { reads.push('store'); return reads.length === 1 ? { first: one } : { second: two } }, _setEpoch(epoch) { epochs.push(epoch) } }
  Fiber.prototype._refresh.call(refresh)
  expect(epochs).toEqual([':7:9'])
  expect(reads).toEqual(['store', 'store'])
  expect(hints).toEqual(['default'])
  const frozen = Object.freeze({ dependency: undefined })
  owner._store = frozen
  implementation.check = undefined
  expect(() => Fiber.prototype._checkImpl.call(owner, 'dependency')).toThrow(TypeError)
})

it('preserves browser Fiber update input identity across raw-config accessors', () => {
  const input = { value: 7 }, normalized = { normalized: true }, writes = [], owner = {
    state: 2,
    assertActive() {},
    set _config(value) { writes.push(value) },
    get _config() { throw new Error('raw config reread') },
    _resolveConfig(value) { expect(value).toBe(input); return normalized },
    context: { waterfall(receiver, name, config, noSave) {
      expect([receiver, name, config, noSave]).toEqual([owner, 'internal/update', normalized, false])
      return 'vetoed'
    } },
  }
  expect(Fiber.prototype.update.call(owner, input)).toBe('vetoed')
  expect(writes).toEqual([input])
})

it('preserves browser Fiber class hook iterator records and cached next methods', async () => {
  const ctx = new Context(), trace = []
  class Hooks {
    constructor() {
      let index = 0
      this[symbols.initHooks] = {
        [Symbol.iterator]() {
          trace.push('iterator')
          return { get next() { trace.push('next getter'); return () => index++ ? { done: true } : { done: false, value() { trace.push('hook') } } } }
        },
      }
    }
    [symbols.init]() { trace.push('init') }
  }
  await ctx.plugin(Hooks)
  expect(trace).toEqual(['iterator', 'next getter', 'hook', 'init'])
  for (const primitive of [1, true, 'text', Symbol('value'), 1n]) expect(isConstructor(primitive)).toBe(false)
  for (const nullable of [null, undefined]) expect(() => isConstructor(nullable)).toThrow(TypeError)
  const generator = function* () {}, asyncGenerator = async function* () {}
  const generatorPrototype = Object.getPrototypeOf(generator), descriptor = Object.getOwnPropertyDescriptor(generatorPrototype, 'constructor')
  try {
    Object.defineProperty(generatorPrototype, 'constructor', { configurable: true, value: function Different() {} })
    expect(isConstructor(generator)).toBe(false)
    expect(isConstructor(asyncGenerator)).toBe(false)
  } finally {
    Object.defineProperty(generatorPrototype, 'constructor', descriptor)
  }
  await ctx.fiber.dispose()
})

it('preserves browser Registry receiver lookup order and current-map deletion', () => {
  const trace = [], callback = () => {}, runtime = { fibers: [] }, second = new Map([[callback, runtime]])
  const first = { get(key) { expect(key).toBe(callback); trace.push('get'); owner._internal = second; return runtime } }
  const owner = { resolve(plugin) { trace.push('resolve'); return plugin }, _internal: first }
  expect(RegistryService.prototype.delete.call(owner, callback)).toBe(runtime)
  expect(trace).toEqual(['resolve', 'get'])
  expect(second.has(callback)).toBe(false)
  const inaccessible = { resolve() { return undefined }, get _internal() { throw new Error('internal map read before callback admission') } }
  expect(RegistryService.prototype.get.call(inaccessible, {})).toBeUndefined()
  expect(RegistryService.prototype.has.call(inaccessible, {})).toBe(false)
  expect(RegistryService.prototype.delete.call(inaccessible, {})).toBeUndefined()
  const counter = Object.getOwnPropertyDescriptor(RegistryService.prototype, 'counter').get
  expect(() => counter.call(Object.freeze({ _counter: 0 }))).toThrow(TypeError)
})

it('preserves browser Registry explicit null inject targets and iterator-driven declarations', () => {
  expect(Inject.resolve(null, null)).toBeNull()
  expect(Inject.resolve(undefined, null)).toBeNull()
  expect(() => Inject.resolve(['dependency'], null)).toThrow(TypeError)
  const result = Object.create(null), calls = [], declaration = ['ignored']
  declaration[Symbol.iterator] = function* () { calls.push('iterate'); yield 'actual' }
  expect(Inject.resolve(declaration, result)).toBe(result)
  expect(result).toEqual({ actual: null })
  expect(calls).toEqual(['iterate'])
})

it('preserves browser Registry Inject method computed prototype fields', () => {
  const initializers = [], contextual = { scoped: true }, instance = {
    ctx: { inject(dependencies, callback) { expect(dependencies).toEqual({ required: undefined }); return callback(contextual) } },
    [symbols.tracker]: { property: '__proto__' },
  }
  function method() { expect(this.__proto__).toBe(contextual); expect(Object.getPrototypeOf(this)).toBe(Object.prototype) }
  Inject('required')(method, { kind: 'method', addInitializer(initializer) { initializers.push(initializer) } })
  initializers[0].call(instance)
  instance[symbols.initHooks][0]()
})

it('preserves browser Fiber captured runtime ownership after metadata replacement', async () => {
  const ctx = new Context(), events = [], callback = () => () => events.push('cleanup')
  const mounted = ctx.plugin(callback), fiber = await mounted, runtime = fiber.runtime
  const replacement = { callback: () => {}, fibers: new DisposableList() }
  fiber.runtime = replacement
  await mounted.dispose()
  expect(events).toEqual(['cleanup'])
  expect(runtime.fibers.length).toBe(0)
  expect(replacement.fibers.length).toBe(0)
  expect(ctx.registry.has(callback)).toBe(false)
  await ctx.fiber.dispose()
})

it('preserves browser Fiber lazy runtime names and eager inject entry snapshots', async () => {
  const ctx = new Context(), trace = [], inherited = ctx[Context.intercept], extend = ctx.extend
  let child
  ctx.extend = function (metadata) { child = extend.call(this, metadata); return child }
  const inject = {
    get first() { trace.push(['first', child[Context.intercept] === inherited]); return { value: 1 } },
    get second() { trace.push(['second', child[Context.intercept] === inherited]); return { value: 2 } },
  }
  const runtime = { callback: () => {}, fibers: new DisposableList(), get name() { trace.push('name'); return 'lazy runtime' } }
  const fiber = new Fiber(ctx, {}, inject, runtime, () => [])
  expect(trace).toEqual([['first', true], ['second', true]])
  expect(fiber.name).toBe('lazy runtime')
  expect(trace.slice(-2)).toEqual(['name', 'name'])
  const name = Object.getOwnPropertyDescriptor(Fiber.prototype, 'name').get
  let runtimeReads = 0
  const owner = { get runtime() { return ++runtimeReads === 1 ? { name: 'selected' } : { name: undefined } } }
  expect(name.call(owner)).toBeUndefined()
  expect(runtimeReads).toBe(2)
  const failure = new Error('runtime reread failed')
  runtimeReads = 0
  Object.defineProperty(owner, 'runtime', { get() { if (++runtimeReads === 1) return { name: 'selected' }; throw failure } })
  expect(() => name.call(owner)).toThrow(failure)
  expect(child[Context.intercept].first).toEqual({ value: 1 })
  expect(Object.getPrototypeOf(child[Context.intercept])).toBe(inherited)
  await fiber.dispose()
  await ctx.fiber.dispose()
})

it('preserves browser Fiber config assignment failures before plugin startup', async () => {
  const ctx = new Context(), calls = [], mounted = ctx.plugin(() => { calls.push('started') }, { value: 7 })
  const fiber = Object.getPrototypeOf(mounted)
  Object.defineProperty(fiber, 'config', { value: undefined, writable: false })
  await expect(mounted).rejects.toThrow(TypeError)
  expect(calls).toEqual([])
  expect(fiber.state).toBe(3)
  await mounted.dispose()
  await ctx.fiber.dispose()
})

it('preserves browser Fiber unload map dispatch, sparse batches, and collection failure identity', async () => {
  const ctx = new Context(), trace = [], fiber = new Fiber(ctx, {}, {}, null, () => [])
  fiber._runner.epoch = '__INACTIVE__'
  fiber.state = 5
  fiber._disposables = { clear() { trace.push('clear'); return { map(callback) { trace.push('map'); return [callback(() => { trace.push('cleanup') })] } } } }
  await fiber._unload()
  expect(trace).toEqual(['clear', 'map', 'cleanup'])
  const sparse = new Array(3)
  sparse[2] = () => { trace.push('sparse cleanup') }
  fiber._disposables = { clear: () => sparse }
  await fiber._unload()
  expect(trace.at(-1)).toBe('sparse cleanup')
  expect(ctx.logger.buffer).toEqual([])
  const original = ctx.fiber._disposables, failure = new Error('collection failed')
  ctx.fiber._disposables = { clear() { throw failure } }
  await expect(ctx.fiber._unload()).rejects.toBe(failure)
  expect(ctx.logger.buffer).toEqual([])
  ctx.fiber._disposables = original
  await ctx.fiber.dispose()
})

it('preserves browser Fiber effect removal failures and inactive runner admission', async () => {
  const ctx = new Context(), original = ctx.fiber._execute, trace = []
  let runner
  ctx.fiber._execute = function (value) { runner = value; return original.call(this, value) }
  const dispose = ctx.fiber.effect(() => () => { trace.push('cleanup') })
  expect(dispose()).toBeUndefined()
  Object.defineProperty(runner, 'epoch', { value: false, writable: false })
  expect(dispose()).toBeUndefined()
  expect(trace).toEqual(['cleanup'])
  ctx.fiber._execute = original
  const owner = Object.create(ctx.fiber)
  owner._disposables = { push() { return undefined }, delete() {} }
  const broken = owner.effect(() => () => { trace.push('broken cleanup') })
  expect(() => broken()).toThrow(TypeError)
  expect(trace).toEqual(['cleanup', 'broken cleanup'])
  await ctx.fiber.dispose()
})
