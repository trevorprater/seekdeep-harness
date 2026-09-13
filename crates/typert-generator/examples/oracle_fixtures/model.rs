//! Read-only invocation of the pinned compiler's model and backend.

pub(super) const SCRIPT: &str = r"
const { resolve } = require('node:path');
const { createRequire } = require('node:module');

const sourceRoot = resolve(process.argv[1]);
const sourceRequire = createRequire(resolve(sourceRoot, 'package.json'));
sourceRequire('tsx/cjs');
const { WorkspaceAnalyzer } = sourceRequire('./packages/typert/generator/src/analyzer.ts');
const { FaceModelEmitter } = sourceRequire('./packages/typert/generator/src/emitter.ts');
const { TypeGraphRenderer } = sourceRequire('./packages/typert/generator/src/renderer.ts');
const { childTypeNodeIds } = sourceRequire('./packages/typert/generator/src/model.ts');

function analyze(name, faces) {
  return new WorkspaceAnalyzer({
    root: resolve(sourceRoot, 'packages/typert/generator/tests/fixtures', name),
    ...(faces === undefined ? {} : { faces }),
  }).analyze();
}

function outcome(run) {
  try {
    return { ok: run() };
  } catch (error) {
    return { error: { name: error.name, message: error.message } };
  }
}

function rendererFixture(workspace) {
  return {
    workspace,
    faces: workspace.faces.map(face => {
      const renderer = new TypeGraphRenderer(face.graph);
      return {
        face: face.face,
        nodes: face.graph.nodes.map(node => ({
          id: node.id,
          rendered: outcome(() => renderer.renderType(node.id)),
          edges: outcome(() => childTypeNodeIds(node)),
          closure: outcome(() => renderer.declarationClosureForTypes([node.id]).map(item => item.id)),
        })),
        declarations: face.graph.declarations.map(declaration => ({
          id: declaration.id,
          rendered: outcome(() => renderer.renderDeclaration(declaration.id)),
          closure: outcome(() => renderer.declarationClosureForMembers(declaration.members.map(item => item.id)).map(item => item.id)),
          members: declaration.members.map(member => ({
            id: member.id,
            rendered: outcome(() => renderer.renderMember(member)),
            source: outcome(() => renderer.renderMember(member, true)),
          })),
        })),
      };
    }),
  };
}

function artifacts(workspace) {
  return workspace.faces.flatMap(face => {
    const emitter = new FaceModelEmitter(face);
    return face.packages.map(item => ({ face: face.face, artifact: emitter.emit(item.name) }));
  });
}

const typeModel = analyze('type-model');
let fixture;
if (process.argv[2] === 'renderer') {
  fixture = rendererFixture(typeModel);
} else if (process.argv[2] === 'emitter') {
  const remoteModel = analyze('remote-model', ['host']);
  fixture = { cases: [
    { name: 'type-model', artifacts: artifacts(typeModel) },
    { name: 'remote-model', workspace: remoteModel, artifacts: artifacts(remoteModel) },
  ] };
} else {
  throw new Error('Choose renderer or emitter fixture output');
}
process.stdout.write(`${JSON.stringify(fixture, (key, value) => typeof value === 'bigint' ? { $bigint: String(value) } : value, 2)}\n`);
";
