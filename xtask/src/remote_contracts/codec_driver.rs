//! Full descriptor and real-Zod codec differential over compiled browser artifacts.

pub(super) const DRIVER: &str = r#"import { createRequire } from 'node:module';
import { readFileSync, writeFileSync, mkdirSync, mkdtempSync, copyFileSync, symlinkSync, rmSync } from 'node:fs';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { resolve, join, dirname } from 'node:path';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const source = resolve(process.argv[2]);
const req = createRequire(source + '/package.json');
req('tsx/cjs');
const { FaceModelEmitter } = req('./packages/typert/generator/src/emitter.ts');
const { Context } = req('./vendor/cordis/lib/index.js');
const { TypertRegistry } = req('./packages/typert/registry/src/service.ts');
const model = JSON.parse(readFileSync('crates/api-remotes-client/contracts/host-model.json', 'utf8'));
const pin = /^commit=(.+)$/m.exec(readFileSync('SOURCE_SNAPSHOT', 'utf8'))[1];
assert.equal(model.sourceCommit, pin);
const normalize = text => text.replaceAll('@deepseek-ai/dsh-', '@seekdeep-ai/seekdeep-').replaceAll('@deepseek-ai/', '@seekdeep-ai/').replaceAll('_deepseek_ai_dsh_', '_seekdeep_ai_seekdeep_').replaceAll('DeepSeek Harness', 'SeekDeep Harness');
const emitter = new FaceModelEmitter(model.face);
const order = ['commands', 'goal', 'cordis-host-runner', 'host-plugin-inventory', 'message-feedback'];
const zod = pathToFileURL(source + '/packages/typert/registry/node_modules/zod/index.js').href;
const expected = [];
for (const name of order) {
  const code = normalize(emitter.emit('@deepseek-ai/dsh-' + name).remote.js).replace("from 'zod'", 'from ' + JSON.stringify(zod));
  expected.push((await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'))).default);
}
let plugin;
globalThis.window = globalThis;
globalThis.__ModuleLoader__ = { load(row) { plugin = row.factory(); } };
vm.runInThisContext(readFileSync('packages/api/remotes/lib/client.js', 'utf8'));
const ctx = new Context();
const registry = new TypertRegistry(ctx);
const actual = [];
const dispose = await plugin.apply({ get() { return { $mount(value) { actual.push(value); return registry.remotes.register(value); } }; } });
const publicContributions = [];
const publicRoot = mkdtempSync(join(dirname(fileURLToPath(import.meta.url)), 'packages-'));
process.on('exit', () => rmSync(publicRoot, { recursive: true, force: true }));
mkdirSync(join(publicRoot, 'node_modules'), { recursive: true });
symlinkSync(resolve('support/browser-dependencies/node_modules/zod'), join(publicRoot, 'node_modules/zod'), process.platform === 'win32' ? 'junction' : 'dir');
writeFileSync(join(publicRoot, 'package.json'), JSON.stringify({ type: 'module', private: true }));
const publicRequire = createRequire(join(publicRoot, 'package.json'));
for (const name of order) {
  const pkg = model.face.packages.find(pkg => pkg.name === '@deepseek-ai/dsh-' + name);
  const destination = join(publicRoot, 'node_modules', normalize(pkg.name));
  mkdirSync(join(destination, 'lib'), { recursive: true });
  copyFileSync(resolve(pkg.root, 'package.json'), join(destination, 'package.json'));
  copyFileSync(resolve(pkg.root, 'lib/typert.remote-client.js'), join(destination, 'lib/typert.remote-client.js'));
  const value = await import(pathToFileURL(publicRequire.resolve(normalize(pkg.name) + '/remote')).href);
  assert.deepEqual(Object.keys(value).sort(), ['TYPERT_REMOTE', 'default']);
  assert.equal(value.default, value.TYPERT_REMOTE);
  publicContributions.push(value.default);
}

function snapshot(value, schemas = new Map()) {
  if (value === undefined) return { $undefined: true };
  if (typeof value === 'number' && !Number.isFinite(value)) return { $number: String(value) };
  if (typeof value === 'bigint') return { $bigint: String(value) };
  if (Object.is(value, -0)) return { $negativeZero: true };
  if (value?._zod?.def) {
    if (schemas.has(value)) return { $schemaRef: schemas.get(value) };
    const id = schemas.size; schemas.set(value, id);
    const def = {};
    for (const [key, field] of Object.entries(value._zod.def)) {
      def[key] = value._zod.def.type === 'lazy' && key === 'getter' ? snapshot(field(), schemas) : snapshot(field, schemas);
    }
    return { $schemaId: id, def, meta: snapshot(value.meta?.(), schemas) };
  }
  if (Array.isArray(value)) return Array.from(value, item => snapshot(item, schemas));
  if (value !== null && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, snapshot(item, schemas)]));
  if (typeof value === 'function') throw new Error('unclassified function in generated schema definition');
  return value;
}

function sample(schema, depth = 0) {
  if (depth > 30) throw new Error('non-finite sample');
  const d = schema._zod.def;
  switch (d.type) {
    case 'string': return 'fixture';
    case 'number': return 1;
    case 'boolean': return true;
    case 'null': return null;
    case 'undefined': case 'void': return undefined;
    case 'literal': return d.values[0];
    case 'unknown': case 'any': return { fixture: true };
    case 'optional': return undefined;
    case 'nullable': return null;
    case 'readonly': return sample(d.innerType, depth + 1);
    case 'lazy': return sample(d.getter(), depth + 1);
    case 'array': return depth > 4 ? [] : [sample(d.element, depth + 1)];
    case 'record': return {};
    case 'object': return Object.fromEntries(Object.entries(d.shape).map(([key, value]) => [key, sample(value, depth + 1)]));
    case 'tuple': return d.items.map(value => sample(value, depth + 1));
    case 'union': {
      for (const option of d.options) { try { const value = sample(option, depth + 1); if (schema.safeParse(value).success) return value; } catch {} }
      throw new Error('union has no generated sample');
    }
    case 'intersection': {
      const left = sample(d.left, depth + 1), right = sample(d.right, depth + 1);
      for (const value of [left, right, { ...left, ...right }]) if (schema.safeParse(value).success) return value;
      throw new Error('intersection has no generated sample');
    }
    case 'never': throw new Error('uninhabited schema');
    default: throw new Error('unsupported schema sample: ' + d.type);
  }
}

function outcome(schema, input) {
  try { const value = schema.parse(input); return { ok: true, value: snapshot(value), same: value === input, frozen: value !== null && typeof value === 'object' && Object.isFrozen(value) }; }
  catch (error) { return { ok: false, name: error.name, message: error.message, issues: snapshot(error.issues) }; }
}

assert.deepEqual(snapshot(actual), snapshot(expected), 'complete descriptor/schema definitions differ');
assert.deepEqual(snapshot(publicContributions), snapshot(expected), 'public package descriptor/schema definitions differ');
let boundaries = 0, comparisons = 0;
for (let packageIndex = 0; packageIndex < expected.length; packageIndex++) {
  for (let index = 0; index < expected[packageIndex].descriptors.length; index++) {
    const left = expected[packageIndex].descriptors[index], right = actual[packageIndex].descriptors[index], published = publicContributions[packageIndex].descriptors[index];
    const sourceCodecs = [...left.parameters.map(p => p.codec), left.result];
    const targetCodecs = [...right.parameters.map(p => p.codec), right.result];
    const publicCodecs = [...published.parameters.map(p => p.codec), published.result];
    if (left.invocation.kind === 'context') { sourceCodecs.push(left.invocation.codec); targetCodecs.push(right.invocation.codec); publicCodecs.push(published.invocation.codec); }
    for (let boundary = 0; boundary < sourceCodecs.length; boundary++) {
      const sourceSchema = sourceCodecs[boundary].schema, targetSchema = targetCodecs[boundary].schema;
      const inputs = [undefined, null, false, true, 0, -1, 1, NaN, Infinity, '', 'fixture', [], {}, { unexpected: true }];
      let valid;
      try { valid = sample(sourceSchema); inputs.push(valid); }
      catch (error) { if (sourceSchema._zod.def.type !== 'never') throw error; }
      if (valid && typeof valid === 'object' && !Array.isArray(valid)) {
        inputs.push({ ...valid, unexpected: 'strip' });
        for (const key of Object.keys(valid)) { const missing = { ...valid }; delete missing[key]; inputs.push(missing); }
      }
      for (const input of inputs) {
        const expected = outcome(sourceSchema, structuredClone(input));
        assert.deepEqual(outcome(targetSchema, structuredClone(input)), expected, `${left.id} boundary ${boundary}`);
        assert.deepEqual(outcome(publicCodecs[boundary].schema, structuredClone(input)), expected, `${left.id} public boundary ${boundary}`);
        comparisons += 2;
      }
      boundaries++;
    }
  }
}
await dispose();
assert.equal(registry.remotes.list().length, 0);
const publicDisposers = publicContributions.map(contribution => registry.remotes.register(contribution));
assert.equal(registry.remotes.list().length, 24);
for (const dispose of publicDisposers.reverse()) await dispose();
assert.equal(registry.remotes.list().length, 0);
console.log(JSON.stringify({ modules: actual.length, publicModules: publicContributions.length, descriptors: actual.flatMap(value => value.descriptors).length, boundaries, comparisons, sourceAccepted: true, remaining: 0 }));
"#;
