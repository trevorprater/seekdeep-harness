it('preserves browser logger constructor descriptors and mutable severity factories', () => {
  const methods = [], service = { _snMessage: 0, exporters: new Map() }
  class CustomLogger extends Logger {
    _method(type, level) { methods.push([this.name, type, level]); return () => level }
  }
  const logger = new CustomLogger({ name: 'custom', extra: 7 }, service)
  expect(Object.keys(logger)).toEqual(['service', 'name', 'extra', 'error', 'info', 'warn', 'debug'])
  expect(methods).toEqual([['custom', 'error', 0], ['custom', 'info', 1], ['custom', 'warn', 2], ['custom', 'debug', 3]])
  expect([logger.error(), logger.info(), logger.warn(), logger.debug()]).toEqual([0, 1, 2, 3])
  expect(Reflect.ownKeys(Logger)).toEqual(['length', 'name', 'prototype', 'color', 'code', 'format'])
  expect(Object.getOwnPropertyNames(Logger.prototype)).toEqual(['constructor', '_method'])
  for (const [name, length] of [['color', 3], ['code', 2], ['format', 2]]) {
    const descriptor = Object.getOwnPropertyDescriptor(Logger, name)
    expect([descriptor.value.name, descriptor.value.length, descriptor.writable, descriptor.enumerable, descriptor.configurable]).toEqual([name, length, true, false, true])
  }
  expect(Object.keys(defaultFormatters)).toEqual(['s', 'd', 'i', 'f', 'o', 'O', 'c', 'C'])
  for (const [name, formatter] of Object.entries(defaultFormatters)) {
    expect(formatter.name).toBe(name)
    expect(formatter.length).toBe(name === 'C' ? 3 : name === 'c' ? 0 : 1)
    expect(Object.hasOwn(formatter, 'prototype')).toBe(false)
  }
})

it('preserves browser logger coercion hints, missing arguments, and trailing object overrides', () => {
  const hints = [], value = { [Symbol.toPrimitive](hint) { hints.push(hint); return 2.9 } }
  expect(Logger.format({}, { name: 'test', args: ['%s|%d|%i|%f', value, value, value, value] })).toBe('2.9|2|2|2.9')
  expect(hints).toEqual(['string', 'number', 'number', 'number'])
  expect(Logger.format({}, { name: 'test', args: ['%s|%d|%i|%f|%o|%O|%c|%C'] })).toBe('undefined|NaN|NaN|NaN|undefined|undefined||undefined')
  const calls = [], exporter = { formatters: { o(value, receiver, message) { calls.push([this, value, receiver, message]); return '<object>' } } }
  const message = { name: 'test', args: ['trailing', {}, null, [], 'end'] }
  expect(Logger.format(exporter, message)).toBe('trailing <object> null <object> end')
  expect(calls.map(call => [call[0], call[2], call[3]])).toEqual([[undefined, exporter, message], [undefined, exporter, message]])
  expect(Logger.format({ formatters: { '1': () => 'bad', 'é': () => 'bad', s: false } }, { args: ['%1 %é %s', 'tail'] })).toBe('%1 %é %s tail')
  expect(Logger.format({ maxLength: -1 }, { args: ['abc\r\nx'] })).toBe('ab...\n...')
  expect(Logger.format({}, { args: ['last\r'] })).toBe('last\r')
})

it('preserves browser logger error cause reads, aggregate traversal, and bound severity receivers', () => {
  const messages = [], service = { _snMessage: 0, exporters: new Map([[1, { levels: { default: 3 }, export(message) { messages.push(message) } }]]) }
  const logger = new Logger({ name: 'errors' }, service), first = new Error('first'), second = new Error('second'), outer = new Error('outer')
  let reads = 0
  Object.defineProperty(outer, 'cause', { get() { return ++reads === 1 ? first : second } })
  const detached = logger.error
  detached.call({ service: null }, outer)
  expect(reads).toBe(2)
  expect(messages.map(message => message.args[0])).toEqual([second, outer])
  messages.length = 0
  const aggregate = new Error('aggregate'), errors = [first, , second]
  aggregate.errors = errors
  logger.error(aggregate)
  expect(messages.map(message => message.args[0])).toEqual([first, second])
  messages.length = 0
  aggregate.cause = false
  logger.error(aggregate, 'extra')
  expect(messages.map(message => message.args)).toEqual([[aggregate, 'extra']])
})

it('preserves browser logger exporter iteration while registrations change', () => {
  const calls = [], service = { _snMessage: 0, exporters: new Map() }
  service.exporters.set(1, { export(message) {
    calls.push(['first', message.sn])
    service.exporters.delete(2)
    service.exporters.set(3, { export(message) { calls.push(['late', message.sn]) } })
  } })
  service.exporters.set(2, { export() { calls.push(['removed']) } })
  new Logger({ name: 'dynamic' }, service).info('message')
  expect(calls).toEqual([['first', 1], ['late', 1]])
})

it('preserves browser logger name hashing after JavaScript char-code coercion', () => {
  const name = { length: 1, charCodeAt() { return '65' } }
  expect(Logger.code(name, 1)).toBe(c16[6513 % c16.length])
  name.charCodeAt = () => Infinity
  expect(Logger.code(name, 1)).toBe(c16[0])
  name.charCodeAt = () => 1n
  expect(() => Logger.code(name, 1)).toThrow(TypeError)
  const saved = [...c16]
  try {
    c16.splice(0, c16.length, 99)
    expect(Logger.code('mutable', 1)).toBe(99)
    c16.length = 0
    expect(Logger.code('empty', 1)).toBeUndefined()
  } finally { c16.splice(0, c16.length, ...saved) }
})

it('preserves browser logger record fields as own data properties', () => {
  const seen = [], messages = [], prototype = Object.prototype
  const previous = Object.getOwnPropertyDescriptor(prototype, 'args')
  try {
    Object.defineProperty(prototype, 'args', { configurable: true, set(value) { seen.push(value) } })
    const service = { _snMessage: 0, exporters: new Map([[1, { export(message) { messages.push(message) } }]]) }
    new Logger({ name: 'literal' }, service).info('value')
  } finally {
    if (previous) Object.defineProperty(prototype, 'args', previous)
    else delete prototype.args
  }
  expect(seen).toEqual([])
  expect(Object.getOwnPropertyDescriptor(messages[0], 'args')).toEqual({ value: ['value'], writable: true, enumerable: true, configurable: true })
})

it('preserves browser Service utility tracing of ordinary functions with overridden apply', () => {
  const receiver = {}, target = function(value) { return [this, value] }
  target[symbols.tracker] = { noShadow: true }
  target.apply = () => 'incorrect apply'
  const traced = getTraceable({}, target)
  expect(Reflect.apply(traced, receiver, [7])).toEqual([receiver, 7])
  expect(traced[symbols.original]).toBe(target)
})

it('preserves browser Service utility canonical tracker lookup without legacy reads', () => {
  const legacy = Symbol.for('cordis.service.tracker'), target = { ctx: {}, [legacy]: true }, caller = {}
  expect(getTraceable(caller, target)).toBe(target)
  Object.defineProperty(target, legacy, { get() { throw new Error('legacy marker was read') } })
  expect(getTraceable(caller, target)).toBe(target)
  target[symbols.tracker] = { property: 'ctx' }
  expect(getTraceable(caller, target).ctx).toBe(caller)
})

it('preserves browser Service utility callable strict function shape and descriptor inheritance', () => {
  const ctx = {}, prototype = { ctx, [symbols.invoke]() { return this.ctx } }
  const callable = createCallable('shape', prototype, { property: 'ctx' })
  expect(Reflect.ownKeys(callable)).toEqual(['length', 'name', 'prototype'])
  expect(Object.getPrototypeOf(callable)).toBe(prototype)
  expect(Object.getOwnPropertyDescriptor(callable, 'name')).toEqual({ value: 'shape', writable: true, enumerable: false, configurable: true })
  expect(callable.prototype.constructor).toBe(callable)
  expect(callable()).toBe(ctx)
  const failure = new Error('prototype getter failed')
  const broken = new Proxy({}, { getPrototypeOf() { throw failure } })
  expect(() => joinPrototype(broken, {})).toThrow(failure)
  const symbol = Symbol('descriptor'), base = {}, first = Object.create(Object.prototype)
  Object.defineProperty(first, symbol, { get() { return this.marker }, enumerable: false, configurable: false })
  const joined = joinPrototype(first, base)
  expect(Object.getPrototypeOf(joined)).toBe(base)
  expect(Object.getOwnPropertyDescriptor(joined, symbol)).toEqual(Object.getOwnPropertyDescriptor(first, symbol))
})

it('preserves browser Service utility constructor checks for primitive and structural values', () => {
  for (const value of [1, true, 'text', Symbol('value'), 1n]) expect(isConstructor(value)).toBe(false)
  for (const value of [null, undefined]) expect(() => isConstructor(value)).toThrow(TypeError)
  expect(isConstructor({ prototype: {} })).toBe(true)
  expect(isConstructor({ prototype: 0 })).toBe(false)
  const failure = new Error('prototype access')
  expect(() => isConstructor({ get prototype() { throw failure } })).toThrow(failure)
})

it('preserves browser Service utility DisposableList initialization and invalid-key ordering', () => {
  let setters = 0, list
  class Derived extends DisposableList {}
  Object.defineProperty(Derived.prototype, 'sn', { set() { setters++ }, configurable: true })
  list = new Derived()
  expect(setters).toBe(0)
  expect(Object.keys(list)).toEqual(['sn', 'map', 'weak'])
  expect(list.sn).toBe(0)
  expect(() => list.push(7)).toThrow(TypeError)
  expect(list.length).toBe(1)
  expect([...list]).toEqual([7])
  expect(list.delete(7)).toBe(false)
  expect(list.clear()).toEqual([7])
  const value = {}, cleanup = list.push(value), iterator = list[Symbol.iterator]()
  expect(iterator.next()).toEqual({ value, done: false })
  const later = {}
  list.push(later)
  expect(iterator.next()).toEqual({ value: later, done: false })
  expect(cleanup()).toBe(true)
  expect(cleanup()).toBe(false)
  expect(list.clear()).toEqual([later])
})

it('preserves browser Service utility property overlay receivers and symbol writes', () => {
  const symbol = Symbol('field'), target = { constructor: 'original', value: 1 }, receivers = []
  const props = { constructor: 'ignored', get value() { return this.marker }, set value(value) { receivers.push([this, value]) }, [symbol]: 2 }
  const proxy = withProps(target, props), receiver = { marker: 9 }
  expect(Reflect.get(proxy, 'value', receiver)).toBe(9)
  expect(Reflect.set(proxy, 'value', 3, receiver)).toBe(true)
  expect(receivers).toEqual([[receiver, 3]])
  expect(proxy.constructor).toBe('original')
  expect(Reflect.set(proxy, symbol, 4)).toBe(true)
  expect(target[symbol]).toBe(4)
  expect(props[symbol]).toBe(2)
  expect(Reflect.ownKeys(proxy)).toEqual(['constructor', 'value', symbol])
})

it('preserves browser stacks utility thenable failures and malformed thrown reasons', () => {
  const outer = () => ['    at owner()'], failure = new Error('then getter')
  failure.stack = 'Error: then getter\n    at independent()'
  expect(() => composeError(() => ({ get then() { throw failure } }), outer)).toThrow(failure)
  let error
  try { composeError(() => { throw { message: 'value', stack: 7, toString() { return 'stringified' } } }, outer) }
  catch (caught) { error = caught }
  expect(error).toBeInstanceOf(Error)
  expect(error.stack).toBe('Error: stringified\n    at owner()')
  const reason = new Error('kept')
  reason.stack = 'Error: kept\n    at marker()\n    at removed()'
  const supplied = { *[Symbol.iterator]() { yield '    at first()'; yield '    at second()' } }
  try { composeError(info => { info.error = { stack: 'Error\nunused\n    at marker()' }; info.offset = 0; throw reason }, () => supplied) }
  catch (caught) { expect(caught).toBe(reason) }
  expect(reason.stack).toBe('Error: kept\n    at first()\n    at second()')
})
