it('preserves browser reflection single membership lookup through nested proxies', async () => {
  const ctx = new Context(), handler = ctx.reflect.constructor.handler, calls = []
  const target = new Proxy({ present: 1 }, { has(value, key) { calls.push(key); return Reflect.has(value, key) } })
  expect(handler.has(target, 'present')).toBe(true)
  expect(calls).toEqual(['present'])
  calls.length = 0
  expect(handler.has(target, '_private')).toBe(false)
  expect(calls).toEqual(['_private'])
  await ctx.fiber.dispose()
})

it('preserves browser reflection raw setter trap results before Proxy coercion', async () => {
  const ctx = new Context(), handler = ctx.reflect.constructor.handler, sentinel = {}
  const receiver = { [symbols.receiver]: sentinel }
  for (const result of [sentinel, 7, '', null, undefined, false, true]) {
    const target = { reflect: { props: { result: { type: 'accessor', set(value, explicit, error) {
      expect(this).toBe(receiver)
      expect(value).toBe(9)
      expect(explicit).toBe(sentinel)
      expect(error.message).toBe('cannot set property "result" without provide')
      return result
    } } } } }
    expect(handler.set(target, 'result', 9, receiver)).toBe(result)
  }
  const target = { reflect: { props: { service: { type: 'service' } } } }
  const scoped = { events: { waterfall() { return sentinel } } }
  expect(handler.set(target, 'service', 9, scoped)).toBe(sentinel)
  await ctx.fiber.dispose()
})

it('preserves browser reflection bind laziness and mutable trace dispatch', async () => {
  const ctx = new Context(), prototype = ctx.reflect.constructor.prototype, calls = [], owner = {}
  Object.defineProperty(owner, 'ctx', { get() { throw new Error('bind must defer context reads') } })
  owner.trace = function (value) { expect(this).toBe(owner); calls.push(value); return { value } }
  const receiver = {}, first = {}, second = {}
  const bound = prototype.bind.call(owner, function (...args) { return [this, ...args] })
  const output = bound.call(receiver, first, second)
  expect(output.map(item => item.value)).toEqual([receiver, first, second])
  expect(calls).toEqual([receiver, first, second])
  owner.trace = value => value
  expect(bound.call(receiver, first)).toEqual([receiver, first])
  function Record(value) { this.value = value; this.target = new.target }
  const Constructed = prototype.bind.call(owner, Record)
  class Derived extends Constructed {}
  const value = new Derived(first)
  expect(value.value).toBe(first)
  expect(value.target).toBe(Derived)
  const failure = new Error('trace rejected')
  owner.trace = () => { throw failure }
  expect(() => bound()).toThrow(failure)
  expect(() => new Constructed(first)).toThrow(failure)
  await ctx.fiber.dispose()
})

it('preserves browser reflection get context evaluation before implementation lookup', async () => {
  const ctx = new Context(), prototype = ctx.reflect.constructor.prototype, calls = []
  const owner = {
    get ctx() { calls.push('ctx'); return {} },
    _getImpl(name, strict) { calls.push(['lookup', name, strict]); return { value: 7 } },
  }
  expect(prototype.get.call(owner, 'ordered', false)).toBe(7)
  expect(calls).toEqual(['ctx', ['lookup', 'ordered', false]])
  await ctx.fiber.dispose()
})

it('preserves browser reflection current contexts during set and default notifications', async () => {
  const ctx = new Context(), prototype = ctx.reflect.constructor.prototype, label = Symbol('dynamic-scope'), provider = {}, calls = []
  const first = { [symbols.isolate]: { service: label }, fiber: {} }, second = { [symbols.isolate]: { service: label }, fiber: provider }
  let reads = 0
  const implementation = { fiber: provider, value: 1 }
  const setter = { get ctx() { reads++; return reads === 1 ? first : second }, store: { [label]: implementation } }
  expect(prototype.set.call(setter, 'service', 7)).toBe(true)
  expect(implementation.value).toBe(7)
  expect(reads).toBe(2)
  const changed = { [symbols.isolate]: { service: label }, events: { emit(receiver, event, name, value) { calls.push([Object.getPrototypeOf(receiver) === changed, event, name, value]) } } }
  const fiber = { ctx: changed, inject: { service: true }, _checkImpl(name) { calls.push(['check', name]) }, _refresh() { calls.push(['refresh']) } }
  const owner = Object.assign(Object.create(prototype), { store: { [label]: { fiber: { state: 2 }, value: 9 } } })
  owner.ctx = { [symbols.isolate]: { service: Symbol('initial') }, registry: { values() { owner.ctx = changed; return [{ fibers: [fiber] }] } }, events: { emit() { throw new Error('stale notification context') } } }
  expect(owner.notify(['service'])).toEqual([fiber])
  expect(calls).toEqual([['check', 'service'], ['refresh'], [true, 'internal/service', 'service', 9]])
  await ctx.fiber.dispose()
})

it('preserves browser reflection accessor object spread data properties', async () => {
  const ctx = new Context(), marker = Symbol('accessor metadata'), inherited = { inherited: true }, calls = []
  const options = Object.create(inherited)
  Object.defineProperties(options, {
    get: { enumerable: true, value() { return 42 } },
    hidden: { value: 3 },
    observed: { enumerable: true, get() { calls.push('observed'); return 7 } },
  })
  Object.defineProperty(options, '__proto__', { value: inherited, enumerable: true, configurable: true })
  options[marker] = 11
  const remove = ctx.accessor('spread-property', options)
  const definition = ctx.reflect.props['spread-property']
  expect(Object.getPrototypeOf(definition)).toBe(Object.prototype)
  expect(Object.getOwnPropertyDescriptor(definition, '__proto__')).toEqual({ value: inherited, enumerable: true, configurable: true, writable: true })
  expect(Object.getOwnPropertyDescriptor(definition, 'observed')).toEqual({ value: 7, enumerable: true, configurable: true, writable: true })
  expect(definition[marker]).toBe(11)
  expect('hidden' in definition).toBe(false)
  expect('inherited' in definition).toBe(false)
  expect(calls).toEqual(['observed'])
  expect(ctx['spread-property']).toBe(42)
  await remove(); await ctx.fiber.dispose()
})

it('preserves browser reflection provider declaration replacement before duplicate refusal', async () => {
  const ctx = new Context(), name = 'quoted"name\nline'
  const remove = ctx.provide(name, 7)
  const definition = ctx.reflect.props[name]
  definition.marker = true
  expect(() => ctx.provide(name, 8)).toThrow(`service "${name}" has been registered at <root>`)
  expect(ctx.reflect.props[name]).not.toBe(definition)
  expect(ctx.reflect.props[name]).toEqual({ type: 'service' })
  await remove()
  const computed = ctx.accessor('other"name\nline', { get() { return 9 } })
  expect(() => ctx.provide('other"name\nline', 10)).toThrow('property "other"name\nline" is already declared as accessor')
  await computed(); await ctx.fiber.dispose()
})

it('preserves browser reflection property-key conversion for explicit providers', async () => {
  const ctx = new Context(), remove = ctx.provide(5, 'number-name')
  expect(ctx.get(5)).toBe('number-name')
  expect(ctx.get('5')).toBe('number-name')
  expect(ctx.reflect._getImpl(5).name).toBe(5)
  ctx.set(5, 'replacement')
  expect(ctx.get(5)).toBe('replacement')
  expect(() => ctx.provide('5', 'duplicate')).toThrow('service "5" has been registered at <root>')
  const calls = [], consumer = ctx.inject(['5'], child => {
    calls.push(child.get('5'))
    return () => calls.push('cleanup')
  })
  await consumer.await()
  expect(calls).toEqual(['replacement'])
  await remove()
  await consumer.await()
  expect(calls).toEqual(['replacement', 'cleanup'])
  expect(ctx.get(5)).toBeUndefined()
  await ctx.fiber.dispose()
})

it('preserves browser reflection metadata-owned scopes and structural Fiber providers', async () => {
  const ctx = new Context(), outer = ctx.provide('manual-scope', 1)
  const labels = Object.create(ctx[symbols.isolate])
  labels['manual-scope'] = Symbol('manual-scope')
  const child = ctx.extend({ [symbols.isolate]: labels })
  const inner = child.provide('manual-scope', 2)
  expect(ctx.get('manual-scope')).toBe(1)
  expect(child.get('manual-scope')).toBe(2)
  await inner()
  expect(ctx.get('manual-scope')).toBe(1)
  expect(child.get('manual-scope')).toBeUndefined()
  const fiber = { state: 2, name: 'structural-owner', store: Object.create(null), effect(setup) { return setup() } }
  const custom = ctx.extend({ fiber }), remove = custom.provide('structural-owner-service', 7)
  expect(custom.reflect._getImpl('structural-owner-service').fiber).toBe(fiber)
  expect(fiber.store['structural-owner-service'].value).toBe(7)
  expect(ctx.get('structural-owner-service')).toBe(7)
  await remove()
  expect(fiber.store['structural-owner-service']).toBeUndefined()
  await outer(); await ctx.fiber.dispose()
})

it('preserves browser reflection mixin array mapping and sparse-entry failures', async () => {
  const ctx = new Context(), calls = [], source = { value: 8 }
  ctx.provide('map-source', source)
  const members = ['value']
  members.map = function (map) { calls.push(this === members); expect(map('value')).toEqual(['value', 'value']); return [['value', 'mapped-value']] }
  const remove = ctx.mixin('map-source', members)
  expect(calls).toEqual([true])
  expect(ctx['mapped-value']).toBe(8)
  expect('value' in ctx).toBe(false)
  await remove()
  const sparse = ['value', , 'value']
  expect(() => ctx.mixin('map-source', sparse)).toThrow(TypeError)
  expect('value' in ctx).toBe(false)
  await ctx.fiber.dispose()
})

it('preserves browser reflection mixin generator laziness and abrupt iterator closure', async () => {
  const ctx = new Context(), prototype = ctx.reflect.constructor.prototype, calls = [], failure = new Error('accessor refused')
  const members = []
  members.map = function () {
    calls.push('mapped')
    return (function* () {
      try { yield ['first', 'first']; yield ['second', 'second'] }
      finally { calls.push('closed') }
    })()
  }
  const owner = {
    ctx: { fiber: { effect(setup) { expect(setup.constructor.name).toBe('GeneratorFunction'); return setup() } } },
    accessor(name) { calls.push(name); if (name === 'second') throw failure; return () => {} },
  }
  const iterator = prototype.mixin.call(owner, 'source', members)
  expect(calls).toEqual([])
  expect(iterator.next().done).toBe(false)
  expect(calls).toEqual(['mapped', 'first'])
  expect(() => iterator.next()).toThrow(failure)
  expect(calls).toEqual(['mapped', 'first', 'second', 'closed'])
  expect(iterator.next()).toEqual({ done: true, value: undefined })
  calls.length = 0
  const cancelled = prototype.mixin.call(owner, 'source', members)
  cancelled.next()
  expect(cancelled.return(7)).toEqual({ done: true, value: 7 })
  expect(calls).toEqual(['mapped', 'first', 'closed'])
  await ctx.fiber.dispose()
})

it('preserves browser Service constructor and extension descriptor failures', () => {
  const publications = [], ctx = { reflect: { provide(...args) { publications.push(args) } } }
  class Named extends Service { static provide = 'fallback'; [Service.invoke](value) { return value } }
  for (const name of [undefined, null, '', 0, false]) {
    const value = new Named(ctx, name)
    expect(value.name).toBe(name ?? 'fallback')
    expect(publications.at(-1)[0]).toBe(name ?? 'fallback')
    expect(value(9)).toBe(9)
    expect(Object.getOwnPropertyDescriptor(value, symbols.tracker)).toEqual({ value: { associate: name ?? 'fallback', property: 'ctx' }, writable: true, enumerable: false, configurable: false })
  }
  const value = new Named(ctx), failure = new Error('extension getter'), props = { get value() { throw failure } }
  expect(() => value[Service.extend](props)).toThrow(failure)
  expect(value.name).toBe('fallback')
})

it('preserves browser reflection disposed admission before declarations and conflicts', async () => {
  const ctx = new Context(), fiber = ctx.plugin(() => {}), events = []
  await fiber.await()
  await fiber.dispose()
  ctx.on('internal/service', name => events.push(name))
  expect(() => fiber.ctx.provide('never-published', 7)).toThrow('cannot create effect on inactive context')
  expect('never-published' in ctx).toBe(false)
  expect(events).toEqual([])
  ctx.accessor('taken', { get() { return 8 } })
  expect(() => fiber.ctx.provide('taken', 9)).toThrow('cannot create effect on inactive context')
  expect(() => fiber.ctx.accessor('taken', { get() { return 10 } })).toThrow('cannot create effect on inactive context')
  await ctx.fiber.dispose()
})

it('preserves browser reflection mixins in the reading and writing context isolation', async () => {
  const ctx = new Context(), scope = ctx.isolate('counter'), outer = { value: 1 }, inner = { value: 2 }
  ctx.provide('counter', outer)
  scope.provide('counter', inner)
  const remove = ctx.mixin('counter', { value: 'counter-value' })
  expect(ctx['counter-value']).toBe(1)
  expect(scope['counter-value']).toBe(2)
  scope['counter-value'] = 9
  expect(outer.value).toBe(1)
  expect(inner.value).toBe(9)
  await remove(); await ctx.fiber.dispose()
})

it('preserves browser Service falsy base and head omission with retained intercept entries', async () => {
  const ctx = new Context(), scope = ctx.intercept('service', null).intercept('service', false).intercept('service', { value: 7 })
  const service = { ctx: scope, name: 'service', Config: { merge(...values) { return values } } }
  for (const value of [null, false, 0, -0, '', undefined, NaN]) {
    expect(Service.prototype[Service.resolveConfig].call(service, value, value)).toEqual([null, false, { value: 7 }])
  }
  expect(Service.prototype[Service.resolveConfig].call(service, [], {})).toEqual([[], null, false, { value: 7 }, {}])
  await ctx.fiber.dispose()
})
