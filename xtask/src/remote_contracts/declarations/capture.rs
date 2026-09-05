//! Read-only extraction of the oracle's foreign-language declaration boundary.

pub(super) const SCRIPT: &str = r"
import { createRequire } from 'node:module';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
const source = resolve(process.argv[1]);
const modelPath = resolve(process.argv[2]);
const require = createRequire(join(source, 'package.json'));
require('tsx/cjs');
const ts = require('typescript');
const { FaceModelEmitter } = require('./packages/typert/generator/src/emitter.ts');
const { face } = JSON.parse(readFileSync(modelPath, 'utf8'));
const configPath = join(source, 'tsconfig.base.client.json');
const config = ts.readConfigFile(configPath, ts.sys.readFile);
if (config.error) throw new Error(ts.flattenDiagnosticMessageText(config.error.messageText, '\n'));
const parsed = ts.parseJsonConfigFileContent(config.config, ts.sys, source, undefined, configPath);
const options = { ...parsed.options, composite: false, incremental: false, noEmit: false,
  emitDeclarationOnly: true, declaration: true, declarationMap: false, outDir: join(source, '__declaration_capture__'), rootDir: source };
const packageRoots = [];
for (const group of readdirSync(join(source, 'packages'), { withFileTypes: true }).filter(entry => entry.isDirectory())) {
  for (const pkg of readdirSync(join(source, 'packages', group.name), { withFileTypes: true }).filter(entry => entry.isDirectory())) packageRoots.push(join(source, 'packages', group.name, pkg.name));
}
for (const pkg of readdirSync(join(source, 'vendor'), { withFileTypes: true }).filter(entry => entry.isDirectory())) packageRoots.push(join(source, 'vendor', pkg.name));
for (const packageRoot of packageRoots) {
  const manifestPath = join(packageRoot, 'package.json');
  if (!ts.sys.fileExists(manifestPath)) continue;
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  for (const [subpath, entry] of Object.entries(manifest.exports ?? {})) {
    const type = entry?.types;
    if (!subpath.startsWith('.') || typeof type !== 'string' || !type.startsWith('./lib/types/')) continue;
    const candidate = join(packageRoot, type.replace('./lib/types/', 'src/').replace(/\.d\.ts$/, '.ts'));
    if (ts.sys.fileExists(candidate)) options.paths[manifest.name + (subpath === '.' ? '' : subpath.slice(1))] = [candidate];
  }
}
const virtual = new Map();
const emitter = new FaceModelEmitter(face);
for (const pkg of face.packages) {
  const path = resolve(source, pkg.root, 'lib/typert.remote-client.d.ts');
  virtual.set(path, emitter.emit(pkg.name).remote.dts);
  options.paths[pkg.name + '/remote'] = [path];
}
const host = ts.createCompilerHost(options);
const read = host.readFile, exists = host.fileExists, directory = host.directoryExists;
host.readFile = path => virtual.get(resolve(path)) ?? read(path);
host.fileExists = path => virtual.has(resolve(path)) || exists(path);
host.directoryExists = path => [...virtual.keys()].some(file => dirname(file) === resolve(path)) || directory(path);
const roots = ['packages/api/remotes/src/client/index.ts', 'packages/api/gateway/src/client/index.ts', 'packages/typert/registry/src/client/index.ts'].map(path => resolve(source, path));
const program = ts.createProgram(roots, options, host);
const records = new Map();
const emitted = program.emit(undefined, (path, text, bom, onError, files) => {
  if (!path.endsWith('.d.ts') || files?.length !== 1) throw new Error('unexpected declaration emission ' + path);
  records.set(resolve(files[0].fileName), text);
}, undefined, true);
if (emitted.emitSkipped || emitted.diagnostics.length) throw new Error(ts.formatDiagnosticsWithColorAndContext(emitted.diagnostics, {
  getCanonicalFileName: path => path, getCurrentDirectory: () => source, getNewLine: () => '\n',
}));
for (const [path, text] of virtual) records.set(path, text);
const pending = [...roots];
const selected = new Map();
const external = new Set();
while (pending.length) {
  const path = pending.shift();
  if (selected.has(path)) continue;
  const text = records.get(path);
  if (text === undefined) throw new Error('missing declaration for ' + path);
  selected.set(path, text);
  for (const item of ts.preProcessFile(text, true, true).importedFiles) {
    const found = ts.resolveModuleName(item.fileName, path, options, host).resolvedModule;
    if (found && records.has(resolve(found.resolvedFileName))) pending.push(resolve(found.resolvedFileName));
    else if (item.fileName.startsWith('.') || item.fileName.startsWith('@deepseek-ai/')) throw new Error('unresolved public declaration edge ' + path + ' -> ' + item.fileName + ': ' + found?.resolvedFileName);
    else external.add(item.fileName);
  }
}
const packages = new Map();
const modules = [];
for (const [absolute, content] of [...selected].sort(([a], [b]) => a.localeCompare(b))) {
  const path = relative(source, absolute).replaceAll('\\', '/');
  const components = path.split('/');
  const length = components[0] === 'packages' ? 3 : components[0] === 'vendor' ? 2 : 0;
  if (!length) throw new Error('declaration outside package boundary ' + path);
  const packageRoot = components.slice(0, length).join('/');
  if (!packages.has(packageRoot)) {
    const manifest = JSON.parse(readFileSync(join(source, packageRoot, 'package.json'), 'utf8'));
    packages.set(packageRoot, { root: packageRoot, name: manifest.name, exports: manifest.exports });
  }
  if (virtual.has(absolute)) continue;
  if (components[length] !== 'src') throw new Error('unexpected declaration source location ' + path);
  const output = components.slice(length + 1).join('/').replace(/\.tsx?$/, '.d.ts');
  modules.push({ source: path, packageRoot, output: packageRoot + '/lib/types/' + output, content });
}
process.stdout.write(JSON.stringify({ formatVersion: 1, compilerVersion: ts.version,
  roots: roots.map(path => relative(source, path).replaceAll('\\', '/')),
  modules, packages: [...packages.values()].sort((a, b) => a.root.localeCompare(b.root)), external: [...external].sort() }));
";
