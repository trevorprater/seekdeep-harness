//! Runs the pinned Typert generator specs against the Rust generator.
//!
//! The source specs are copied unchanged next to compatibility adapters whose
//! every behavior is a request to the compiled `seekdeep-typert-generator`
//! runner, so the Vitest corpus (including its committed snapshots) becomes the
//! executable specification for the native analyzer, emitter, renderer, catalog
//! projection, and build-plugin surfaces.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

const SPECS: &[&str] = &[
    "type-model.spec.ts",
    "remote-model.spec.ts",
    "renderer.spec.ts",
    "schema-emitter.spec.ts",
    "tsdown-plugin.spec.ts",
    "tools-catalog.spec.ts",
    "cordis-catalog-contract.spec.ts",
    "cordis-catalog.spec.ts",
];

const ANALYZER: &str = r"
import { spawnSync } from 'node:child_process'

export class TypertAnalysisError extends Error { override name = 'TypertAnalysisError' }
export class TypertEmitError extends Error { override name = 'TypertEmitError' }
export class TypeGraphRenderError extends Error { override name = 'TypeGraphRenderError' }

export type AnalysisMode = 'check' | 'write'
export type TypertFace = 'host' | 'client'

export interface WorkspaceAnalyzerOptions {
  readonly root: string
  readonly hostConfig?: string
  readonly clientConfig?: string
  readonly packages?: readonly string[]
  readonly faces?: readonly TypertFace[]
  readonly checkDiagnostics?: boolean
  readonly mode?: AnalysisMode
  readonly caches?: WorkspaceCaches
}

export interface DiscoveredTypertPackage {
  readonly package: string
  readonly root: string
  readonly faces: readonly TypertFace[]
}

/** Memo handle; every request runs in a fresh runner process that owns its own compiler memo. */
export class WorkspaceCaches {
  readonly configs = new Map<string, unknown>()
  readonly registrations = new Map<string, unknown>()
  invalidate(_file: string): void {}
}

const replacer = (_key: string, value: unknown): unknown =>
  typeof value === 'bigint' ? { $bigint: String(value) } : value instanceof Set ? [...value] : value

const reviver = (_key: string, value: unknown): unknown =>
  value !== null && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).length === 1 && '$bigint' in value
    ? BigInt((value as { $bigint: string }).$bigint)
    : value

export function run(request: Record<string, unknown>): any {
  const binary = process.env.SEEKDEEP_TYPERT_GENERATOR
  if (binary === undefined) throw new Error('SEEKDEEP_TYPERT_GENERATOR is not set')
  const result = spawnSync(binary, [], {
    input: JSON.stringify(request, replacer),
    encoding: 'utf8',
    maxBuffer: 1024 * 1024 * 1024,
  })
  if (result.error) throw result.error
  let reply: { ok?: unknown; error?: { name: string; message: string } }
  try {
    reply = JSON.parse(result.stdout.trim(), reviver)
  } catch {
    throw new Error(`typert generator produced no reply (status ${String(result.status)}): ${result.stderr}`)
  }
  if (reply.error !== undefined) throw generatorError(reply.error)
  return reply.ok
}

function generatorError({ name, message }: { name: string; message: string }): Error {
  switch (name) {
    case 'TypertAnalysisError': return new TypertAnalysisError(message)
    case 'TypertEmitError': return new TypertEmitError(message)
    case 'TypeGraphRenderError': return new TypeGraphRenderError(message)
    case 'SyntaxError': return new SyntaxError(message)
    default: return new Error(message)
  }
}

function serializeOptions(options: WorkspaceAnalyzerOptions): Record<string, unknown> {
  const { caches, ...rest } = options
  void caches
  return rest
}

export class WorkspaceAnalyzer {
  constructor(private readonly options: WorkspaceAnalyzerOptions) {}
  analyze(): any { return run({ command: 'analyze', options: serializeOptions(this.options) }) }
  analyzeInBatches(batchSize = 8): any {
    return run({ command: 'analyzeInBatches', options: serializeOptions(this.options), batchSize })
  }
  discoverPackages(): DiscoveredTypertPackage[] {
    return run({ command: 'discoverPackages', options: serializeOptions(this.options) })
  }
  indexSourceDeclarations(): any { return run({ command: 'indexSourceDeclarations', options: serializeOptions(this.options) }) }
}
";

const EMITTER: &str = r"
import { run, TypertEmitError } from './analyzer.ts'
export { TypertEmitError }
export class FaceModelEmitter {
  constructor(private readonly face: unknown) {}
  emit(packageName: string): any { return run({ command: 'emit', face: this.face, package: packageName }) }
}
";

const RENDERER: &str = r"
import { run, TypeGraphRenderError } from './analyzer.ts'
export { TypeGraphRenderError }
export class TypeGraphRenderer {
  constructor(private readonly graph: unknown) {}
  renderType(id: string): string { return run({ command: 'renderType', graph: this.graph, id }) }
  renderDeclaration(id: string): string { return run({ command: 'renderDeclaration', graph: this.graph, id }) }
  renderMember(member: unknown): string { return run({ command: 'renderMember', graph: this.graph, member }) }
  node(id: string): unknown { return run({ command: 'rendererNode', graph: this.graph, id }) }
  declaration(id: string): unknown { return run({ command: 'rendererDeclaration', graph: this.graph, id }) }
  member(id: string): unknown { return run({ command: 'rendererMember', graph: this.graph, id }) }
  declarationClosureForMembers(members: readonly string[]): any[] {
    return run({ command: 'declarationClosureForMembers', graph: this.graph, members })
  }
  declarationClosureForTypes(types: readonly string[]): any[] {
    return run({ command: 'declarationClosureForTypes', graph: this.graph, types })
  }
}
";

const MODEL: &str = r"
import { run } from './analyzer.ts'
export type * from __SOURCE_MODEL__
export function childTypeNodeIds(node: unknown): string[] { return run({ command: 'childTypeNodeIds', node }) }
";

const WORKSPACE: &str = r"
import { run } from './analyzer.ts'
export class WorkspaceTypertGenerator {
  constructor(private readonly root: string) {}
  discover(faces?: readonly string[]): any {
    return run({ command: 'discover', root: this.root, ...(faces === undefined ? {} : { faces }) })
  }
  generate(packages?: readonly string[], faces?: readonly string[]): any {
    return run({
      command: 'generate',
      root: this.root,
      ...(packages === undefined ? {} : { packages }),
      ...(faces === undefined ? {} : { faces }),
    })
  }
}
";

const TSDOWN_PLUGIN: &str = r"
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { run } from './analyzer.ts'
import { WorkspaceTypertGenerator } from './workspace.ts'

export function typertPlugin(pluginOptions: { mode?: 'package' | 'workspace'; faces?: readonly string[] } = {}) {
  const artifactsByRoot = new Map<string, readonly any[]>()
  const emittedWorkspaces = new Set<string>()
  return {
    name: 'seekdeep-typert-generator',
    transform(code: string, id: string) {
      const lowered = run({ command: 'transpile', code, file: id })
      return lowered === null ? undefined : { code: lowered.code, map: lowered.map ?? undefined }
    },
    writeBundle(bundleOptions: { dir?: string }) {
      if (bundleOptions.dir === undefined) return
      const root: string = run({ command: 'workspaceRoot', start: bundleOptions.dir })
      if (emittedWorkspaces.has(root)) return
      if (pluginOptions.mode === 'workspace') {
        emitWorkspace(root, pluginOptions.faces)
        emittedWorkspaces.add(root)
        return
      }
      const packageDir: string | null = run({ command: 'packageRoot', start: bundleOptions.dir, workspace: root })
      if (packageDir === null) return
      const manifest = JSON.parse(readFileSync(join(packageDir, 'package.json'), 'utf8')) as { name?: string; exports?: unknown }
      if (manifest.name === undefined || !run({ command: 'hasTypertExport', exports: manifest.exports ?? null })) return
      let artifacts = artifactsByRoot.get(root)
      if (artifacts === undefined) {
        const generator = new WorkspaceTypertGenerator(root)
        artifacts = pluginOptions.faces === undefined
          ? generator.generate()
          : generator.generate(undefined, pluginOptions.faces)
        artifactsByRoot.set(root, artifacts as readonly any[])
      }
      run({ command: 'writeArtifacts', packageDir, artifacts: (artifacts as readonly any[]).filter(candidate => candidate.package === manifest.name) })
    },
  }

  function emitWorkspace(root: string, faces: readonly string[] | undefined): void {
    const generator = new WorkspaceTypertGenerator(root)
    const packages = (generator.discover(faces) as { package: string; root: string }[])
      .filter(candidate => run({
        command: 'hasTypertExport',
        exports: (JSON.parse(readFileSync(join(root, candidate.root, 'package.json'), 'utf8')) as { exports?: unknown }).exports ?? null,
      }))
      .map(candidate => candidate.package)
    if (packages.length === 0) return
    for (const artifact of generator.generate(packages, faces) as { packageRoot: string }[]) {
      run({ command: 'writeArtifacts', packageDir: join(root, artifact.packageRoot), artifacts: [artifact] })
    }
  }
}
";

const CORDIS_CATALOG: &str = r"
import { run } from './analyzer.ts'
export type * from __SOURCE_CATALOG__
export const REGION_BEGIN = '<!-- BEGIN GENERATED cordis-surface (gen-cordis-catalog.ts) — do not edit between markers -->'
export const REGION_END = '<!-- END GENERATED cordis-surface -->'
export function projectCordisCatalog(scanRoot: string, policy: unknown, targetFace = 'host') {
  const projection = run({ command: 'projectCordisCatalog', root: scanRoot, policy, face: targetFace })
  return {
    projector: {
      renderRuntimeApi(model: unknown): string {
        return run({ command: 'renderRuntimeApi', face: projection.face, sourceDeclarations: projection.sourceDeclarations, policy, model })
      },
    },
    model: projection.model,
  }
}
export function collectEvents(scanRoot: string, policy: unknown): any[] { return [...projectCordisCatalog(scanRoot, policy).model.events] }
export function collectServices(scanRoot: string, policy: unknown): any[] { return [...projectCordisCatalog(scanRoot, policy).model.services] }
export function renderPageRegion(page: string, services: unknown[], events: unknown[], policy: unknown): string {
  return run({ command: 'renderPageRegion', page, services, events, policy })
}
export function renderInheritedPage(policy: unknown): string { return run({ command: 'renderInheritedPage', policy }) }
";

/// Specs whose fixtures live beside the spec and carry the mechanical product rename.
const FIXTURE_SPECS: &[&str] = &[
    "type-model.spec.ts",
    "remote-model.spec.ts",
    "renderer.spec.ts",
    "schema-emitter.spec.ts",
    "tsdown-plugin.spec.ts",
];

/// The mechanical product rename applied to every ported text surface.
fn rename(text: &str) -> String {
    text.replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
        .replace("@deepseek-ai/cordis", "@seekdeep-ai/cordis")
        .replace("DeepSeek Harness", "SeekDeep Harness")
}

pub(super) fn run(source: &Path, filter: Option<&str>) -> anyhow::Result<()> {
    super::verify_source(source)?;
    let metadata = super::cargo_metadata()?;
    let status = Command::new("cargo")
        .args([
            "build",
            "--locked",
            "-p",
            "seekdeep-typert-generator",
            "--bin",
            "seekdeep-typert-generator",
        ])
        .env("CARGO_INCREMENTAL", "0")
        .current_dir(&metadata.workspace_root)
        .status()?;
    anyhow::ensure!(status.success(), "typert generator runner build failed");
    let runner = metadata
        .target_directory
        .join("debug/seekdeep-typert-generator");
    let corpus = metadata.target_directory.join("xtask/typert-corpus");
    if corpus.exists() {
        std::fs::remove_dir_all(&corpus)?;
    }
    // The corpus mirrors the source layout so realpath-relative external symbol
    // identities recorded in the pinned snapshot resolve to the same depth.
    let source_generator = source.join("packages/typert/generator");
    let port_generator = metadata.workspace_root.join("packages/typert/generator");
    let generator = corpus.join("packages/typert/generator");
    std::fs::create_dir_all(generator.join("src"))?;
    materialize_node_modules(source, &source_generator, &corpus, &generator)?;
    let adapters = [
        ("analyzer.ts", ANALYZER.to_owned()),
        ("emitter.ts", EMITTER.to_owned()),
        ("renderer.ts", RENDERER.to_owned()),
        (
            "model.ts",
            MODEL.replace(
                "__SOURCE_MODEL__",
                &super::quoted_path(&source_generator.join("src/model.ts"))?,
            ),
        ),
        ("workspace.ts", WORKSPACE.to_owned()),
        ("tsdown-plugin.ts", TSDOWN_PLUGIN.to_owned()),
        (
            "cordis-catalog.ts",
            CORDIS_CATALOG.replace(
                "__SOURCE_CATALOG__",
                &super::quoted_path(&source_generator.join("src/cordis-catalog.ts"))?,
            ),
        ),
    ];
    for (name, body) in adapters {
        std::fs::write(generator.join("src").join(name), body.trim_start())?;
    }
    let tests = generator.join("tests");
    std::fs::create_dir_all(tests.join("__snapshots__"))?;
    verify_fixture_rename(
        &source_generator.join("tests/fixtures"),
        &port_generator.join("tests/fixtures"),
    )?;
    copy_directory(
        &port_generator.join("tests/fixtures"),
        &tests.join("fixtures"),
    )?;
    std::fs::copy(
        port_generator.join("tests/__snapshots__/type-model.spec.ts.snap"),
        tests.join("__snapshots__/type-model.spec.ts.snap"),
    )?;
    let source_root = super::quoted_path(source)?;
    let catalog_script = super::quoted_path(&source.join("scripts/gen-cordis-catalog.ts"))?;
    for spec in SPECS {
        let body = std::fs::read_to_string(source_generator.join("tests").join(spec))?;
        let body = if FIXTURE_SPECS.contains(spec) {
            rename(&body).replace("'dsh-typert-generator'", "'seekdeep-typert-generator'")
        } else {
            // The runtime catalog banner carries the renamed product module identity.
            body.replace("resolve(import.meta.dirname, '../../../..')", &source_root)
                .replace("'../../../../scripts/gen-cordis-catalog.ts'", &catalog_script)
                .replace(
                    "expected('packages/extensions/tool-cordis/src/api-catalog.ts'),",
                    "expected('packages/extensions/tool-cordis/src/api-catalog.ts').replace('@deepseek-ai/dsh-tool-cordis/api-catalog', '@seekdeep-ai/seekdeep-tool-cordis/api-catalog'),",
                )
        };
        std::fs::write(tests.join(spec), body)?;
    }
    std::fs::write(
        corpus.join("package.json"),
        "{\"type\":\"module\",\"private\":true}\n",
    )?;
    let config = format!(
        "import tsconfigPaths from 'vite-tsconfig-paths'\nimport {{ standardDecoratorPlugin }} from {shared}\nexport default {{\n  plugins: [tsconfigPaths({{ projects: [{base}] }}), standardDecoratorPlugin()],\n  test: {{ include: ['packages/typert/generator/tests/*.spec.ts'], maxWorkers: 1, fileParallelism: false, testTimeout: 600_000 }},\n}}\n",
        shared = super::quoted_path(&source.join("vitest.shared.ts"))?,
        base = super::quoted_path(&source.join("tsconfig.base.json"))?,
    );
    let config_path = corpus.join("vitest.config.mts");
    std::fs::write(&config_path, config)?;
    let mut command = Command::new("node");
    command
        .arg(source.join("node_modules/vitest/vitest.mjs"))
        .args(["run", "--config"])
        .arg(&config_path)
        .env("CI", "1")
        .env("SEEKDEEP_TYPERT_GENERATOR", &runner)
        .env(
            "SEEKDEEP_TYPESCRIPT_LIBRARY",
            source.join("node_modules/typescript/lib/typescript.js"),
        )
        .current_dir(&corpus);
    if let Some(filter) = filter {
        command.arg(filter);
    }
    let status = command.status()?;
    anyhow::ensure!(
        status.success(),
        "pinned Typert generator corpus failed against the Rust generator"
    );
    println!(
        "Typert corpus: {} pinned generator spec files passed against the Rust generator",
        SPECS.len()
    );
    Ok(())
}

/// Every ported fixture file must be the mechanical rename of its source file.
fn verify_fixture_rename(source: &Path, port: &Path) -> anyhow::Result<()> {
    for entry in walk(source)? {
        let relative = entry.strip_prefix(source)?;
        let ported = port.join(relative);
        anyhow::ensure!(
            ported.is_file(),
            "ported fixture missing: {}",
            ported.display()
        );
        anyhow::ensure!(
            std::fs::read_to_string(&ported)? == rename(&std::fs::read_to_string(&entry)?),
            "ported fixture drifted from the renamed source: {}",
            ported.display()
        );
    }
    Ok(())
}

fn walk(directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            files.extend(walk(&entry.path())?);
        } else {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

/// Builds the corpus `node_modules` trees: symlinks into the pinned source
/// installation, except the dependency whose real path the snapshot records,
/// which is copied so its identity resolves relative to the corpus root.
fn materialize_node_modules(
    source: &Path,
    source_generator: &Path,
    corpus: &Path,
    generator: &Path,
) -> anyhow::Result<()> {
    let root_modules = corpus.join("node_modules");
    std::fs::create_dir_all(root_modules.join(".pnpm"))?;
    let zod_link = source_generator.join("node_modules/zod");
    let zod_real = std::fs::canonicalize(&zod_link)?;
    let zod_relative = zod_real.strip_prefix(source.join("node_modules/.pnpm").canonicalize()?)?;
    let store_entry = zod_relative
        .components()
        .next()
        .ok_or_else(|| anyhow::anyhow!("zod store entry absent"))?;
    for entry in std::fs::read_dir(source.join("node_modules"))? {
        let entry = entry?;
        if entry.file_name() != ".pnpm" {
            std::os::unix::fs::symlink(entry.path(), root_modules.join(entry.file_name()))?;
        }
    }
    for entry in std::fs::read_dir(source.join("node_modules/.pnpm"))? {
        let entry = entry?;
        let target = root_modules.join(".pnpm").join(entry.file_name());
        if entry.file_name() == store_entry.as_os_str() {
            copy_directory(&entry.path(), &target)?;
        } else {
            std::os::unix::fs::symlink(entry.path(), target)?;
        }
    }
    let generator_modules = generator.join("node_modules");
    std::fs::create_dir_all(&generator_modules)?;
    for entry in std::fs::read_dir(source_generator.join("node_modules"))? {
        let entry = entry?;
        let target = generator_modules.join(entry.file_name());
        if entry.file_name() == "zod" {
            std::os::unix::fs::symlink(std::fs::read_link(&zod_link)?, target)?;
        } else {
            std::os::unix::fs::symlink(entry.path(), target)?;
        }
    }
    Ok(())
}

fn copy_directory(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target: PathBuf = to.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &target)?;
        } else if file_type.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
