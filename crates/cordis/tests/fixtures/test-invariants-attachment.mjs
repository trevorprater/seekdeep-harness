import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import './wasm-file-fetch.mjs';

const runtime = await import(new URL('../../../../vendor/cordis/lib/index.js', import.meta.url));
const { Context, RegistryService, Service, ValidationError, TEST_INVARIANT_READY_SERVICE,
  createTestInvariantAttachmentStore, installTestInvariantHost } = runtime;

assert.equal(TEST_INVARIANT_READY_SERVICE, 'testInvariantReady');
class AttachmentStore extends Service {
  constructor(ctx) { super(ctx, 'attachments'); }
}
const TestStore = createTestInvariantAttachmentStore(AttachmentStore);
const sourceRoot = process.env.SEEKDEEP_SOURCE_ORACLE_ROOT ?? '/Users/trevor/ws/deepseek-harness';
const requireSource = createRequire(`${sourceRoot}/package.json`);
const ts = requireSource('typescript');
const sourceFile = `${sourceRoot}/scripts/test-invariants.ts`;
const source = ts.createSourceFile(sourceFile, readFileSync(sourceFile, 'utf8'), ts.ScriptTarget.Latest, true);
const declaration = source.statements.find(node => ts.isClassDeclaration(node) && node.name?.text === 'TestAttachmentStore');
const sourceClass = ts.transpileModule(declaration.getText(source), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
const SourceStore = Function('AttachmentStore', sourceClass + '; return TestAttachmentStore;')(AttachmentStore);
assert.equal(TestStore.name, SourceStore.name);
assert.equal(TestStore.length, SourceStore.length);
assert.deepEqual(Object.getOwnPropertyNames(TestStore.prototype), Object.getOwnPropertyNames(SourceStore.prototype));
const ctx = new Context();
const fiber = ctx.plugin(TestStore);
await fiber;
assert.deepEqual(ctx.attachments.imageLimits, {
  maxImageBytes: 1,
  maxImagesPerMessage: 1,
  maxMessageImageBytes: 1,
  maxImagePixels: 1,
  mediaTypes: ['image/png'],
});
for (const [method, verb] of [['validateImage', 'validate'], ['saveImage', 'save'], ['readImage', 'read']]) {
  const descriptor = Object.getOwnPropertyDescriptor(TestStore.prototype, method);
  assert.equal(descriptor.writable, true);
  assert.equal(descriptor.configurable, true);
  assert.equal(descriptor.enumerable, false);
  const original = Object.getOwnPropertyDescriptor(SourceStore.prototype, method).value;
  assert.equal(descriptor.value.name, original.name);
  assert.equal(descriptor.value.length, original.length);
  assert.equal(Object.hasOwn(descriptor.value, 'prototype'), Object.hasOwn(original, 'prototype'));
  await assert.rejects(ctx.attachments[method]({}), {
    name: 'Error', message: `test invariant attachment store does not ${verb} images`,
  });
}
await fiber.dispose();

const original = RegistryService.prototype.plugin;
const unexpected = () => { throw new Error('manual tree mounted the host'); };
const uninstall = installTestInvariantHost(RegistryService.prototype, unexpected, TestStore, ValidationError,
  () => '/repo/packages/core/tools/tests/invariant.spec.ts', {});
assert.notEqual(RegistryService.prototype.plugin, original);
let calls = 0;
const manual = new Context();
const manualFiber = manual.plugin(() => { calls += 1; });
await manualFiber;
assert.equal(calls, 1);
assert.deepEqual(Object.keys(manualFiber.inject), []);
await manualFiber.dispose();
uninstall();
assert.equal(RegistryService.prototype.plugin, original);
uninstall();
assert.equal(RegistryService.prototype.plugin, original);
const declarations = fileURLToPath(new URL('../../../../vendor/cordis/lib/types/index.d.ts', import.meta.url));
const fixture = fileURLToPath(new URL('./test-invariants-types.mts', import.meta.url));
const program = ts.createProgram([fixture], {
  noEmit: true, skipLibCheck: true, target: ts.ScriptTarget.ES2022,
  module: ts.ModuleKind.NodeNext, moduleResolution: ts.ModuleResolutionKind.NodeNext,
  types: [], paths: { '@seekdeep-ai/cordis': [declarations] },
});
assert.deepEqual(ts.getPreEmitDiagnostics(program).map(diagnostic => ts.flattenDiagnosticMessageText(diagnostic.messageText, '\n')), []);
process.stdout.write(JSON.stringify({ attachmentLimitFields: 5, rejectingMethods: 3, methodDescriptors: 3, sourceCallableMetadata: true, publicDeclarations: true, manualBypass: true, reversibleInstall: true }) + '\n');
