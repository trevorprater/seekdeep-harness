//! JavaScript driver generated into `target/` by the built Remote smoke.

pub(crate) const DRIVER: &str = r"import { pathToFileURL } from 'node:url'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { spawn } from 'node:child_process'
import { once } from 'node:events'

const root = process.cwd()
let origin = process.argv[2]
let server
let temporaryHome
if (!origin) {
  temporaryHome = await mkdtemp(join(tmpdir(), 'seekdeep-built-remote-'))
  server = spawn(join(root, 'target/debug/seekdeep'), ['web', '--host', '127.0.0.1', '--port', '0'], {
    cwd: root,
    env: { ...process.env, SEEKDEEP_HOME: temporaryHome },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  server.stdout.setEncoding('utf8')
  server.stderr.setEncoding('utf8')
  let stdout = ''
  let stderr = ''
  server.stderr.on('data', chunk => { stderr += chunk })
  origin = await new Promise((resolveOrigin, rejectOrigin) => {
    let settled = false
    server.stdout.on('data', chunk => {
      stdout += chunk
      const match = /seekdeep web: (http:\/\/[^\s]+)/.exec(stdout)
      if (match && !settled) {
        settled = true
        resolveOrigin(match[1])
      }
    })
    server.once('exit', code => {
      if (!settled) rejectOrigin(new Error(`seekdeep web exited ${String(code)} before announcing a URL\n${stderr}`))
    })
  })
}

try {
const artifact = path => pathToFileURL(join(root, path)).href
const cordisWasm = await import(`${artifact('vendor/cordis/lib/client.js')}?built-remote-smoke`)
await cordisWasm.default({
  module_or_path: await readFile(join(root, 'vendor/cordis/lib/client_bg.wasm')),
})
const tracker = Symbol.for('cordis.service.tracker')
const trace = (ctx, value) => {
  if ((typeof value !== 'object' && typeof value !== 'function') || value === null || value[tracker] !== true) return value
  let proxy
  proxy = new Proxy(value, {
    get(target, key, receiver) {
      if (key === 'ctx') return ctx
      const inner = Reflect.get(target, key, receiver)
      return typeof inner === 'function' ? (...args) => Reflect.apply(inner, proxy, args) : inner
    },
  })
  return proxy
}
const wrapContext = core => {
  let context
  context = new Proxy(core, {
    get(target, key, receiver) {
      if (key === 'emit') return (name, ...args) => target.emitArgs(name, args)
      if (key === 'parallel') return (name, ...args) => target.parallelArgs(name, args)
      if (key === 'serial') return (name, ...args) => target.serialArgs(name, args)
      if (key === 'bail') return (name, ...args) => target.bailArgs(name, args)
      if (key === 'get') return name => trace(context, target.get(name))
      if (Reflect.has(target, key)) {
        const value = Reflect.get(target, key, receiver)
        return typeof value === 'function' ? value.bind(target) : value
      }
      const metadata = target.metaGet(key)
      if (metadata !== undefined) return metadata
      return typeof key === 'string' ? trace(context, target.get(key)) : undefined
    },
  })
  return context
}
cordisWasm.configureContextWrapper(wrapContext)
class Context {
  constructor() { return cordisWasm.createContext() }
}
const cordis = { ...cordisWasm, Context }
const handoffs = new Map()
globalThis.window = globalThis
globalThis.__ModuleLoader__ = {
  load(handoff) { handoffs.set(handoff.id, handoff) },
}
Object.defineProperty(globalThis, 'location', {
  configurable: true,
  value: { hostname: '127.0.0.1', origin, search: '' },
})

for (const path of [
  'packages/typert/registry/lib/client.js',
  'packages/client/connection/lib/client.js',
  'packages/api/gateway/lib/client.js',
  'packages/api/remotes/lib/client.js',
]) {
  await import(`${artifact(path)}?built-remote-smoke`)
}

const instantiate = id => {
  const handoff = handoffs.get(id)
  if (handoff === undefined) throw new Error(`missing Client bundle handoff ${id}`)
  return handoff.factory(specifier => {
    if (specifier === '@seekdeep-ai/cordis') return cordis
    throw new Error(`unexpected Client external ${specifier}`)
  })
}

let sequence = 0
const hostFetch = globalThis.fetch.bind(globalThis)
const callHost = async (method, payload) => {
  sequence += 1
  const rpcId = `setup-${String(sequence)}`
  const response = await hostFetch(`${origin}/api/${method}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ type: 'client-request', rpcId, method, payload }),
  })
  if (!response.ok) throw new Error(`setup ${method} returned HTTP ${String(response.status)}`)
  const envelope = await response.json()
  if (envelope.rpcId !== rpcId) throw new Error(`setup ${method} returned the wrong rpcId`)
  if (!envelope.result?.ok) throw new Error(`setup ${method} failed: ${JSON.stringify(envelope.result?.error)}`)
  return envelope.result.value
}

const rootSession = await callHost('session.create', { cwd: root })
const scopedSession = await callHost('session.create', { cwd: root })

const client = new cordis.Context()
for (const id of [
  '@seekdeep-ai/seekdeep-typert-registry',
  '@seekdeep-ai/seekdeep-client-connection',
  '@seekdeep-ai/seekdeep-api-gateway',
  '@seekdeep-ai/seekdeep-api-remotes',
]) {
  const plugin = instantiate(id)
  await client.plugin({ name: id, inject: plugin.inject, apply: plugin.apply })
}
const disposeBinder = client.typert.contexts.registerClient('agent', {
  identity: context => context.builtAgentId,
})

let remoteCalls = 0
const nativeFetch = globalThis.fetch
globalThis.fetch = async (...args) => {
  remoteCalls += 1
  return nativeFetch(...args)
}

let invalidRejected = false
try {
  await client.remote.goals.create(rootSession.sessionId, { objective: 1 })
} catch {
  invalidRejected = true
}
if (remoteCalls !== 0) throw new Error('invalid generated input reached the transport')

const rootResult = await client.remote.goals.create(
  rootSession.sessionId,
  { objective: 'root goal' },
)
if (!rootResult.ok) throw new Error(`root Goal creation failed: ${JSON.stringify(rootResult.error)}`)
const rootEdit = await client.remote.goals.edit(
  rootSession.sessionId,
  rootResult.value.ref,
  { objective: 'edited root goal' },
)
if (!rootEdit.ok) throw new Error(`root Goal edit failed: ${JSON.stringify(rootEdit.error)}`)
const scoped = client.extend({ builtAgentId: scopedSession.sessionId })
const scopedResult = await scoped.remote.goals.create({
  objective: 'scoped goal',
  maxGoalRounds: 3,
})
if (!scopedResult.ok) throw new Error(`scoped Goal creation failed: ${JSON.stringify(scopedResult.error)}`)
const rootHistory = await callHost('session.history', { sessionId: rootSession.sessionId })
const scopedHistory = await callHost('session.history', { sessionId: scopedSession.sessionId })
const rootGoalEvents = rootHistory.events.filter(entry => entry.event.type === 'goal/change').length
const scopedGoalEvents = scopedHistory.events.filter(entry => entry.event.type === 'goal/change').length
if (remoteCalls !== 3) throw new Error(`expected three generated Remote calls, got ${String(remoteCalls)}`)
if (rootResult.value.ref.revision !== 1 || !rootResult.value.ref.id.startsWith('goal-')) {
  throw new Error(`invalid root Goal result: ${JSON.stringify(rootResult.value)}`)
}
if (rootEdit.value.objective !== 'edited root goal' || rootEdit.value.revision !== 2) {
  throw new Error(`invalid root Goal edit: ${JSON.stringify(rootEdit.value)}`)
}
if (scopedResult.value.ref.revision !== 1 || !scopedResult.value.ref.id.startsWith('goal-')) {
  throw new Error(`invalid scoped Goal result: ${JSON.stringify(scopedResult.value)}`)
}
if (rootGoalEvents !== 2 || scopedGoalEvents !== 1) {
  throw new Error(`unexpected durable Goal event counts: root=${String(rootGoalEvents)} scoped=${String(scopedGoalEvents)}`)
}

console.log(JSON.stringify({
  invalidRejected,
  remoteCalls,
  rootSessionId: rootSession.sessionId,
  scopedSessionId: scopedSession.sessionId,
  rootResult: rootResult.value,
  rootEdit: rootEdit.value,
  scopedResult: scopedResult.value,
  rootGoalEvents,
  scopedGoalEvents,
}))

disposeBinder()
await client.fiber.dispose()
} finally {
  if (server) {
    if (server.exitCode === null) {
      server.kill('SIGINT')
      await once(server, 'exit')
    }
  }
  if (temporaryHome) await rm(temporaryHome, { recursive: true, force: true })
}
";
