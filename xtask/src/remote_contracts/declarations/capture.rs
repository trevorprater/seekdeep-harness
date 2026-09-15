//! Read-only extraction of the oracle's foreign-language declaration boundary.

pub(super) const SCRIPT: &str = r"
import { createRequire, isBuiltin } from 'node:module';
import { readFileSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
const source = resolve(process.argv[1]);
const modelPath = resolve(process.argv[2]);
const require = createRequire(join(source, 'package.json'));
require('tsx/cjs');
const ts = require('typescript');
const { FaceModelEmitter } = require('./packages/typert/generator/src/emitter.ts');
const { face } = JSON.parse(readFileSync(modelPath, 'utf8'));
const virtual = new Map();
const outputs = new Map();
const directories = new Set();
const diagnostics = [];
const capturedAt = new Date();
function virtualWrite(path, text) {
  path = resolve(path);
  const fromRoot = relative(source, path);
  if (!fromRoot || fromRoot === '..' || fromRoot.startsWith('..' + sep) || isAbsolute(fromRoot)) {
    throw new Error('declaration output outside source ' + path);
  }
  virtual.set(path, text);
  for (let current = dirname(path); current.startsWith(source); current = dirname(current)) {
    directories.add(current);
    if (current === source) break;
  }
}
const emitter = new FaceModelEmitter(face);
for (const pkg of face.packages) {
  virtualWrite(join(source, pkg.root, 'lib/typert.remote-client.d.ts'), emitter.emit(pkg.name).remote.dts);
}
const system = {
  ...ts.sys,
  getCurrentDirectory: () => source,
  readFile: path => virtual.get(resolve(path)) ?? ts.sys.readFile(path),
  fileExists: path => virtual.has(resolve(path)) || ts.sys.fileExists(path),
  directoryExists: path => directories.has(resolve(path)) || ts.sys.directoryExists(path),
  writeFile(path, text) { virtualWrite(path, text); outputs.set(resolve(path), text); },
  createDirectory: path => directories.add(resolve(path)),
  deleteFile: path => { throw new Error('unexpected declaration capture deletion ' + path); },
  setModifiedTime: () => {},
  getModifiedTime: path => virtual.has(resolve(path)) ? capturedAt : ts.sys.getModifiedTime(path),
};
const report = diagnostic => diagnostics.push(diagnostic);
const host = ts.createSolutionBuilderHost(system, undefined, report, report);
// Host and Client Cordis augmentations belong to separate project programs.
const roots = [join(source, 'tsconfig.json')];
const builder = ts.createSolutionBuilder(host, roots, {
  force: true, declaration: true, emitDeclarationOnly: true, declarationMap: false,
});
const status = builder.build();
if (status !== 0 || diagnostics.length) throw new Error(ts.formatDiagnosticsWithColorAndContext(diagnostics, {
  getCanonicalFileName: path => path, getCurrentDirectory: () => source, getNewLine: () => '\n',
}) || 'declaration build failed with status ' + status);
const packages = new Map();
const modules = [];
const external = new Set();
for (const [absolute, content] of [...outputs].sort(([a], [b]) => a.localeCompare(b))) {
  if (!absolute.endsWith('.d.ts')) continue;
  const output = relative(source, absolute).replaceAll('\\', '/');
  const components = output.split('/');
  const length = components[0] === 'packages' ? 3 : components[0] === 'vendor' ? 2
    : output.startsWith('native/landlock-run/packages/entry/') ? 4 : 0;
  if (!length) continue;
  const packageRoot = components.slice(0, length).join('/');
  const prefix = packageRoot + (components[0] === 'native' ? '/lib/' : '/lib/types/');
  if (!output.startsWith(prefix)) throw new Error('unexpected public declaration output ' + output);
  const stem = packageRoot + '/src/' + output.slice(prefix.length).replace(/\.d\.ts$/, '');
  const origins = [stem + '.ts', stem + '.tsx'].filter(path => ts.sys.fileExists(join(source, path)));
  if (origins.length !== 1) throw new Error('ambiguous declaration source for ' + output);
  if (!packages.has(packageRoot)) {
    const manifest = JSON.parse(readFileSync(join(source, packageRoot, 'package.json'), 'utf8'));
    packages.set(packageRoot, { root: packageRoot, name: manifest.name, exports: manifest.exports });
  }
  modules.push({ source: origins[0], packageRoot, output, content });
  for (const item of ts.preProcessFile(content, true, true).importedFiles) {
    const name = item.fileName;
    if (name.startsWith('.') || name.startsWith('@deepseek-ai/') || isBuiltin(name)) continue;
    external.add(name.split('/').slice(0, name.startsWith('@') ? 2 : 1).join('/'));
  }
}
if (!modules.length) throw new Error('declaration capture emitted no public modules');
process.stdout.write(JSON.stringify({ formatVersion: 1, compilerVersion: ts.version,
  roots: roots.map(path => relative(source, path).replaceAll('\\', '/')),
  modules, packages: [...packages.values()].sort((a, b) => a.root.localeCompare(b.root)), external: [...external].sort() }));
";
