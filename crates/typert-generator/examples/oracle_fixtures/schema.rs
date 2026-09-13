//! Source schema-case extraction; the real source suite owns its assertions.

pub(super) const SCRIPT: &str = r"
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const { createRequire } = require('node:module');

const sourceRoot = resolve(process.argv[1]);
const sourceRequire = createRequire(resolve(sourceRoot, 'package.json'));
sourceRequire('tsx/cjs');
const ts = sourceRequire('typescript');
const { FaceModelEmitter } = sourceRequire('./packages/typert/generator/src/emitter.ts');
const path = resolve(sourceRoot, 'packages/typert/generator/tests/schema-emitter.spec.ts');
const text = readFileSync(path, 'utf8');
const file = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true);
const wanted = new Set(['location', 'documentation', 'supportedCases', 'unsupportedNodeCases']);
const selected = file.statements.filter(statement =>
  ts.isFunctionDeclaration(statement) && statement.name?.text !== 'loadSchema'
  || ts.isVariableStatement(statement) && statement.declarationList.declarations.some(declaration =>
    ts.isIdentifier(declaration.name) && wanted.has(declaration.name.text)))
  .map(statement => statement.getText(file)).join('\n');

let captured;
class Capture extends FaceModelEmitter {
  constructor(face) {
    super(face);
    captured = face;
  }
}
const helpers = new Function('FaceModelEmitter', ts.transpileModule(
  `${selected}\nreturn {supportedCases,unsupportedNodeCases,emit};`,
  { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } },
).outputText)(Capture);

function encode(value) {
  if (value === undefined) return { $undefined: true };
  if (typeof value === 'bigint') return { $bigint: String(value) };
  if (typeof value === 'symbol') return { $symbol: value.description };
  if (typeof value === 'function') return { $function: true };
  if (value instanceof Date) return { $date: value.toISOString() };
  if (Array.isArray(value)) return value.map(encode);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, encode(item)]));
  return value;
}

const inputs = [
  ...helpers.supportedCases,
  ...helpers.unsupportedNodeCases.map(input => ({ ...input, name: input.kind, accepted: [], rejected: [] })),
];
const cases = inputs.map(input => {
  let outcome;
  try {
    outcome = { ok: helpers.emit(input.nodes) };
  } catch (error) {
    outcome = { error: { name: error.name, message: error.message } };
  }
  return { name: input.name, face: captured, outcome, accepted: input.accepted.map(encode), rejected: input.rejected.map(encode) };
});
process.stdout.write(`${JSON.stringify({ cases }, (key, value) => typeof value === 'bigint' ? { $bigint: String(value) } : value, 2)}\n`);
";
