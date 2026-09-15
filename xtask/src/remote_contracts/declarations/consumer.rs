//! Isolated, strict consumer programs over real generated package boundaries.

pub(super) const SCRIPT: &str = r#"
import { createRequire } from 'node:module';
import { cpSync, readFileSync, realpathSync, writeFileSync, mkdirSync, mkdtempSync, statSync, symlinkSync } from 'node:fs';
import { join, dirname } from 'node:path';
import assert from 'node:assert/strict';
const [root, output] = process.argv.slice(2);
const dependencies = join(root, 'support/browser-dependencies');
const require = createRequire(join(dependencies, 'package.json'));
const ts = require('typescript');
const model = JSON.parse(readFileSync(join(root, 'crates/api-remotes-client/contracts/client-declarations.json'), 'utf8'));
assert.equal(model.sourceCommit, /^commit=(.+)$/m.exec(readFileSync(join(root, 'SOURCE_SNAPSHOT'), 'utf8'))[1]);
assert.equal(ts.version, model.compilerVersion);
const directory = mkdtempSync(join(output, 'case-'));
const normalize = value => value.replaceAll('subagent-dsh-sdk', 'subagent-seekdeep-sdk').replaceAll('@deepseek-ai/dsh-', '@seekdeep-ai/seekdeep-').replaceAll('@deepseek-ai/', '@seekdeep-ai/');
function link(name, path) {
  const target = join(directory, 'node_modules', name);
  mkdirSync(dirname(target), { recursive: true });
  symlinkSync(path, target, process.platform === 'win32' ? 'junction' : 'dir');
}
for (const pkg of model.packages) {
  const path = join(root, normalize(pkg.root));
  const manifest = JSON.parse(readFileSync(join(path, 'package.json'), 'utf8'));
  assert.equal(manifest.name, normalize(pkg.name));
  const target = join(directory, 'node_modules', manifest.name);
  mkdirSync(target, { recursive: true });
  writeFileSync(join(target, 'package.json'), readFileSync(join(path, 'package.json')));
  // Published declarations resolve peers in the consumer, without workspace node_modules.
  cpSync(join(path, 'lib'), join(target, 'lib'), {
    recursive: true,
    filter: path => statSync(path).isDirectory() || /\.d\.[cm]?ts(?:\.map)?$/.test(path),
  });
}
for (const name of model.external) link(name, join(dependencies, 'node_modules', name));
writeFileSync(join(directory, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
const options = { strict: true, noUncheckedIndexedAccess: true, exactOptionalPropertyTypes: true,
  skipLibCheck: false, types: [], lib: ['lib.es2024.d.ts', 'lib.dom.d.ts', 'lib.dom.iterable.d.ts', 'lib.esnext.disposable.d.ts'],
  module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024, moduleResolution: ts.ModuleResolutionKind.Bundler,
  preserveSymlinks: true, noEmit: true, allowImportingTsExtensions: true };
function diagnostics(name, source, compilerOptions = options) {
  const path = join(directory, name + '.ts'); writeFileSync(path, source);
  const compilerHost = ts.createCompilerHost(compilerOptions);
  compilerHost.getCurrentDirectory = () => directory;
  const program = ts.createProgram([path], compilerOptions, compilerHost);
  const errors = ts.getPreEmitDiagnostics(program);
  for (const file of program.getSourceFiles()) {
    if (file.fileName.includes('/node_modules/@seekdeep-ai/')) assert(file.isDeclarationFile, 'consumer imported implementation source: ' + file.fileName);
  }
  return { program, path, errors };
}
function formatted(errors) {
  return ts.formatDiagnosticsWithColorAndContext(errors, { getCanonicalFileName: path => path, getCurrentDirectory: () => directory, getNewLine: () => '\n' });
}
const positive = `
import type { Context } from '@seekdeep-ai/cordis';
import type { ClientRemote, SessionId, MessageId, ApiRemoteForwardedEvent } from '@seekdeep-ai/seekdeep-api-remotes/client';
import type { RemoteResult, TypertRemoteScopeApi, TypertRemoteEvent } from '@seekdeep-ai/seekdeep-typert-protocol';
import type { CreateGoalRequest, CreateGoalResult } from '@seekdeep-ai/seekdeep-goal/client';
import type {} from '@seekdeep-ai/seekdeep-typert-registry/client';
type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;
type Assert<T extends true> = T;
type IsAny<T> = 0 extends 1 & T ? true : false;
export type Keys = Assert<Equal<keyof ClientRemote, '$mount' | '$on' | '$dispatch' | 'commands' | 'goals' | 'dynamicCordisRunner' | 'pluginInventory' | 'messageFeedback'>>;
export type Arguments = Assert<Equal<Parameters<ClientRemote['goals']['create']>, [SessionId, CreateGoalRequest]>>;
export type Result = Assert<Equal<ReturnType<ClientRemote['goals']['create']>, Promise<RemoteResult<CreateGoalResult>>>>;
export type ScopedArguments = Assert<Equal<Parameters<TypertRemoteScopeApi<'agent'>['goals']['create']>, [CreateGoalRequest]>>;
export type Cancellation = Assert<Equal<Parameters<ClientRemote['commands']['execute']>, [SessionId, string, (AbortSignal | undefined)?]>>;
export type CanonicalId = Assert<Equal<SessionId, import('@seekdeep-ai/seekdeep-session/types').SessionId>>;
export type DistinctIds = Assert<Equal<Equal<SessionId, MessageId>, false>>;
export type StrictId = Assert<Equal<IsAny<SessionId>, false>>;
export type StrictResult = Assert<Equal<IsAny<Awaited<ReturnType<ClientRemote['goals']['create']>>>, false>>;
export type NoHostServices = Assert<Equal<Extract<keyof Context, 'agents' | 'sessions' | 'goals'>, never>>;
export type Forwarding = Assert<Equal<Extract<'cordis/request-run', TypertRemoteEvent>, 'cordis/request-run'>>;
export type SelectedForwarding = Assert<Equal<Extract<'cordis/request-run', ApiRemoteForwardedEvent>, 'cordis/request-run'>>;
export async function typedRemote(ctx: Context, sessionId: SessionId) {
  const created = await ctx.remote.goals.create(sessionId, { objective: 'typed consumer goal' });
  if (!created.ok) throw new Error(created.error.message);
  const edited = await ctx.remote.goals.edit(sessionId, created.value.ref, { objective: 'typed consumer edited goal' });
  if (!edited.ok) throw new Error(edited.error.message);
  const commands = await ctx.remote.commands.list(sessionId);
  if (!commands.ok) throw new Error(commands.error.message);
  return { ref: created.value.ref, revision: edited.value.revision, commands: commands.value.length };
}
`;
const good = diagnostics('positive', positive);
assert.equal(good.errors.length, 0, formatted(good.errors));
const prefix = `import type { Context } from '@seekdeep-ai/cordis';
import type { SessionId, MessageId } from '@seekdeep-ai/seekdeep-api-remotes/client';
import type { TypertRemoteScopeApi } from '@seekdeep-ai/seekdeep-typert-protocol';
declare const ctx: Context; declare const id: SessionId; declare const messageId: MessageId;
declare const scoped: TypertRemoteScopeApi<'agent'>;\n`;
const cases = [
  ['wrong-id', 'ctx.remote.goals.create(messageId, { objective: "bad" });', 2345],
  ['wrong-payload', 'ctx.remote.goals.create(id, { objective: 1 });', 2322],
  ['missing-identity', 'ctx.remote.goals.create({ objective: "bad" });', 2554],
  ['unexpected-signal', 'ctx.remote.goals.create(id, { objective: "bad" }, new AbortController().signal);', 2554],
  ['wrong-command', 'ctx.remote.commands.execute(id, 4);', 2345],
  ['private-host-service', 'ctx.goals;', 2339],
  ['unmarked-method', 'ctx.remote.goals.delete(id);', 2339],
  ['result-without-narrowing', 'const result = await ctx.remote.goals.create(id, { objective: "bad" }); result.value;', 2339],
  ['readonly-result', 'const result = await ctx.remote.goals.create(id, { objective: "bad" }); if (result.ok) result.value.ref.revision = 4;', 2540],
  ['scoped-extra-identity', 'scoped.goals.create(id, { objective: "bad" });', 2554],
  ['unselected-event', 'ctx.remote.$on("fixture/not-forwarded", () => {});', 2345],
  ['wrong-event-payload', 'ctx.remote.$on("cordis/request-run", (event: number) => {});', 2345],
];
for (const [name, code, expected] of cases) {
  const { errors } = diagnostics(name, prefix + code);
  assert.deepEqual(errors.map(error => error.code), [expected], name + '\n' + formatted(errors));
}
const absent = diagnostics('without-assembly', `import type { TypertClientRemote } from '@seekdeep-ai/seekdeep-typert-protocol'; declare const remote: TypertClientRemote; remote.goals;`);
assert.deepEqual(absent.errors.map(error => error.code), [2339], formatted(absent.errors));
const nodeTypes = realpathSync(join(root, 'node_modules/@types/node'));
link('@types/node', nodeTypes);
const nodeRequire = createRequire(join(nodeTypes, 'package.json'));
link('undici-types', dirname(nodeRequire.resolve('undici-types/package.json')));
const hostOptions = { ...options, types: ['node'] };
const hostPrefix = `
import type { Context } from '@seekdeep-ai/cordis';
import type { Agent, CreateAgentOptions } from '@seekdeep-ai/seekdeep-agent';
import { Session, type SessionStore, type SessionId } from '@seekdeep-ai/seekdeep-session';
import type { MessageId } from '@seekdeep-ai/seekdeep-llm';
import type { DeepSeekHarnessOptions } from '@seekdeep-ai/seekdeep-sdk-client';
import type { ClientModuleRegistry, WebBootGraph } from '@seekdeep-ai/seekdeep-client-modules';
import type { BashEnvContributor } from '@seekdeep-ai/seekdeep-shell-env';
import type { ShellExecRequest } from '@seekdeep-ai/seekdeep-shell';
import type { SeekdeepEnvironment, SeekdeepEnvironmentKey } from '@seekdeep-ai/seekdeep-subprocess';
import { Binary } from '@seekdeep-ai/cosmokit';
declare const id: SessionId; declare const messageId: MessageId; declare const ctx: Context;
`;
const hostPositive = hostPrefix + `
type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;
type Assert<T extends true> = T;
type IsAny<T> = 0 extends 1 & T ? true : false;
export type SessionIdentity = Assert<Equal<Session['id'], SessionId>>;
export type AgentOwner = Assert<Equal<Agent['session'], Session>>;
export type FactoryIdentity = Assert<Equal<CreateAgentOptions['sessionId'], SessionId>>;
export type SessionService = Assert<Equal<Context['sessions'], SessionStore>>;
export type HostModuleRegistry = Assert<Equal<Context['clientModules'], ClientModuleRegistry>>;
export type HostModuleGraph = Assert<Equal<ReturnType<ClientModuleRegistry['graph']>, WebBootGraph>>;
export type NoClientService = Assert<Equal<Extract<keyof Context, 'remote'>, never>>;
export type BinaryResult = Assert<Equal<ReturnType<typeof Binary.toBase64>, string>>;
export type StrictBinaryResult = Assert<Equal<IsAny<ReturnType<typeof Binary.toBase64>>, false>>;
export type OutputLimit = Assert<Equal<NonNullable<DeepSeekHarnessOptions['maxTokens']>, number>>;
export type EnvironmentPrefix = Assert<Equal<Extract<'SEEKDEEP_FIXTURE', keyof BashEnvContributor['variables']>, 'SEEKDEEP_FIXTURE'>>;
export type LegacyEnvironmentPrefix = Assert<Equal<Extract<'DSH_FIXTURE', keyof BashEnvContributor['variables']>, never>>;
export type ManagedEnvironmentKey = Assert<Equal<Extract<'SEEKDEEP_FIXTURE', SeekdeepEnvironmentKey>, 'SEEKDEEP_FIXTURE'>>;
export type ShellEnvironment = Assert<Equal<ShellExecRequest['seekdeepEnv'], SeekdeepEnvironment | undefined>>;
export function createSession(id: SessionId) { return Session.create(id); }
`;
const hostGood = diagnostics('host-positive', hostPositive, hostOptions);
assert.equal(hostGood.errors.length, 0, formatted(hostGood.errors));
const hostCases = [
  ['host-raw-identity', 'Session.create("raw-session");', 2345],
  ['host-wrong-identity', 'Session.create(messageId);', 2345],
  ['host-factory-identity', 'const options: Pick<CreateAgentOptions, "sessionId"> = { sessionId: messageId };', 2322],
  ['host-binary-result', 'const value: number = Binary.toBase64(new Uint8Array());', 2322],
  ['host-output-limit', 'const options: Pick<DeepSeekHarnessOptions, "maxTokens"> = { maxTokens: "bad" };', 2322],
  ['host-client-service', 'ctx.remote;', 2339],
  ['host-managed-environment-key', 'const key: SeekdeepEnvironmentKey = "DSH_FIXTURE";', 2322],
];
for (const [name, code, expected] of hostCases) {
  const { errors } = diagnostics(name, hostPrefix + code, hostOptions);
  assert.deepEqual(errors.map(error => error.code), [expected], name + '\n' + formatted(errors));
}
const compiled = ts.transpileModule(positive, { compilerOptions: { ...options, noEmit: false, allowImportingTsExtensions: false }, fileName: 'positive.ts' });
const executable = join(output, 'typed-consumer.mjs'); writeFileSync(executable, compiled.outputText);
console.log(JSON.stringify({ declarations: model.modules.length, packages: model.packages.length,
  client: { positive: 1, negative: cases.length + 1 }, host: { positive: 1, negative: hostCases.length },
  compiler: ts.version, executable }));
"#;
