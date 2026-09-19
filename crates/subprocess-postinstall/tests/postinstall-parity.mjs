import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, mkdirSync, readFileSync, realpathSync, symlinkSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { spawnSync } from 'node:child_process';

const [target, source, temporaryInput] = process.argv.slice(2);
const temporary = realpathSync(temporaryInput);
const scenarios = [
  'absent', 'prebuild', 'release', 'both', 'other-platform', 'directory',
  'symlink', 'broken-symlink', 'first-error', 'second-error', 'missing-package',
];
const probe = [
  "import fs from 'node:fs';",
  "import { syncBuiltinESMExports } from 'node:module';",
  "import { relative } from 'node:path';",
  "import { pathToFileURL } from 'node:url';",
  "const [root, entry, scenario, filesText] = process.argv.slice(2);",
  "const files = JSON.parse(filesText);",
  "const calls = [];",
  "const originalExists = fs.existsSync;",
  "const originalChmod = fs.chmodSync;",
  "const marker = Object.assign(new Error('controlled chmod failure'), { code: 'EACCES' });",
  "const normalize = path => relative(root, path).replaceAll('\\\\', '/');",
  "fs.existsSync = path => { calls.push(['exists', normalize(path)]); return originalExists(path); };",
  "fs.chmodSync = (path, mode) => {",
  "  calls.push(['chmod', normalize(path), mode]);",
  "  if ((scenario === 'first-error' && normalize(path).includes('/prebuilds/')) ||",
  "      (scenario === 'second-error' && normalize(path).includes('/build/'))) throw marker;",
  "  originalChmod(path, mode);",
  "};",
  "syncBuiltinESMExports();",
  "let outcome;",
  "try { outcome = { exports: Object.keys(await import(pathToFileURL(entry).href)) }; }",
  "catch (error) { outcome = { name: error.name, code: error.code, marker: error === marker,",
  "  missingPackage: error.message.includes(\"Cannot find package 'node-pty'\") }; }",
  "const modes = files.map(path => {",
  "  try { const stat = fs.statSync(path); return [normalize(path), stat.mode & 0o777, stat.isDirectory()]; }",
  "  catch (error) { return [normalize(path), error.code]; }",
  "});",
  "console.log(JSON.stringify({ outcome, calls, modes }));",
].join('\n');
let comparisons = 0;
for (const scenario of scenarios) {
  const results = [];
  for (const [variant, script] of [['source', source], ['rust', target]]) {
    const root = join(temporary, scenario, variant, 'space and 中文');
    const packageRoot = join(root, 'node_modules/node-pty');
    const prebuild = join(packageRoot, 'prebuilds', process.platform + '-' + process.arch, 'spawn-helper');
    const release = join(packageRoot, 'build/Release/spawn-helper');
    const other = join(packageRoot, 'prebuilds/unrelated-platform/spawn-helper');
    const referent = join(root, 'link-target');
    const absent = join(root, 'absent-target');
    const files = [prebuild, release, other, referent, absent];
    const put = path => {
      mkdirSync(dirname(path), { recursive: true });
      writeFileSync(path, '#!/bin/sh\nexit 0\n');
      chmodSync(path, 0o600);
    };
    mkdirSync(root, { recursive: true });
    if (scenario !== 'missing-package') {
      mkdirSync(join(packageRoot, 'lib'), { recursive: true });
      writeFileSync(join(packageRoot, 'package.json'), JSON.stringify({ name: 'node-pty', main: 'lib/index.js' }));
      writeFileSync(join(packageRoot, 'lib/index.js'), 'module.exports = {};\n');
    }
    if (['prebuild', 'both', 'first-error', 'second-error'].includes(scenario)) put(prebuild);
    if (['release', 'both', 'first-error', 'second-error'].includes(scenario)) put(release);
    if (scenario === 'other-platform') put(other);
    if (scenario === 'directory') { mkdirSync(prebuild, { recursive: true }); chmodSync(prebuild, 0o700); }
    if (scenario === 'symlink' || scenario === 'broken-symlink') {
      mkdirSync(dirname(prebuild), { recursive: true });
      if (scenario === 'symlink') put(referent);
      symlinkSync(scenario === 'symlink' ? referent : absent, prebuild, 'file');
    }
    const entry = join(root, 'install.mjs');
    copyFileSync(script, entry);
    const probePath = join(root, 'probe.mjs');
    writeFileSync(probePath, probe);
    const environment = { ...process.env, PATH: join(root, 'no-executables') };
    delete environment.NODE_PATH;
    const output = spawnSync(process.execPath, [probePath, root, entry, scenario, JSON.stringify(files)], {
      cwd: root, env: environment, encoding: 'utf8',
    });
    assert.equal(output.status, 0, scenario + '/' + variant + '\n' + output.stdout + output.stderr);
    results.push(JSON.parse(output.stdout));
  }
  assert.deepEqual(results[1], results[0], scenario);
  if (scenario.endsWith('-error')) assert.equal(results[1].outcome.marker, true, scenario);
  if (scenario === 'absent') assert.deepEqual(results[1].outcome.exports, []);
  comparisons++;
}
assert(readFileSync(target, 'utf8').includes("Buffer.from('"), 'the published entry must contain its Rust module');
console.log(JSON.stringify({ scenarios: comparisons, originalErrorsRetained: true, nodeOnlyBootstrap: true }));
