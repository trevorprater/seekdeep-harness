//! Repository gates that keep the source inventory and Rust parity evidence synchronized.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::{Duration, UNIX_EPOCH},
};

use base64::Engine as _;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate or verify the plugin configuration catalog from the pinned source tree.
    ConfigCatalog {
        /// Pinned source checkout containing the TypeScript package declarations.
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
        /// Verify the tracked output without writing it.
        #[arg(long)]
        check: bool,
    },
    /// Verify every exported Rust API has documentation and all prose-adjacent lints pass.
    Docs,
    /// Synchronize the tracked source-file inventory while preserving evidence.
    Inventory {
        /// Source checkout recorded in `SOURCE_SNAPSHOT`.
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
    },
    /// Verify Mach-O deployment targets do not exceed the runtime wheel's macOS claim.
    MacosDeploymentTarget {
        /// Runtime executable and any required native helper sidecars.
        #[arg(required = true)]
        executables: Vec<PathBuf>,
        /// Wheel platform tag; defaults from the checked-in platform manifest.
        #[arg(long)]
        platform_tag: Option<String>,
    },
    /// Check or rewrite canonical packed-row Session JSONL fixtures.
    SessionFixtureLayout {
        /// Rewrite noncanonical fixtures in place.
        #[arg(long)]
        rewrite: bool,
        /// Repository root; defaults to this workspace.
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Verify that every source surface has explicit parity evidence.
    Parity {
        /// Source checkout recorded in `SOURCE_SNAPSHOT`.
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
        /// Surfaces enforced: `all` is the final gate, `runtime` defers
        /// localization artifacts (Chinese translations and their metadata).
        #[arg(long, value_enum, default_value_t = Scope::All)]
        scope: Scope,
    },
    /// Generate or verify the durable Session event catalog from the pinned source tree.
    PersistenceCatalog {
        /// Source checkout recorded in `SOURCE_SNAPSHOT`.
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
        /// Verify tracked outputs without writing them.
        #[arg(long)]
        check: bool,
    },
    /// Generate or verify the model-facing tool-schema catalog from Rust runtime registrations.
    ToolCatalog {
        /// Pinned source checkout used for the exhaustive `tool-*` inventory.
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
        /// Verify the tracked output without writing it.
        #[arg(long)]
        check: bool,
    },
    /// Builds one Rust/WASM Client package as a synchronous classic module-table bundle.
    WasmPackage {
        /// Cargo package containing the browser cdylib.
        #[arg(long, default_value = "seekdeep-client-runtime")]
        package: String,
        /// Cargo artifact stem (`-` becomes `_`).
        #[arg(long, default_value = "seekdeep_client_runtime")]
        artifact: String,
        /// Client module-table identity.
        #[arg(long, default_value = "@seekdeep-ai/seekdeep-client-runtime")]
        module_id: String,
        /// Package output directory.
        #[arg(long, default_value = "packages/client/runtime/lib")]
        out_dir: PathBuf,
        /// Rebuild whenever package-owned Rust/CSS/manifest inputs change.
        #[arg(long)]
        watch: bool,
    },
}

/// Which surfaces the parity gate enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Scope {
    /// Enforce every tracked source surface (the final manifest gate).
    All,
    /// Enforce runtime surfaces only; defer localization artifacts.
    Runtime,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Command::ConfigCatalog { source, check } => {
            xtask::config_catalog::run(Path::new("."), &source, check)
        }
        Command::Docs => docs(),
        Command::Inventory { source } => inventory(&source),
        Command::MacosDeploymentTarget {
            executables,
            platform_tag,
        } => macos_deployment_target(&executables, platform_tag.as_deref()),
        Command::SessionFixtureLayout { rewrite, root } => {
            let root = root.unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .expect("xtask has a workspace parent")
                    .to_owned()
            });
            xtask::session_fixture_layout::run(&root, rewrite)
        }
        Command::Parity { source, scope } => parity(&source, scope),
        Command::PersistenceCatalog { source, check } => {
            xtask::persistence_catalog::run(Path::new("."), &source, check)
        }
        Command::ToolCatalog { source, check } => {
            verify_source(&source)?;
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(xtask::tool_catalog::run(Path::new("."), &source, check))
        }
        Command::WasmPackage {
            package,
            artifact,
            module_id,
            out_dir,
            watch,
        } => wasm_package(&package, &artifact, &module_id, &out_dir, watch),
    }
}

fn macos_deployment_target(
    executables: &[PathBuf],
    platform_tag: Option<&str>,
) -> anyhow::Result<()> {
    let platform_tag = match platform_tag {
        Some(platform_tag) => platform_tag.to_owned(),
        None => default_macos_platform_tag()?,
    };
    for (path, version) in
        xtask::macos_deployment::validate_deployment_targets(executables, &platform_tag)?
    {
        println!(
            "{}: macOS {} <= {platform_tag}",
            path.display(),
            version.render()
        );
    }
    Ok(())
}

fn default_macos_platform_tag() -> anyhow::Result<String> {
    let manifest_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../python/sdk-runtime/platforms.json");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
    manifest
        .pointer("/macos-arm64/tag")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("python runtime platform manifest has no macos-arm64 tag"))
}

fn wasm_package(
    package: &str,
    artifact: &str,
    module_id: &str,
    out_dir: &Path,
    watch: bool,
) -> anyhow::Result<()> {
    if !watch {
        return wasm_package_once(package, artifact, module_id, out_dir);
    }
    if let Err(error) = wasm_package_once(package, artifact, module_id, out_dir) {
        eprintln!("Rust/WASM initial watch build failed: {error:#}");
    }
    let package_root = cargo_package_root(package)?;
    let asset_root = match module_id {
        "@seekdeep-ai/seekdeep-client-ui-theme" => Some(
            cargo_metadata()?
                .workspace_root
                .join("packages/client/ui-theme/src/styles"),
        ),
        "@seekdeep-ai/seekdeep-client-web" => Some(
            cargo_metadata()?
                .workspace_root
                .join("packages/client/web/src/base.css"),
        ),
        "@seekdeep-ai/seekdeep-client-ui-primitives" => Some(
            cargo_metadata()?
                .workspace_root
                .join("packages/client/ui-primitives"),
        ),
        _ => None,
    };
    let mut previous = wasm_watch_snapshot(&package_root, asset_root.as_deref())?;
    println!(
        "watching {} for Rust/WASM bundle changes",
        package_root.display()
    );
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let current = wasm_watch_snapshot(&package_root, asset_root.as_deref())?;
        if current == previous {
            continue;
        }
        previous = current;
        if let Err(error) = wasm_package_once(package, artifact, module_id, out_dir) {
            eprintln!("Rust/WASM watch rebuild failed: {error:#}");
        }
    }
}

fn wasm_package_once(
    package: &str,
    artifact: &str,
    module_id: &str,
    out_dir: &Path,
) -> anyhow::Result<()> {
    let metadata = cargo_metadata()?;
    let status = ProcessCommand::new("cargo")
        .args([
            "build",
            "-p",
            package,
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .status()?;
    anyhow::ensure!(
        status.success(),
        "Rust/WASM release build failed for {package}"
    );
    let wasm = metadata
        .target_directory
        .join("wasm32-unknown-unknown/release")
        .join(format!("{artifact}.wasm"));
    anyhow::ensure!(
        wasm.is_file(),
        "Rust/WASM artifact is missing: {}",
        wasm.display()
    );
    if module_id == "@seekdeep-ai/seekdeep-client-web" {
        return wasm_web_shell_package(&metadata, artifact, out_dir, &wasm);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-primitives" {
        return wasm_ui_primitives_package(&metadata, artifact, out_dir, &wasm);
    }
    let staging = metadata
        .target_directory
        .join("xtask/wasm-package")
        .join(artifact);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let global = format!("__{}_wasm", artifact.replace('-', "_"));
    let status = ProcessCommand::new("wasm-bindgen")
        .args([
            "--target",
            "no-modules",
            "--out-name",
            "client",
            "--no-modules-global",
            &global,
            "--out-dir",
        ])
        .arg(&staging)
        .arg(&wasm)
        .status()?;
    anyhow::ensure!(status.success(), "wasm-bindgen failed for {package}");
    let bindings = std::fs::read_to_string(staging.join("client.js"))?;
    let bytes = std::fs::read(staging.join("client_bg.wasm"))?;
    let bundle = classic_module_bundle(&bindings, &bytes, &global, module_id)?;
    let out_dir = if out_dir.is_absolute() {
        out_dir.to_owned()
    } else {
        metadata.workspace_root.join(out_dir)
    };
    std::fs::create_dir_all(&out_dir)?;
    std::fs::write(out_dir.join("client.js"), bundle)?;
    let type_dir = out_dir.join("types/client");
    std::fs::create_dir_all(&type_dir)?;
    let mut declarations = std::fs::read_to_string(staging.join("client.d.ts"))?
        .replace("        [Symbol.dispose](): void;\n", "");
    declarations.push_str(&compatibility_declarations(module_id));
    std::fs::write(type_dir.join("index.d.ts"), declarations)?;
    copy_wasm_package_assets(&metadata.workspace_root, module_id, &out_dir)?;
    write_wasm_package_compatibility_entries(module_id, &out_dir)?;
    println!(
        "built {module_id} Rust/WASM classic bundle at {}",
        out_dir.join("client.js").display()
    );
    Ok(())
}

fn wasm_web_shell_package(
    metadata: &CargoMetadata,
    artifact: &str,
    out_dir: &Path,
    wasm: &Path,
) -> anyhow::Result<()> {
    let staging = metadata
        .target_directory
        .join("xtask/wasm-package")
        .join(artifact);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let status = ProcessCommand::new("wasm-bindgen")
        .args(["--target", "web", "--out-name", "client", "--out-dir"])
        .arg(&staging)
        .arg(wasm)
        .status()?;
    anyhow::ensure!(status.success(), "wasm-bindgen failed for client web shell");
    let out_dir = if out_dir.is_absolute() {
        out_dir.to_owned()
    } else {
        metadata.workspace_root.join(out_dir)
    };
    std::fs::create_dir_all(&out_dir)?;
    for name in ["client.js", "client.d.ts", "client_bg.wasm"] {
        std::fs::copy(staging.join(name), out_dir.join(name))?;
    }
    std::fs::copy(
        metadata
            .workspace_root
            .join("packages/client/web/src/base.css"),
        out_dir.join("base.css"),
    )?;
    std::fs::write(out_dir.join("index.js"), client_web_esm_wrapper())?;
    let type_dir = out_dir.join("types");
    std::fs::create_dir_all(&type_dir)?;
    std::fs::write(type_dir.join("index.d.ts"), client_web_esm_declarations())?;
    println!(
        "built @seekdeep-ai/seekdeep-client-web Rust/WASM ESM shell at {}",
        out_dir.join("index.js").display()
    );
    Ok(())
}

fn client_web_esm_wrapper() -> &'static str {
    r"import init, * as wasm from './client.js';
import * as React from 'react';
import * as ReactJsxRuntime from 'react/jsx-runtime';
import * as ReactDom from 'react-dom';
import * as ReactDomClient from 'react-dom/client';
import * as Cordis from '@seekdeep-ai/cordis';
import Loader from '@seekdeep-ai/cordis-plugin-loader';
import * as ClientModules from '@seekdeep-ai/seekdeep-client-modules/client';
import * as ModulesClient from '@seekdeep-ai/seekdeep-client-modules/client';
import * as WebReact from '@seekdeep-ai/seekdeep-client-web-react';
import * as UiSlots from '@seekdeep-ai/seekdeep-client-ui-slots';
import * as UiPrimitives from '@seekdeep-ai/seekdeep-client-ui-primitives';
import * as UiAttachment from '@seekdeep-ai/seekdeep-client-ui-attachment';
import * as SchemaForm from '@seekdeep-ai/seekdeep-client-schema-form';
import './base.css';

await init(new URL('./client_bg.wasm', import.meta.url));
const staticModules = {
  'react': React,
  'react/jsx-runtime': ReactJsxRuntime,
  'react-dom': ReactDom,
  'react-dom/client': ReactDomClient,
  '@seekdeep-ai/cordis': Cordis,
  '@seekdeep-ai/seekdeep-client-ui-slots': UiSlots,
  '@seekdeep-ai/seekdeep-client-web-react': WebReact,
  '@seekdeep-ai/seekdeep-client-ui-primitives': UiPrimitives,
  '@seekdeep-ai/seekdeep-client-ui-attachment': UiAttachment,
  '@seekdeep-ai/seekdeep-client-schema-form': SchemaForm,
};
wasm.configureClientWeb(React, ReactDomClient, Cordis, Loader, ClientModules, ModulesClient, WebReact, staticModules);

export const AppWebEntry = wasm.AppWebEntry;
export const AppRoot = wasm.appRootComponent();
export const DocumentTitle = wasm.documentTitleComponent();
export const buildRenderApp = wasm.buildRenderApp;
export const APP_SHELL_ID = wasm.appShellId();
export const getStaticModules = wasm.getStaticModules;
export const PLATFORM_MODULES = Object.freeze(Array.from(wasm.platformModules()));
export const createSignal = wasm.createSignal;
export const createLoaderStatusStore = wasm.createLoaderStatusStore;
export const FIBER_STATE = Object.freeze({ PENDING: 0, LOADING: 1, ACTIVE: 2, FAILED: 3, DISPOSED: 4, UNLOADING: 5 });
export const STATE_LABELS = Object.freeze({ 0: 'pending', 1: 'loading', 2: 'active', 3: 'failed', 4: 'disposed', 5: 'unloading' });
"
}

fn client_web_esm_declarations() -> &'static str {
    r"export { AppWebEntry } from '../client.js';
export const AppRoot: Function;
export const DocumentTitle: Function;
export const buildRenderApp: typeof import('../client.js').buildRenderApp;
export const APP_SHELL_ID: '@seekdeep-ai/seekdeep-client-app-shell';
export const getStaticModules: typeof import('../client.js').getStaticModules;
export const PLATFORM_MODULES: readonly string[];
export const createSignal: typeof import('../client.js').createSignal;
export const createLoaderStatusStore: typeof import('../client.js').createLoaderStatusStore;
export const FIBER_STATE: Readonly<{ PENDING: 0; LOADING: 1; ACTIVE: 2; FAILED: 3; DISPOSED: 4; UNLOADING: 5 }>;
export const STATE_LABELS: Readonly<Record<number, 'pending' | 'loading' | 'active' | 'failed' | 'disposed' | 'unloading'>>;
export interface AppRootProps { settled: object; status: object; error: object; renderApp(): unknown }
export interface DocumentTitleProps { title?: string }
export interface AssemblyDeps { ctx: unknown }
export interface AppShellService { renderApp(): unknown }
export type BootSeams = { loadBundle?: Function };
export type LoaderEntryState = 'pending' | 'loading' | 'active' | 'failed' | 'disposed' | 'unloading';
export type LoaderStatus = Record<string, LoaderEntryState>;
"
}

fn wasm_ui_primitives_package(
    metadata: &CargoMetadata,
    artifact: &str,
    out_dir: &Path,
    wasm: &Path,
) -> anyhow::Result<()> {
    let staging = metadata
        .target_directory
        .join("xtask/wasm-package")
        .join(artifact);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let status = ProcessCommand::new("wasm-bindgen")
        .args(["--target", "web", "--out-name", "client", "--out-dir"])
        .arg(&staging)
        .arg(wasm)
        .status()?;
    anyhow::ensure!(
        status.success(),
        "wasm-bindgen failed for client UI primitives"
    );
    let out_dir = if out_dir.is_absolute() {
        out_dir.to_owned()
    } else {
        metadata.workspace_root.join(out_dir)
    };
    std::fs::create_dir_all(&out_dir)?;
    for name in ["client.js", "client.d.ts", "client_bg.wasm"] {
        std::fs::copy(staging.join(name), out_dir.join(name))?;
    }
    std::fs::write(
        out_dir.join("highlight-backend.js"),
        ui_primitives_highlight_backend(),
    )?;
    std::fs::write(
        out_dir.join("markdown-backend.js"),
        ui_primitives_markdown_backend(),
    )?;
    std::fs::write(out_dir.join("index.js"), ui_primitives_esm_wrapper())?;
    let type_dir = out_dir.join("types");
    if type_dir.exists() {
        std::fs::remove_dir_all(&type_dir)?;
    }
    let client_type_dir = type_dir.join("client");
    std::fs::create_dir_all(&client_type_dir)?;
    std::fs::copy(
        staging.join("client.d.ts"),
        client_type_dir.join("index.d.ts"),
    )?;
    copy_ui_primitives_type_declarations(&metadata.workspace_root, &type_dir)?;
    std::fs::write(
        type_dir.join("internal.d.ts"),
        ui_primitives_internal_declarations(),
    )?;
    copy_ui_primitives_katex_assets(&metadata.workspace_root, &out_dir)?;
    std::fs::write(
        out_dir.join("invariant.js"),
        ui_primitives_invariant_wrapper(),
    )?;
    std::fs::write(
        out_dir.join("internal.js"),
        ui_primitives_internal_wrapper(),
    )?;
    println!(
        "built @seekdeep-ai/seekdeep-client-ui-primitives Rust/WASM ESM library at {}",
        out_dir.join("index.js").display()
    );
    Ok(())
}

fn copy_ui_primitives_type_declarations(
    workspace: &Path,
    destination: &Path,
) -> anyhow::Result<()> {
    let source = workspace.join("packages/client/ui-primitives/assets/types");
    let mut count = 0_usize;
    for entry in walkdir::WalkDir::new(&source) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.path().file_name().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        if !name.ends_with(".d.ts.txt") {
            anyhow::ensure!(
                name == "README.md",
                "unexpected type compatibility asset: {}",
                entry.path().display()
            );
            continue;
        }
        let relative = entry.path().strip_prefix(&source)?;
        let relative = relative.with_file_name(name.strip_suffix(".txt").expect("checked suffix"));
        let output = destination.join(relative);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let declaration = std::fs::read_to_string(entry.path())?
            .replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
            .replace("@deepseek-ai/cordis", "@seekdeep-ai/cordis")
            .replace("DeepSeek Harness", "SeekDeep Harness");
        std::fs::write(output, declaration)?;
        count += 1;
    }
    anyhow::ensure!(
        count == 43,
        "expected 43 ui-primitives declaration files, found {count}"
    );
    Ok(())
}

fn ui_primitives_invariant_wrapper() -> &'static str {
    r"const PACKAGE_NAME = '@seekdeep-ai/seekdeep-client-ui-primitives';
export const name = 'client-ui-primitives-invariant';
export const inject = ['invariants'];
const install = () => {};
export const apply = ctx => Promise.resolve(ctx.invariants.register(PACKAGE_NAME, install));
"
}

fn ui_primitives_internal_wrapper() -> &'static str {
    r"export * from './index.js';
import * as wasm from './client.js';
export const highlightToHtml = wasm.highlightToHtml;
export const highlightLines = wasm.highlightLines;
export const subscribeGrammarLoaded = wasm.subscribeGrammarLoaded;
export const grammarLoadCount = wasm.grammarLoadCount;
export const usePointerGrace = wasm.usePointerGrace;
export const useCopyFeedback = wasm.useCopyFeedback;
"
}

fn ui_primitives_internal_declarations() -> &'static str {
    r"export * from './index.js';
export const highlightToHtml: typeof import('./client/index.js').highlightToHtml;
export const highlightLines: typeof import('./client/index.js').highlightLines;
export const subscribeGrammarLoaded: typeof import('./client/index.js').subscribeGrammarLoaded;
export const grammarLoadCount: typeof import('./client/index.js').grammarLoadCount;
export const usePointerGrace: typeof import('./client/index.js').usePointerGrace;
export const useCopyFeedback: typeof import('./client/index.js').useCopyFeedback;
"
}

fn copy_ui_primitives_katex_assets(workspace: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let source = workspace.join("packages/client/ui-primitives/assets/katex");
    let destination = out_dir.join("katex");
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    let destination_fonts = destination.join("fonts");
    std::fs::create_dir_all(&destination_fonts)?;
    for name in ["katex.min.css", "LICENSE", "README.md"] {
        std::fs::copy(source.join(name), destination.join(name))?;
    }
    let source_fonts = source.join("fonts");
    let mut fonts = std::fs::read_dir(&source_fonts)?.collect::<Result<Vec<_>, _>>()?;
    fonts.sort_by_key(std::fs::DirEntry::file_name);
    for font in fonts {
        let path = font.path();
        anyhow::ensure!(
            path.is_file(),
            "KaTeX font asset is not a file: {}",
            path.display()
        );
        anyhow::ensure!(
            matches!(
                path.extension().and_then(std::ffi::OsStr::to_str),
                Some("ttf" | "woff" | "woff2")
            ),
            "KaTeX font asset has an unsupported extension: {}",
            path.display()
        );
        std::fs::copy(&path, destination_fonts.join(font.file_name()))?;
    }
    Ok(())
}

fn ui_primitives_highlight_backend() -> &'static str {
    r#"import { createHighlighterCoreSync, createCssVariablesTheme } from 'shiki/core';
import { createJavaScriptRegexEngine, defaultJavaScriptRegexConstructor } from 'shiki/engine/javascript';
import langTs from '@shikijs/langs/typescript';
import langBash from '@shikijs/langs/shellscript';
import langJson from '@shikijs/langs/json';

const theme = createCssVariablesTheme({ name: 'css-variables', variablePrefix: '--shiki-', fontStyle: true });
const engine = createJavaScriptRegexEngine({
  forgiving: true,
  regexConstructor: pattern => defaultJavaScriptRegexConstructor(pattern, { lazyCompileLength: Number.POSITIVE_INFINITY }),
});
const lazy = new Map([
  ['python', () => import('@shikijs/langs/python')], ['ruby', () => import('@shikijs/langs/ruby')],
  ['go', () => import('@shikijs/langs/go')], ['rust', () => import('@shikijs/langs/rust')],
  ['java', () => import('@shikijs/langs/java')], ['c', () => import('@shikijs/langs/c')],
  ['cpp', () => import('@shikijs/langs/cpp')], ['csharp', () => import('@shikijs/langs/csharp')],
  ['kotlin', () => import('@shikijs/langs/kotlin')], ['swift', () => import('@shikijs/langs/swift')],
  ['php', () => import('@shikijs/langs/php')], ['yaml', () => import('@shikijs/langs/yaml')],
  ['toml', () => import('@shikijs/langs/toml')], ['ini', () => import('@shikijs/langs/ini')],
  ['markdown', () => import('@shikijs/langs/markdown')], ['mdx', () => import('@shikijs/langs/mdx')],
  ['html', () => import('@shikijs/langs/html')], ['css', () => import('@shikijs/langs/css')],
  ['scss', () => import('@shikijs/langs/scss')], ['less', () => import('@shikijs/langs/less')],
  ['sql', () => import('@shikijs/langs/sql')], ['xml', () => import('@shikijs/langs/xml')],
  ['lua', () => import('@shikijs/langs/lua')],
]);
let singleton;
function createHighlighter() {
  const instance = createHighlighterCoreSync({ themes: [theme], langs: [langTs, langBash, langJson], engine });
  for (const sample of [
    { lang: 'typescript', code: 'const answer: number = 42' },
    { lang: 'shellscript', code: 'printf \'%s\\n\' "$HOME"' },
    { lang: 'json', code: '{"ready":true}' },
  ]) instance.codeToTokens(sample.code, { lang: sample.lang, theme: 'css-variables', tokenizeTimeLimit: 0 });
  return instance;
}
function highlighter() {
  singleton ??= createHighlighter();
  return singleton;
}
export function createHighlightBackend() {
  return {
    warm() { highlighter(); },
    loadGrammar(id) {
      const load = lazy.get(id);
      if (load === undefined) throw new Error(`unknown lazy grammar: ${id}`);
      return load().then(mod => { highlighter().loadLanguageSync(mod.default); });
    },
    codeToHtml(code, lang) { return highlighter().codeToHtml(code, { lang, theme: 'css-variables' }); },
    codeToTokens(code, lang) { return highlighter().codeToTokens(code, { lang, theme: 'css-variables' }); },
  };
}
"#
}

fn ui_primitives_markdown_backend() -> &'static str {
    r#"import katex from 'katex';
import { normalizeUri } from 'micromark-util-sanitize-uri';

export function createMarkdownBackend(cssUrl) {
  return {
    cssUrl,
    normalizeUri,
    renderTex(value, options) { return katex.renderToString(value, options); },
  };
}
"#
}

fn ui_primitives_esm_wrapper() -> String {
    let mut wrapper = r"import init, * as wasm from './client.js';
import * as React from 'react';
import * as ReactDOM from 'react-dom';
import { createHighlightBackend } from './highlight-backend.js';
import { createMarkdownBackend } from './markdown-backend.js';

await init({ module_or_path: new URL('./client_bg.wasm', import.meta.url) });
wasm.configureClientUiPrimitiveHighlight(createHighlightBackend());
wasm.configureClientUiPrimitiveHooks(React);
wasm.configureClientUiPrimitiveAtoms(React, ReactDOM);
wasm.configureClientUiPrimitiveDialogs(React, ReactDOM);
wasm.configureClientUiPrimitiveIcons(React);
wasm.configureClientUiPrimitiveTooltip(React);
wasm.configureClientUiPrimitiveBlocks(React);
wasm.configureClientUiPrimitiveWeb(React);
wasm.configureClientUiPrimitiveHoverCard(React, ReactDOM);
wasm.configureClientUiPrimitiveMenu(React, ReactDOM);
wasm.configureClientUiPrimitiveJsonTree(React, ReactDOM);
wasm.configureClientUiPrimitiveMarkdownAtoms(React);
wasm.configureClientUiPrimitiveCodeBlock(React);
wasm.configureClientUiPrimitiveMarkdown(React, createMarkdownBackend(new URL('./katex/katex.min.css', import.meta.url).href));
wasm.configureClientUiPrimitiveReadBlock(React);
const iconComponents = wasm.iconComponents();

export const Button = wasm.buttonComponent();
export const Pill = wasm.pillComponent();
export const Input = wasm.inputComponent();
export const StateDot = wasm.stateDotComponent();
export const ConnectionBanner = wasm.connectionBannerComponent();
export const OnboardingSurface = wasm.onboardingSurfaceComponent();
export const Toast = wasm.toastComponent();
export const Modal = wasm.modalComponent();
export const DisclosureRow = wasm.disclosureRowComponent();
export const RiskConfirmation = wasm.riskConfirmationComponent();
export const Tooltip = wasm.tooltipComponent();
export const DiffBlock = wasm.diffBlockComponent();
export const SearchBlock = wasm.searchBlockComponent();
export const TerminalBlock = wasm.terminalBlockComponent();
export const WebBlock = wasm.webBlockComponent();
export const HoverCard = wasm.hoverCardComponent();
export const Menu = wasm.menuComponent();
export const JsonTree = wasm.jsonTreeComponent();
export const JsonBlock = wasm.jsonBlockComponent();
export const MessageText = wasm.messageTextComponent();
export const CodeBlock = wasm.codeBlockComponent();
export const MarkdownText = wasm.markdownTextComponent();
export const ReadBlock = wasm.readBlockComponent();
export const DEFAULT_DIFF_MAX_LINES = wasm.defaultDiffMaxLines();
export const DEFAULT_SEARCH_MAX_LINES = wasm.defaultSearchMaxLines();
export const DEFAULT_TERMINAL_MAX_LINES = wasm.defaultTerminalMaxLines();
export const DEFAULT_READ_MAX_LINES = wasm.defaultReadMaxLines();
export const extractMarkdownPlainText = wasm.extractMarkdownPlainText;
export const writeClipboard = wasm.writeClipboard;
export const useAnchoredMaxHeight = wasm.useAnchoredMaxHeight;
"
    .to_owned();
    for definition in seekdeep_client_ui_primitives::ICON_DEFINITIONS {
        debug_assert!(is_javascript_identifier(definition.name));
        wrapper.push_str(&format!(
            "export const {name} = iconComponents.{name};\n",
            name = definition.name
        ));
    }
    wrapper
}

fn wasm_watch_snapshot(
    package_root: &Path,
    asset_root: Option<&Path>,
) -> anyhow::Result<BTreeMap<PathBuf, (u64, u128)>> {
    let mut snapshot = watch_snapshot(package_root)?;
    if let Some(asset_root) = asset_root {
        snapshot.extend(watch_snapshot(asset_root)?);
    }
    Ok(snapshot)
}

fn copy_wasm_package_assets(
    workspace: &Path,
    module_id: &str,
    out_dir: &Path,
) -> anyhow::Result<()> {
    if module_id != "@seekdeep-ai/seekdeep-client-ui-theme" {
        return Ok(());
    }
    let source = workspace.join("packages/client/ui-theme/src/styles");
    let destination = out_dir.join("styles");
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::create_dir_all(&destination)?;
    let mut assets = std::fs::read_dir(&source)?.collect::<Result<Vec<_>, _>>()?;
    assets.sort_by_key(std::fs::DirEntry::file_name);
    for asset in assets {
        let path = asset.path();
        anyhow::ensure!(
            path.extension().and_then(std::ffi::OsStr::to_str) == Some("css"),
            "theme style directory contains a non-CSS asset: {}",
            path.display()
        );
        std::fs::copy(&path, destination.join(asset.file_name()))?;
    }
    Ok(())
}

fn write_wasm_package_compatibility_entries(module_id: &str, out_dir: &Path) -> anyhow::Result<()> {
    let invariant_name = match module_id {
        "@seekdeep-ai/seekdeep-client-ui-message-feedback" => "client-ui-feedback-invariant",
        "@seekdeep-ai/seekdeep-client-ui-jobs" => "client-ui-jobs-invariant",
        "@seekdeep-ai/seekdeep-client-ui-plan" => "client-ui-plan-invariant",
        "@seekdeep-ai/seekdeep-client-ui-goal" => "client-ui-goal-invariant",
        "@seekdeep-ai/seekdeep-client-ui-deliverables" => "client-ui-deliverables-invariant",
        "@seekdeep-ai/seekdeep-client-ui-directory-picker-native" => {
            "client-ui-directory-picker-native-invariant"
        }
        "@seekdeep-ai/seekdeep-client-ui-trajectory" => "client-ui-trajectory-invariant",
        "@seekdeep-ai/seekdeep-client-ui-user-questions" => "client-ui-user-questions-invariant",
        "@seekdeep-ai/seekdeep-client-ui-workflow-run" => "client-ui-workflow-run-invariant",
        "@seekdeep-ai/seekdeep-client-ui-settings-plugin-inventory" => {
            "client-ui-settings-plugin-inventory-invariant"
        }
        "@seekdeep-ai/seekdeep-client-ui-skill" => "client-ui-skill-invariant",
        "@seekdeep-ai/seekdeep-client-ui-subagent" => "client-ui-subagent-invariant",
        "@seekdeep-ai/seekdeep-client-ui-permission-presets" => {
            "client-ui-permission-presets-invariant"
        }
        "@seekdeep-ai/seekdeep-client-ui-model-selection" => "client-ui-model-selection-invariant",
        "@seekdeep-ai/seekdeep-client-ui-input-trigger" => "client-ui-input-trigger-invariant",
        "@seekdeep-ai/seekdeep-client-ui-commands" => "client-ui-commands-invariant",
        _ => return Ok(()),
    };
    std::fs::write(out_dir.join("index.js"), "export function apply() {}\n")?;
    std::fs::write(
        out_dir.join("invariant.js"),
        format!(
            r"const PACKAGE_NAME = '{module_id}';
export const name = '{invariant_name}';
export const inject = ['invariants'];
const install = () => {{}};
export const apply = ctx => Promise.resolve(ctx.invariants.register(PACKAGE_NAME, install));
",
        ),
    )?;
    let type_dir = out_dir.join("types");
    std::fs::create_dir_all(&type_dir)?;
    std::fs::write(
        type_dir.join("index.d.ts"),
        "export declare function apply(): void;\n",
    )?;
    std::fs::write(
        type_dir.join("invariant.d.ts"),
        format!(
            r#"export interface InvariantContext {{
  invariants: {{ register(packageName: string, install: () => void): () => void }};
}}
export declare const name = "{invariant_name}";
export declare const inject: string[];
export declare const apply: (ctx: InvariantContext) => Promise<() => void>;
"#,
        ),
    )?;
    Ok(())
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoMetadataPackage>,
    target_directory: PathBuf,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct CargoMetadataPackage {
    name: String,
    manifest_path: PathBuf,
}

fn cargo_package_root(package: &str) -> anyhow::Result<PathBuf> {
    let metadata = cargo_metadata()?;
    metadata
        .packages
        .into_iter()
        .find(|candidate| candidate.name == package)
        .and_then(|candidate| candidate.manifest_path.parent().map(Path::to_path_buf))
        .ok_or_else(|| anyhow::anyhow!("Cargo package {package:?} is not in this workspace"))
}

fn cargo_metadata() -> anyhow::Result<CargoMetadata> {
    let output = ProcessCommand::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()?;
    anyhow::ensure!(output.status.success(), "cargo metadata failed");
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn watch_snapshot(root: &Path) -> anyhow::Result<BTreeMap<PathBuf, (u64, u128)>> {
    let mut snapshot = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if relative
            .components()
            .any(|component| matches!(component.as_os_str().to_str(), Some("lib" | "node_modules")))
        {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        snapshot.insert(entry.path().to_owned(), (metadata.len(), modified));
    }
    Ok(snapshot)
}

fn classic_module_bundle(
    bindings: &str,
    wasm: &[u8],
    global: &str,
    module_id: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        is_javascript_identifier(global),
        "WASM global must be a JavaScript identifier"
    );
    let unique_declaration = format!("var {global} =");
    let bindings = if bindings.contains("let wasm_bindgen =") {
        bindings.replacen("let wasm_bindgen =", &unique_declaration, 1)
    } else {
        anyhow::ensure!(
            bindings.contains(&unique_declaration),
            "wasm-bindgen output omitted its expected global declaration"
        );
        bindings.to_owned()
    };
    let compatibility = compatibility_prelude(global, module_id);
    let factory = module_factory(global, module_id);
    let module_id = serde_json::to_string(module_id)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(wasm);
    Ok(format!(
        "{bindings}\n(() => {{\n  const binary = atob({encoded:?});\n  const bytes = Uint8Array.from(binary, value => value.charCodeAt(0));\n  {global}.initSync({{ module: bytes }});\n{compatibility}  window.__ModuleLoader__.load({{ id: {module_id}, factory: {factory} }});\n}})();\n"
    ))
}

fn compatibility_prelude(global: &str, module_id: &str) -> String {
    if module_id == "@seekdeep-ai/seekdeep-client-locale" {
        return format!(
            "  Object.assign({global}, {{ apply: {global}.applyClientLocale, inject: ['slots', 'connection', 'remote', 'settingsScope'] }});\n"
        );
    }
    if module_id != "@seekdeep-ai/seekdeep-client-runtime" {
        return String::new();
    }
    format!(
        "  class SessionCreateError extends Error {{ constructor(rpcError, requestedSessionId) {{ super(`session create failed: ${{rpcError.code}}: ${{rpcError.message}}`); this.name = 'SessionCreateError'; this.rpcError = rpcError; this.requestedSessionId = requestedSessionId; }} }}\n  class SessionForkError extends Error {{ constructor(rpcError, sourceSessionId) {{ super(`session fork failed: ${{rpcError.code}}: ${{rpcError.message}}`); this.name = 'SessionForkError'; this.rpcError = rpcError; this.sourceSessionId = sourceSessionId; }} }}\n  class WorkspaceCreateError extends Error {{ constructor(rpcError) {{ super(`workspace create failed: ${{rpcError.code}}: ${{rpcError.message}}`); this.name = 'WorkspaceCreateError'; this.rpcError = rpcError; }} }}\n  class DirectoryBrowseError extends Error {{ constructor(rpcError) {{ super(`directory browse failed: ${{rpcError.code}}: ${{rpcError.message}}`); this.name = 'DirectoryBrowseError'; this.rpcError = rpcError; }} }}\n  Object.assign({global}, {{ apply: {global}.applyClientRuntime, SlotRegistry: {global}.ClientSlotRegistry, SessionCreateError, SessionForkError, WorkspaceCreateError, DirectoryBrowseError, EMPTY_CHAT_SNAPSHOT: {global}.emptyChatSnapshot(), EMPTY_CONVERSATION_VIEWS: {global}.emptyConversationViews() }});\n"
    )
}

#[allow(clippy::too_many_lines)] // Closed module-specific factory dispatch stays auditable here.
fn module_factory(global: &str, module_id: &str) -> String {
    if module_id == "@seekdeep-ai/seekdeep-api-remotes" {
        return format!(
            "require => {{ {global}.configureApiRemotes([require('@seekdeep-ai/seekdeep-commands/remote'), require('@seekdeep-ai/seekdeep-goal/remote'), require('@seekdeep-ai/seekdeep-cordis-host-runner/remote'), require('@seekdeep-ai/seekdeep-host-plugin-inventory/remote'), require('@seekdeep-ai/seekdeep-message-feedback/remote')]); Object.assign({global}, {{ apply: {global}.applyApiRemotes, inject: ['remote'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-locale" {
        return format!(
            "require => {{ {global}.configureClientLocale(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives'), require('@seekdeep-ai/seekdeep-client-runtime/client')); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings" {
        return format!(
            "require => {{ const {{ Service }} = require('@seekdeep-ai/cordis'); class SettingsScopeBinder extends Service {{ constructor(ctx) {{ super(ctx, 'settingsScope'); }} bind(spec) {{ return {global}.bindSettingsScope(this.ctx, spec); }} }} {global}.configureClientUiSettings(SettingsScopeBinder); Object.assign({global}, {{ apply: {global}.applyClientUiSettings, inject: [], SettingsScopeBinder, SettingsScopeController: {global}.__SettingsScopeController }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings-general" {
        return format!(
            "require => {{ {global}.configureClientUiSettingsGeneral(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives'), require('@seekdeep-ai/seekdeep-client-web-react')); Object.assign({global}, {{ apply: {global}.applyClientUiSettingsGeneral, inject: ['slots', 'locale', 'connection'], SettingsDocumentStore: {global}.__SettingsDocumentStore }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings-plugin-inventory" {
        return format!(
            "require => {{ {global}.configureClientUiSettingsPluginInventory(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiSettingsPluginInventory, inject: ['slots', 'locale', 'remote', 'remote.pluginInventory'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-skill" {
        return format!(
            "require => {{ {global}.configureClientUiSkill(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiSkill, inject: ['inputTriggers', 'connection', 'sessions', 'slots', 'locale', 'remote'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-subagent" {
        return ui_subagent_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-permission-presets" {
        return ui_permission_presets_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-model-selection" {
        return ui_model_selection_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-input-trigger" {
        return ui_input_trigger_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-commands" {
        return ui_commands_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-message-feedback" {
        return format!(
            "require => {{ {global}.configureClientUiMessageFeedback(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiMessageFeedback, inject: ['slots', 'remote', 'remote.messageFeedback', 'locale'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-jobs" {
        return format!(
            "require => {{ {global}.configureClientUiJobs(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiJobs, inject: ['sessions', 'slots', 'locale'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-plan" {
        return format!(
            "require => {{ {global}.configureClientUiPlan(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiPlan, inject: ['slots', 'remote', 'remote.commands', 'locale'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-goal" {
        return format!(
            "require => {{ {global}.configureClientUiGoal(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiGoal, inject: ['slots', 'sessions', 'remote', 'remote.goals', 'locale', 'conversationEvents'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-deliverables" {
        return format!(
            "require => {{ {global}.configureClientUiDeliverables(require('react')); Object.assign({global}, {{ apply: {global}.applyClientUiDeliverables, inject: ['slots', 'locale', 'conversationEvents', 'connection'], ProducedFiles: {global}.producedFilesComponent() }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-directory-picker-native" {
        return format!(
            "require => {{ {global}.configureClientUiDirectoryPickerNative(require('react')); Object.assign({global}, {{ apply: {global}.applyClientUiDirectoryPickerNative, inject: ['slots', 'workspaces'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-trajectory" {
        return format!(
            "require => {{ {global}.configureClientUiTrajectoryModules(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); {global}.configureClientUiTrajectoryRuntime(require('@seekdeep-ai/seekdeep-client-runtime/client')); Object.assign({global}, {{ apply: {global}.applyClientUiTrajectory, inject: ['slots', 'conversationEvents', 'conversationViews', 'sessions', 'locale'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-user-questions" {
        return format!(
            "require => {{ {global}.configureClientUiUserQuestions(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiUserQuestions, inject: ['slots', 'locale'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-workflow-run" {
        return format!(
            "require => {{ {global}.configureClientUiWorkflowRun(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives'), require('@seekdeep-ai/seekdeep-client-runtime/client')); Object.assign({global}, {{ apply: {global}.applyClientUiWorkflowRun, inject: ['conversationEvents', 'slots', 'sessions', 'locale'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-layout" {
        return format!(
            "require => {{ {global}.configureClientUiLayout(require('react'), require('@seekdeep-ai/seekdeep-client-runtime/client')); Object.assign({global}, {{ apply: {global}.applyClientUiLayout, inject: ['slots', 'theme'] }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-theme" {
        return format!(
            "require => {{ {global}.configureClientUiTheme(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives'), require('@seekdeep-ai/seekdeep-client-runtime/client')); Object.assign({global}, {{ apply: {global}.applyClientUiTheme, inject: ['slots', 'locale', 'connection', 'remote', 'settingsScope'], SETTINGS_NS: 'settings.theme' }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-web-react" {
        return format!(
            "require => {{ const React = require('react'); {global}.configureClientWebReact(React, {global}.createSelectorShim(React)); const errors = {global}.webReactErrorClasses(); Object.assign({global}, errors, {{ createSlotRenderer: {global}.createSlotRenderer, SessionProvider: {global}.sessionProviderComponent(), bindSnapshotSelector: {global}.bindSnapshotSelector, useInvoke: {global}.useInvoke }}); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-sidebar" {
        return format!(
            "require => {{ {global}.configureClientUiSidebar(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiSidebar, inject: ['slots', 'layout', 'sessions', 'workspaces', 'locale'] }}); return {global}; }}"
        );
    }
    format!("() => {global}")
}

fn ui_subagent_module_factory(global: &str) -> String {
    format!(
        "require => {{ {global}.configureClientUiSubagent(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiSubagent, inject: ['inputTriggers', 'sessions', 'slots', 'locale'] }}); return {global}; }}"
    )
}

fn ui_permission_presets_module_factory(global: &str) -> String {
    format!(
        "require => {{ {global}.configureClientUiPermissionPresets(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiPermissionPresets, inject: ['commandUi', 'sessions', 'slots', 'locale', 'connection', 'remote'], PermissionPresetSettingsController: {global}.__PermissionPresetSettingsController }}); return {global}; }}"
    )
}

fn ui_model_selection_module_factory(global: &str) -> String {
    format!(
        "require => {{ {global}.configureClientUiModelSelection(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); class ModelDirectoryResolver {{ static inject = ['connection', 'sessions', 'remote']; constructor(ctx, config) {{ return {global}.createModelDirectoryResolver(ctx, config.blockReason); }} }} Object.assign({global}, {{ apply: {global}.applyClientUiModelSelection, inject: ['commandUi', 'connection', 'locale', 'sessions', 'slots', 'remote'], ModelDirectory: {global}.__ModelDirectory, ModelDirectoryResolver }}); return {global}; }}"
    )
}

fn ui_input_trigger_module_factory(global: &str) -> String {
    format!(
        "require => {{ {global}.configureClientUiInputTrigger(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); class InputTriggerService {{ static inject = ['sessions']; constructor(ctx) {{ return new {global}.__InputTriggerService(ctx); }} }} Object.assign({global}, {{ apply: {global}.applyClientUiInputTrigger, inject: ['sessions', 'locale'], InputTriggerService, InputTriggerController: {global}.__InputTriggerController }}); return {global}; }}"
    )
}

fn ui_commands_module_factory(global: &str) -> String {
    format!(
        "require => {{ {global}.configureClientUiCommands(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}.__CommandUiRuntime, {{ inject: ['inputTriggers', 'sessions', 'remote', 'remote.commands'] }}); Object.assign({global}, {{ apply: {global}.applyClientUiCommands, inject: ['inputTriggers', 'sessions', 'remote', 'remote.commands', 'locale'], CommandUiRuntime: {global}.__CommandUiRuntime, CommandDirectory: {global}.__CommandDirectory, PopupSelectController: {global}.__PopupSelectController, PopupSelectView: {global}.popupSelectViewComponent() }}); return {global}; }}"
    )
}

#[allow(clippy::too_many_lines)] // Closed module-specific declaration dispatch stays auditable here.
fn compatibility_declarations(module_id: &str) -> String {
    if module_id == "@seekdeep-ai/seekdeep-api-remotes" {
        return api_remotes_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-locale" {
        return "\nexport const apply: typeof wasm_bindgen.applyClientLocale;\nexport const inject: readonly ['slots', 'connection', 'remote', 'settingsScope'];\n".to_owned();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings" {
        return r"
import type { SettingsScope, SettingsScopeSpec } from '@seekdeep-ai/seekdeep-client-runtime/client';
export const apply: typeof wasm_bindgen.applyClientUiSettings;
export const inject: readonly [];
export const SettingsScopeController: {
  new <T>(api: unknown, spec: SettingsScopeSpec<T>, persistence?: 'host' | 'memory'): SettingsScope<T> & {
    load(): Promise<void>;
    dispose(): Promise<void>;
  };
};
export class SettingsScopeBinder {
  constructor(ctx: unknown);
  bind<T>(spec: SettingsScopeSpec<T>): SettingsScope<T>;
}
export interface SettingsGeneralItemOwnerProps { children?: never }
export interface SettingsPluginsTabOwnerProps { children?: never }
export interface SettingsTriggerOwnerProps { wide: boolean }
export interface SettingsHeaderOwnerProps { children?: never }
export interface SettingsSectionOwnerProps { close: () => void }
export interface SettingsOnboardingOwnerProps {
  stepId: string;
  complete: () => void;
  openSection: (id: string) => void;
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface SlotMap {
    'settings.trigger': { kind: 'single'; scope: 'root'; owner: SettingsTriggerOwnerProps };
    'settings.header': { kind: 'single'; scope: 'root'; owner: SettingsHeaderOwnerProps };
    'settings.action': { kind: 'list'; scope: 'root'; owner: SettingsHeaderOwnerProps };
    'settings.close': { kind: 'single'; scope: 'root'; owner: SettingsHeaderOwnerProps };
    'settings.section': { kind: 'list'; scope: 'root'; owner: SettingsSectionOwnerProps };
    'settings.plugins.tab': { kind: 'list'; scope: 'root'; owner: SettingsPluginsTabOwnerProps };
    'settings.onboarding': { kind: 'list'; scope: 'root'; owner: SettingsOnboardingOwnerProps };
    'settings.general.item': { kind: 'list'; scope: 'root'; owner: SettingsGeneralItemOwnerProps };
  }
}
"
        .to_owned();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings-general" {
        return r"
export const apply: typeof wasm_bindgen.applyClientUiSettingsGeneral;
export const inject: readonly ['slots', 'locale', 'connection'];
export type SettingsKey = 'trigger' | 'title' | 'close' | 'openDocument' | 'openDocument.error' | 'general.nav';
export interface SettingsDocumentState {
  status: 'idle' | 'loading' | 'ready' | 'unavailable';
  opening: boolean;
  error: string | null;
}
export interface SettingsDocumentSnapshotStore {
  getSnapshot(): SettingsDocumentState;
  subscribe(listener: () => void): () => void;
}
export const SettingsDocumentStore: {
  new (api: unknown): {
    readonly store: SettingsDocumentSnapshotStore;
    load(): Promise<void>;
    open(): Promise<void>;
  };
};
export interface SettingsSectionRow { id: string; order: number; label: string }
export interface SettingsOnboardingStep { id: string; order: number }
export interface TriggerContentProps { wide: boolean; t(key: string): string }
export interface HeaderContentProps { t(key: string): string }
export interface CloseLabelProps { t(key: string): string }
export interface GeneralSectionComponentProps { renderSlot: Function; close(): void }
export interface SettingsDocumentActionInjected { controller: InstanceType<typeof SettingsDocumentStore>; useSnapshot: Function }
export type SettingsDocumentActionProps = SettingsDocumentActionInjected & { t(key: string): string };
"
        .to_owned();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings-plugin-inventory" {
        return ui_settings_plugin_inventory_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-skill" {
        return ui_skill_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-subagent" {
        return ui_subagent_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-permission-presets" {
        return ui_permission_presets_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-model-selection" {
        return ui_model_selection_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-input-trigger" {
        return ui_input_trigger_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-commands" {
        return ui_commands_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-message-feedback" {
        return ui_message_feedback_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-jobs" {
        return ui_jobs_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-plan" {
        return ui_plan_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-goal" {
        return ui_goal_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-deliverables" {
        return ui_deliverables_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-directory-picker-native" {
        return ui_directory_picker_native_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-trajectory" {
        return ui_trajectory_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-user-questions" {
        return ui_user_questions_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-workflow-run" {
        return ui_workflow_run_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-layout" {
        return ui_layout_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-theme" {
        return ui_theme_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-web-react" {
        return client_web_react_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-sidebar" {
        return ui_sidebar_declarations();
    }
    if module_id != "@seekdeep-ai/seekdeep-client-runtime" {
        return String::new();
    }
    "\nexport const apply: typeof wasm_bindgen.applyClientRuntime;\nexport const isAppendSurfaceEvent: typeof wasm_bindgen.isAppendSurfaceEvent;\nexport const isReplacementSurfaceEvent: typeof wasm_bindgen.isReplacementSurfaceEvent;\nexport const SlotRegistry: typeof wasm_bindgen.ClientSlotRegistry;\nexport const ConversationEventRegistry: typeof wasm_bindgen.ConversationEventRegistry;\nexport const ConversationViewRegistry: typeof wasm_bindgen.ConversationViewRegistry;\nexport const ConversationNodeAssembler: typeof wasm_bindgen.ConversationNodeAssembler;\nexport const ConversationLocationIndex: typeof wasm_bindgen.ConversationLocationIndex;\nexport const conversationContextKey: typeof wasm_bindgen.conversationContextKey;\nexport const SessionRuntime: typeof wasm_bindgen.SessionRuntime;\nexport const scopeOf: typeof wasm_bindgen.scopeOf;\nexport const workspaceTitleOf: typeof wasm_bindgen.workspaceTitleOf;\nexport const indexSubagentDescendants: typeof wasm_bindgen.indexSubagentDescendants;\nexport const SessionProvideChannel: typeof wasm_bindgen.SessionProvideChannel;\nexport const createScope: typeof wasm_bindgen.createScope;\nexport const WorkspaceRuntime: typeof wasm_bindgen.WorkspaceRuntime;\nexport const resolveWorkspacePath: typeof wasm_bindgen.resolveWorkspacePath;\nexport const createSnapshotStore: typeof wasm_bindgen.createSnapshotStore;\nexport const defineStore: typeof wasm_bindgen.defineStore;\nexport const shallowEqual: typeof wasm_bindgen.shallowEqual;\nexport const toAssistantBlock: typeof wasm_bindgen.toAssistantBlock;\nexport const toAssistantBlocks: typeof wasm_bindgen.toAssistantBlocks;\nexport const emptyAssistantBlock: typeof wasm_bindgen.emptyAssistantBlock;\nexport const isTokenDelta: typeof wasm_bindgen.isTokenDelta;\nexport const contextForm: typeof wasm_bindgen.contextForm;\nexport const contextProvenance: typeof wasm_bindgen.contextProvenance;\nexport const displayFailureMessage: typeof wasm_bindgen.displayFailureMessage;\nexport const PendingWait: typeof wasm_bindgen.PendingWait;\nexport class SessionCreateError extends Error { constructor(rpcError: any, requestedSessionId: string | undefined); readonly rpcError: any; readonly requestedSessionId: string | undefined; }\nexport class SessionForkError extends Error { constructor(rpcError: any, sourceSessionId: string); readonly rpcError: any; readonly sourceSessionId: string; }\nexport class WorkspaceCreateError extends Error { constructor(rpcError: any); readonly rpcError: any; }\nexport class DirectoryBrowseError extends Error { constructor(rpcError: any); readonly rpcError: any; }\nexport const EMPTY_CHAT_SNAPSHOT: ReturnType<typeof wasm_bindgen.emptyChatSnapshot>;\nexport const EMPTY_CONVERSATION_VIEWS: ReturnType<typeof wasm_bindgen.emptyConversationViews>;\n".to_owned()
        + runtime_settings_contract_declarations()
}

fn ui_skill_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiSkill;
export const inject: readonly ['inputTriggers', 'connection', 'sessions', 'slots', 'locale', 'remote'];
export type SkillKey =
  | 'row.running' | 'row.failed' | 'row.stopped' | 'row.instructions' | 'menu.userOnly';
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { skill: SkillKey }
}
"
    .to_owned()
}

fn ui_subagent_declarations() -> String {
    r"
import type { SessionId, SubagentAddress } from '@seekdeep-ai/seekdeep-client-runtime/client';
import type { PropsLocale, PropsRuntime } from '@seekdeep-ai/seekdeep-client-ui-slots';
export const apply: typeof wasm_bindgen.applyClientUiSubagent;
export const inject: readonly ['inputTriggers', 'sessions', 'slots', 'locale'];
export type SubagentKey =
  | 'diagnostic.corrupt' | 'diagnostic.unsupported' | 'diagnostic.unavailable'
  | 'duration.seconds' | 'duration.minutes' | 'duration.hours' | 'duration.days'
  | 'duration.daysHours' | 'duration.months' | 'duration.monthsDays' | 'duration.years'
  | 'duration.yearsMonths' | 'duration.exactDays' | 'duration.exactTitle'
  | 'loading.label' | 'loading.aria' | 'load.error' | 'retry' | 'mode.oneShot'
  | 'mode.continuable' | 'activity.running' | 'activity.inactive' | 'branch.collapse'
  | 'branch.expand' | 'count.total.one' | 'count.total.other' | 'count.running.one'
  | 'count.running.other' | 'tree.aria' | 'readonly.oneShot.title' | 'readonly.title'
  | 'readonly.oneShot.body' | 'readonly.body';
export interface SubagentCatalogInjected {
  openChild: (address: SubagentAddress) => void;
  refresh: (parentSessionId: SessionId) => void;
  setCatalogOpen: (parentSessionId: SessionId, open: boolean) => void;
}
export type SubagentCatalogActionProps =
  PropsRuntime<'conversation.session.header.actions'> & SubagentCatalogInjected & PropsLocale<'subagent'>;
export interface SubagentReadOnlyMatch { reason: 'one-shot' | 'parent-unavailable' }
export type SubagentReadOnlyComposerProps =
  PropsRuntime<'conversation.composer'> & { matched: SubagentReadOnlyMatch } & PropsLocale<'subagent'>;
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { subagent: SubagentKey }
}
"
    .to_owned()
}

fn ui_permission_presets_declarations() -> String {
    r"
import type { InjectFace, PropsLocale, PropsRuntime } from '@seekdeep-ai/seekdeep-client-ui-slots';
export const apply: typeof wasm_bindgen.applyClientUiPermissionPresets;
export const inject: readonly ['commandUi', 'sessions', 'slots', 'locale', 'connection', 'remote'];
export type PermissionSettingsKey =
  | 'title' | 'description' | 'loading' | 'unavailable' | 'confirm.title'
  | 'confirm.description' | 'confirm.acknowledge' | 'confirm.cancel' | 'confirm.enable';
export interface PermissionDefaultOption { id: string; label: string }
export interface PermissionSettingsState {
  status: 'idle' | 'loading' | 'ready' | 'saving' | 'unavailable' | 'error';
  error: string | null;
  writable: boolean;
  currentValue: string;
  options: readonly PermissionDefaultOption[];
  revision: number;
}
export interface PermissionSnapshotStore {
  getSnapshot(): PermissionSettingsState;
  subscribe(listener: () => void): () => void;
}
export const PermissionPresetSettingsController: {
  new (api: unknown): {
    readonly store: PermissionSnapshotStore;
    load(): Promise<void>;
    select(preset: string): Promise<void>;
    dispose(): void;
  };
};
export const permissionDefaultOf: (view: unknown) => {
  currentValue: string;
  options: PermissionDefaultOption[];
};
export const refreshPermissionIfLoaded:
  (controller: InstanceType<typeof PermissionPresetSettingsController>) => void;
export interface PermissionRowInjected {
  hooks: { permission: PermissionSnapshotStore };
  load: () => Promise<void>;
  select: (preset: string) => Promise<void>;
}
export type PermissionRowProps = PropsRuntime<'settings.general.item'>
  & PropsLocale<'settings.permission'> & InjectFace<PermissionRowInjected>;
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { 'settings.permission': PermissionSettingsKey }
}
"
    .to_owned()
}

fn ui_model_selection_declarations() -> String {
    r"
import type { ModelCatalogFailure, ModelProviderGroup, ModelSelection, SessionId, SessionModels } from '@seekdeep-ai/seekdeep-api-remotes/client';
export const apply: typeof wasm_bindgen.applyClientUiModelSelection;
export const inject: readonly ['commandUi', 'connection', 'locale', 'sessions', 'slots', 'remote'];
export type ModelKey =
  | 'command.description' | 'option.loadError' | 'trigger.fallback' | 'trigger.selectAria'
  | 'trigger.aria' | 'trigger.ariaEffort' | 'menu.aria' | 'menu.model' | 'menu.effort'
  | 'effort.providerDefault' | 'status.loading' | 'error.action' | 'action.reload'
  | 'warning.groupLoad' | 'empty.models' | 'blocked.composer' | 'empty.efforts';
export interface ModelDirectoryState {
  current: ModelSelection | null;
  routable: boolean | null;
  groups: readonly ModelProviderGroup[];
  failures: readonly ModelCatalogFailure[];
  status: 'idle' | 'loading' | 'ready' | 'selecting' | 'error';
  error: string | null;
}
export interface ModelDirectoryStore {
  getSnapshot(): ModelDirectoryState;
  subscribe(listener: () => void): () => void;
}
export const ModelDirectory: {
  new (sessions: unknown, sessionId: SessionId, available: () => boolean): {
    readonly store: ModelDirectoryStore;
    load(): Promise<SessionModels>;
    select(selection: ModelSelection): Promise<void>;
    resetConnected(): void;
    dispose(): void;
  };
};
export class ModelDirectoryResolver {
  static readonly inject: readonly ['connection', 'sessions', 'remote'];
  constructor(ctx: unknown, config: { blockReason: () => string });
  directoryFor(sessionId: SessionId): InstanceType<typeof ModelDirectory>;
}
export interface ModelSelectInjected {
  available: boolean;
  directory: ModelDirectoryStore;
  load: () => void;
  select: (selection: ModelSelection) => Promise<boolean>;
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { model: ModelKey }
}
"
    .to_owned()
}

fn ui_input_trigger_declarations() -> String {
    r"
import type { ClientContext, SessionId } from '@seekdeep-ai/seekdeep-client-runtime/client';
export const apply: typeof wasm_bindgen.applyClientUiInputTrigger;
export const inject: readonly ['sessions', 'locale'];
export type TriggerChar = '/' | '@';
export type TriggerPosition = 'leading' | 'inline';
export type PickVia = 'menu' | 'space' | 'enter';
export type ArbitrateKey = 'up' | 'down' | 'enter' | 'escape';
export type ArbitrateOutcome = 'consumed' | 'pick-highlighted' | 'pass';
export interface ClientSessionContext { readonly sessionId: SessionId }
export interface InputTriggerCandidate {
  readonly name: string;
  readonly description?: string;
  readonly icon?: string;
  readonly hint?: string;
}
export interface TokenSpan { readonly start: number; readonly end: number; readonly draftRev: number }
export interface TriggerGuard { readonly tier: 'plain' | 'claimed' | 'frozen' }
export interface TriggerHit { trigger: TriggerChar; query: string; position: TriggerPosition; span: TokenSpan }
export interface ReferenceInsert {
  readonly source: string;
  readonly ref: string;
  readonly label: string;
  readonly clipboardText: string;
}
export interface CommandClaim {
  readonly token: string;
  readonly hint?: string;
  submit(args: string, actx: ClientContext): Promise<unknown>;
}
export type PickOutcome = { readonly claim: CommandClaim } | { readonly insert: ReferenceInsert }
  | { readonly text: string } | 'handled' | undefined;
export interface InputTriggerSource {
  readonly trigger: TriggerChar;
  readonly name: string;
  readonly order?: number;
  candidates(session: ClientSessionContext, request: { query: string; position: TriggerPosition; signal: AbortSignal }): Promise<readonly InputTriggerCandidate[]>;
  onPick(input: { candidate: InputTriggerCandidate; session: ClientSessionContext; position: TriggerPosition; via: PickVia; span: TokenSpan }): PickOutcome;
  matchSpace?(session: ClientSessionContext, token: string): PickOutcome;
  matchEnter?(session: ClientSessionContext, line: string, signal: AbortSignal): Promise<PickOutcome>;
  warm?(session: ClientSessionContext): void;
  lexicon?(session: ClientSessionContext): readonly string[] | undefined;
  subscribeLexicon?(session: ClientSessionContext, listener: () => void): () => void;
  readonly codec?: { clipboardText(ref: string): string; serialize(ref: string, signal: AbortSignal): Promise<string> };
}
export interface MenuState {
  open: boolean;
  hit: TriggerHit | null;
  generation: number;
  groups: readonly { source: string; status: 'pending' | 'ready'; items: readonly InputTriggerCandidate[] }[];
  highlight: { source: string; index: number } | null;
}
export interface SnapshotStore<T> { getSnapshot(): T; subscribe(listener: () => void): () => void }
export const InputTriggerController: {
  new (...args: never[]): {
    readonly menu: SnapshotStore<MenuState>;
    readonly launcher: SnapshotStore<string | null>;
    readonly lexicon: SnapshotStore<ReadonlyMap<TriggerChar, readonly string[]>>;
    track(draft: string, caret: number, guard: TriggerGuard, draftRev: number): void;
    toggleSource(source: string, hit: TriggerHit): void;
    pick(source: string, index: number): void;
    arbitrate(key: ArbitrateKey, composing: boolean): ArbitrateOutcome;
    onSpace(): boolean;
    serializeReference(source: string, ref: string, signal: AbortSignal): Promise<string>;
    adjudicate(line: string, signal: AbortSignal): Promise<PickOutcome>;
    dismiss(): void;
    dispose(): void;
  };
};
export class InputTriggerService {
  static readonly inject: readonly ['sessions'];
  constructor(ctx: unknown);
  registerSource(source: InputTriggerSource): () => void;
  sessionOf(actx: ClientContext): InstanceType<typeof InputTriggerController>;
}
export interface MenuViewInjected {
  menu: SnapshotStore<MenuState>;
  onPick: (source: string, index: number) => void;
  onDismiss: () => void;
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { 'slash.menu': 'command' | 'skill' | 'subagent' | 'loading' | 'suggestions.aria' }
  interface SlotMap { 'conversation.input.overlay': { kind: 'list'; scope: 'session' } }
}
"
    .to_owned()
}

fn ui_commands_declarations() -> String {
    r"
import type { ClientContext, SessionId } from '@seekdeep-ai/seekdeep-client-runtime/client';
import type { ClientSessionContext, SnapshotStore, TokenSpan } from '@seekdeep-ai/seekdeep-client-ui-input-trigger/client';
import type { CommandResult } from '@seekdeep-ai/seekdeep-commands/types';
export const apply: typeof wasm_bindgen.applyClientUiCommands;
export const inject: readonly ['inputTriggers', 'sessions', 'remote', 'remote.commands', 'locale'];
export type DirectoryStatus = 'cold' | 'pending' | 'ready' | 'failed';
export interface CommandDescriptor {
  readonly name: string;
  readonly description: string;
  readonly input?: { readonly hint: string };
}
export class CommandDirectory {
  constructor(fetchCommands: (sessionId: SessionId) => Promise<readonly CommandDescriptor[]>);
  status(sessionId: SessionId): DirectoryStatus;
  resolve(sessionId: SessionId, name: string): CommandDescriptor | undefined;
  invalidateAll(): void;
  resetConnected(): void;
  warm(sessionId: SessionId): void;
  refresh(sessionId: SessionId): Promise<void>;
  ensureReady(sessionId: SessionId, signal: AbortSignal): Promise<readonly CommandDescriptor[]>;
}
export interface SelectConfirmation {
  readonly title: string;
  readonly description: string;
  readonly acknowledgeLabel: string;
  readonly cancelLabel: string;
  readonly confirmLabel: string;
}
export interface SelectOption {
  readonly id: string;
  readonly label: string;
  readonly detail?: string;
  readonly active?: boolean;
  readonly confirmation?: SelectConfirmation;
}
export type TokenSegment =
  | { readonly via: 'menu'; readonly span: TokenSpan }
  | { readonly via: 'enter'; readonly token: string };
export interface PopupSpec<TCtx> {
  options(context: TCtx, signal: AbortSignal): Promise<readonly SelectOption[]>;
  onSelect(option: SelectOption, context: TCtx): void | Promise<void>;
}
export interface PopupSelectDeps {
  consume(segment: TokenSegment): boolean;
  focusComposer(): void;
}
export interface PopupState {
  readonly open: boolean;
  readonly command: string | null;
  readonly status: 'pending' | 'ready' | 'failed';
  readonly options: readonly SelectOption[];
  readonly search: string;
  readonly active: number;
  readonly submitting: boolean;
  readonly confirming: SelectOption | null;
  readonly acknowledged: boolean;
  readonly error: string | null;
}
export class PopupSelectController<TCtx = unknown> {
  constructor(deps: PopupSelectDeps);
  readonly state: SnapshotStore<PopupState>;
  open(command: string, spec: PopupSpec<TCtx>, context: TCtx, segment: TokenSegment): void;
  retry(): void;
  setSearch(search: string): void;
  move(direction: 1 | -1): void;
  highlight(index: number): void;
  select(index: number): Promise<void>;
  acknowledge(acknowledged: boolean): void;
  cancelConfirmation(): void;
  confirm(): Promise<void>;
  dismiss(options?: { readonly focusComposer?: boolean }): void;
  dispose(): void;
}
export type CommandUiSpec = { readonly kind: 'popupSelect' } & PopupSpec<ClientSessionContext>;
export interface CommandContribution {
  readonly name: string;
  readonly description: string;
  available(session: ClientSessionContext): boolean;
  readonly ui: CommandUiSpec;
}
export interface CommandDecoration {
  readonly name: string;
  available(session: ClientSessionContext): boolean;
  readonly ui: CommandUiSpec;
}
export class CommandUiRuntime {
  static readonly inject: readonly ['inputTriggers', 'sessions', 'remote', 'remote.commands'];
  constructor(ctx: ClientContext);
  register(contribution: CommandContribution): () => void;
  decorate(decoration: CommandDecoration): () => void;
  popupFor(actx: ClientContext): PopupSelectController<ClientSessionContext>;
  bindComposerFocus(id: SessionId, focus: () => void): () => void;
}
export const filterOptions: (options: readonly SelectOption[], search: string) => readonly SelectOption[];
export interface PopupSelectInjected { popup: PopupSelectController<ClientSessionContext> }
export interface PopupSelectViewProps extends PopupSelectInjected { t(key: CommandKey, values?: Record<string, unknown>): string }
export const PopupSelectView: (props: PopupSelectViewProps) => import('react').JSX.Element | null;
export type CommandKey =
  | 'search.placeholder' | 'search.aria' | 'status.loading' | 'status.applying'
  | 'status.empty' | 'overlay.aria' | 'listbox.aria';
declare module '@seekdeep-ai/cordis' {
  interface Context { commandUi: CommandUiRuntime }
  interface Events { 'command/executed'(sessionId: SessionId, name: string, result: CommandResult): void }
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { command: CommandKey }
}
"
    .to_owned()
}

fn ui_settings_plugin_inventory_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiSettingsPluginInventory;
export const inject: readonly ['slots', 'locale', 'remote', 'remote.pluginInventory'];
export type PluginInventoryLocaleKey =
  | 'tab' | 'loading' | 'error' | 'retry' | 'search' | 'catalog' | 'empty'
  | 'emptySearch' | 'enabledTag' | 'disabledTag' | 'configuration' | 'cordis'
  | 'unobserved' | 'pending' | 'loadingPhase' | 'active' | 'failed' | 'unloading';
export type PluginFiberPhase = 'pending' | 'loading' | 'active' | 'failed' | 'unloading';
export interface PluginInventoryEntry {
  entryId: string;
  moduleName: string;
  enabled: boolean;
  fiberPhase: PluginFiberPhase | null;
}
export interface PluginInventorySnapshot { entries: PluginInventoryEntry[] }
export interface PluginInventorySettingsTabInjected {
  list(): Promise<PluginInventorySnapshot>;
}
export interface PluginInventorySettingsTabProps extends PluginInventorySettingsTabInjected {
  t(key: PluginInventoryLocaleKey): string;
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { 'settings.pluginInventory': PluginInventoryLocaleKey }
}
"
    .to_owned()
}

fn ui_message_feedback_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiMessageFeedback;
export const inject: readonly ['slots', 'remote', 'remote.messageFeedback', 'locale'];
export type MessageFeedbackStatus = 'cold' | 'loading' | 'ready' | 'error';
export type MessageFeedbackRating = 'positive' | 'negative';
export interface MessageFeedbackItem {
  messageId: string;
  rating: MessageFeedbackRating;
  note?: string;
  version: string;
  createdAt: number;
  updatedAt: number;
export interface MessageFeedbackView {
  status: MessageFeedbackStatus;
  items: ReadonlyMap<string, MessageFeedbackItem>;
  error: string | null;
}
export type MessageFeedbackActionResult =
  | { readonly ok: true }
  | { readonly ok: false; readonly error: { readonly code: string; readonly message: string } };
export interface MessageFeedbackObservable {
  getSnapshot(): MessageFeedbackView;
  subscribe(listener: () => void): () => void;
}
export interface MessageFeedbackInjected {
  hooks: { feedback: MessageFeedbackObservable };
  ensure(): Promise<MessageFeedbackActionResult>;
  rate(messageId: string, rating: MessageFeedbackRating, note?: string): Promise<MessageFeedbackActionResult>;
  toggle(messageId: string, rating: MessageFeedbackRating): Promise<MessageFeedbackActionResult>;
  clearNote(messageId: string): Promise<MessageFeedbackActionResult>;
  clear(messageId: string): Promise<MessageFeedbackActionResult>;
}
export type MessageFeedbackKey =
  | 'action.like' | 'action.likeActive' | 'action.dislike' | 'action.dislikeActive'
  | 'note.open' | 'note.placeholder' | 'note.save' | 'note.cancel' | 'note.aria'
  | 'error.conflict' | 'error.load' | 'error.generic';
"
    .to_owned()
}

fn ui_directory_picker_native_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiDirectoryPickerNative;
export const inject: readonly ['slots', 'workspaces'];
export interface NativeFlowInjected { pick(): Promise<string | null> }
"
    .to_owned()
}

fn ui_plan_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiPlan;
export const inject: readonly ['slots', 'remote', 'remote.commands', 'locale'];
export type PlanKey = 'chip.on.aria' | 'chip.on.title' | 'chip.off.aria' | 'chip.off.title';
export interface PlanChipInjected {
  exitPlanMode(): Promise<string | null>;
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { plan: PlanKey }
}
"
    .to_owned()
}

fn ui_jobs_declarations() -> String {
    r"
import type { SessionId, SessionListState } from '@seekdeep-ai/seekdeep-client-runtime/client';
export const apply: typeof wasm_bindgen.applyClientUiJobs;
export const inject: readonly ['sessions', 'slots', 'locale'];
export type JobKey =
  | 'count.live.one' | 'count.live.other' | 'count.idle.one' | 'count.idle.other'
  | 'list.aria' | 'status.running' | 'status.stopping' | 'status.completed'
  | 'status.killed' | 'status.failed' | 'duration.seconds' | 'duration.minutes'
  | 'duration.hours' | 'duration.title.live' | 'duration.title.done';
export interface JobListActionProps {
  sessionId: SessionId;
  useSessions<T>(selector: (state: SessionListState) => T): T;
  t(key: JobKey, values?: Record<string, string | number>): string;
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { job: JobKey }
}
"
    .to_owned()
}

fn ui_goal_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiGoal;
export const inject: readonly ['slots', 'sessions', 'remote', 'remote.goals', 'locale', 'conversationEvents'];
export interface GoalActionError { code: string; message: string; details: unknown }
export type GoalActionResult = { ok: true; value?: unknown } | { ok: false; error: GoalActionError };
export interface GoalBarActions {
  onEdit(objective: string): Promise<GoalActionResult>;
  onPause(): Promise<GoalActionResult>;
  onResume(): Promise<GoalActionResult>;
  onClear(): Promise<GoalActionResult>;
}
"
    .to_owned()
}

fn ui_deliverables_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiDeliverables;
export const inject: readonly ['slots', 'locale', 'conversationEvents', 'connection'];
export interface ProducedPath { readonly seq: number; readonly path: string }
export interface DeliverablesTurnData { readonly produced: readonly ProducedPath[] }
export interface ProducedFilesInjected {
  isLoopback: boolean;
  hooks: { hostDescription: unknown };
}
export interface ProducedFilesProps {
  matched: readonly string[];
  openFile(path: string): void;
  isLoopback: boolean;
  useHostDescription<T>(selector: (description: unknown) => T): T;
  t(key: DeliverablesKey, values?: Record<string, string>): string;
}
export const ProducedFiles: (props: ProducedFilesProps) => import('react').JSX.Element;
export const producedForClosing: (
  data: Readonly<DeliverablesTurnData> | undefined,
  seq?: number,
) => readonly string[];
export type DeliverablesKey =
  | 'produced.label' | 'produced.moreOne' | 'produced.more'
  | 'produced.open' | 'produced.showInFolder';
declare module '@seekdeep-ai/seekdeep-client-runtime/client' {
  interface ConversationTurnDataMap { deliverables: DeliverablesTurnData }
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { deliverables: DeliverablesKey }
}
"
    .to_owned()
}

fn ui_trajectory_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiTrajectory;
export const inject: readonly ['slots', 'conversationEvents', 'conversationViews', 'sessions', 'locale'];
export interface TrajectoryViewInjected {
  hooks: { duration: unknown };
  loadOlder(): Promise<boolean>;
  setActualDuration(actualDuration: boolean): void;
}
export type TrajectoryTimelineMode = 'sequence' | 'duration' | 'time' | 'actual';
export interface TrajectoryTimeRange { start: number; end: number }
"
    .to_owned()
}

fn api_remotes_declarations() -> String {
    r"
import type { TypertClientRemote } from '@seekdeep-ai/seekdeep-typert-protocol';
export const apply: typeof wasm_bindgen.applyApiRemotes;
export const inject: readonly ['remote'];
export type { TypertClientRemote as ClientRemote } from '@seekdeep-ai/seekdeep-typert-protocol';
export type { PluginInventorySnapshot } from '@seekdeep-ai/seekdeep-host-plugin-inventory/types';
export type { ApiRemoteForwardedEvent } from '@seekdeep-ai/seekdeep-api-remotes/types';
export type {} from '@seekdeep-ai/seekdeep-commands/remote';
export type {} from '@seekdeep-ai/seekdeep-goal/remote';
export type {} from '@seekdeep-ai/seekdeep-host-plugin-inventory/remote';
export type {} from '@seekdeep-ai/seekdeep-message-feedback/remote';
export type {} from '@seekdeep-ai/seekdeep-commands/types';
export type {} from '@seekdeep-ai/seekdeep-credentials/types';
export type {} from '@seekdeep-ai/seekdeep-llm/types';
export type {} from '@seekdeep-ai/seekdeep-agent-presets/types';
export type {} from '@seekdeep-ai/seekdeep-settings/types';
export type {} from '@seekdeep-ai/seekdeep-api-gateway/client';
export type {} from '@seekdeep-ai/seekdeep-cordis-host-runner/remote';
export type {
  ClientResponse, ConfigurableProviderView, ConnectionHandle, ConnectionSinks, ContentBlock,
  CredentialView, DirectoryListing, DiscoveredModelView, HistoryEntry, HostFrame, IApiClient,
  MessageId, ModelCatalogFailure, ModelProviderGroup, ModelReasoningEffort, ModelSelection,
  MuxFrame, PromptContentPart, QuestionResponsePayload, QueueAction, RpcError, RpcId, RpcReceipt,
  RpcRequest, RpcResponse, RpcResult, SessionId, SessionModels, SessionSearchItem, SessionSummary,
  SettingsNamespaceView, SettingsPathOpView, SkillEntry, StreamChunk, SubagentAddress,
  SubagentCatalog, JobView, ToolCallView, ToolEventView, ToolResultView, WorkspaceId, WorkspaceView,
} from '@seekdeep-ai/seekdeep-client-connection/client';
export type {
  ApprovalRequestId, CordisHalfState, CordisDynamicPackageId, CordisDynamicPluginId,
  CordisDynamicPluginRunId, CordisDynamicRunMode, CordisInspectMethodManifest,
  CordisInspectPlatform, CordisInspectProviderManifest, CordisInspectProviderView,
  CordisInspectQueryRequest, CordisInspectQueryResolution, CordisInspectQueryResolved,
  CordisInspectRequestId, CordisInspectResolveAck, CordisRunDiagnostic, CordisRunStatus,
  DynamicCordisClientSource, DynamicCordisHostHalfResult, DynamicCordisInventoryRow,
  DynamicCordisInvokeResult, DynamicCordisPackage, DynamicCordisRequestResolved,
  DynamicCordisResolveAck, DynamicCordisRetracted, DynamicCordisRunRequest,
  DynamicCordisRunResolution, DynamicCordisRunAttempt, DynamicCordisRunResponse,
  DynamicCordisStopResponse, DynamicCordisUndefineReceipt, RequestRunOutcome,
} from '@seekdeep-ai/seekdeep-cordis-host-runner/types';
export type { JsonValue } from '@seekdeep-ai/seekdeep-session/types';
declare module '@seekdeep-ai/cordis' {
  interface Context { remote: TypertClientRemote }
}
"
    .to_owned()
}

fn ui_sidebar_declarations() -> String {
    r"
import type { WorkspaceId } from '@seekdeep-ai/seekdeep-client-runtime/client';
export const apply: typeof wasm_bindgen.applyClientUiSidebar;
export const inject: readonly ['slots', 'layout', 'sessions', 'workspaces', 'locale'];
export type SidebarKey = 'session.new' | 'session.new.label' | 'toggle.open' | 'toggle.collapse';
export interface SidebarSectionOwnerProps { wide: boolean; expandSidebar(): void }
export interface SidebarSettingsOwnerProps { wide: boolean }
export interface SidebarFooterActionOwnerProps { wide: boolean }
export interface SidebarRootInjected { startSession(workspaceId?: WorkspaceId): void; toggleSidebar(): void }
export interface SidebarRootComponentProps extends SidebarRootInjected {
  collapsed: boolean;
  width: number;
  useSessions: Function;
  useWorkspaces: Function;
  t(key: SidebarKey | string): string;
  renderSlot(name: string, owner: unknown): unknown;
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface SlotMap {
    'sidebar.workspaces': { kind: 'single'; scope: 'root'; owner: SidebarSectionOwnerProps };
    'sidebar.settings': { kind: 'single'; scope: 'root'; owner: SidebarSettingsOwnerProps };
    'sidebar.footer.action': { kind: 'list'; scope: 'root'; owner: SidebarFooterActionOwnerProps };
  }
}
"
    .to_owned()
}

fn ui_user_questions_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiUserQuestions;
export const inject: readonly ['slots', 'locale'];
export interface QuestionComposerProps { matched: unknown; t(key: string): string }
export interface QuestionAnswerItem { id: string; selected: string[]; custom?: string }
export interface QuestionAnswer { answers: QuestionAnswerItem[] }
export interface PlanReview {
  id: string;
  question: string;
  plan: string;
  approve: { label: string; description?: string };
  decline?: { label: string; description?: string };
}
"
    .to_owned()
}

fn ui_workflow_run_declarations() -> String {
    r"
import type { SessionId } from '@seekdeep-ai/seekdeep-client-runtime/client';
export const apply: typeof wasm_bindgen.applyClientUiWorkflowRun;
export const inject: readonly ['conversationEvents', 'slots', 'sessions', 'locale'];
export type WorkflowRunStatus = 'running' | 'completed' | 'failed' | 'cancelled' | 'interrupted';
export interface WorkflowRunMemberData {
  readonly seq: number;
  readonly label: string;
  readonly childId: SessionId;
  readonly status: WorkflowRunStatus;
}
export interface WorkflowRunPhaseData {
  readonly key: string;
  readonly phase: string | null;
  readonly members: readonly WorkflowRunMemberData[];
}
export interface WorkflowRunChatData {
  readonly name: string;
  readonly status: WorkflowRunStatus;
  readonly phases: readonly WorkflowRunPhaseData[];
}
export type WorkflowRunKey =
  | 'run.title' | 'run.members.one' | 'run.members.other' | 'run.empty'
  | 'phase.unassigned' | 'phase.empty' | 'statusCount.running'
  | 'statusCount.completed' | 'statusCount.failed' | 'statusCount.cancelled'
  | 'statusCount.interrupted' | 'member.empty' | 'member.open'
  | 'status.running' | 'status.completed' | 'status.failed'
  | 'status.cancelled' | 'status.interrupted';
declare module '@seekdeep-ai/seekdeep-client-ui-conversation/client' {
  interface ChatNodeDataMap { 'workflow-run': WorkflowRunChatData }
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { workflowRun: WorkflowRunKey }
}
"
    .to_owned()
}

fn ui_layout_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiLayout;
export const inject: readonly ['slots', 'theme'];
export const LayoutController: typeof wasm_bindgen.LayoutController;
export interface ILayout {
  toggleSidebar(): void;
  openDetails(): void;
  closeDetails(): void;
}

export interface SidebarOwnerProps { collapsed: boolean; width: number }
export interface ConvOwnerProps {}
export interface DetailsOwnerProps {}
declare module '@seekdeep-ai/cordis' {
  interface Context { layout: ILayout }
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface SlotMap {
    sidebar: { kind: 'single'; scope: 'root'; owner: SidebarOwnerProps };
    conversation: { kind: 'single'; scope: 'session-maybe'; owner: ConvOwnerProps };
    details: { kind: 'single'; scope: 'session'; owner: DetailsOwnerProps };
    'shell.overlay': { kind: 'list'; scope: 'root' };
  }
}
"
    .to_owned()
}

fn ui_theme_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiTheme;
export const inject: readonly ['slots', 'locale', 'connection', 'remote', 'settingsScope'];
export const SETTINGS_NS: 'settings.theme';
export const ThemeRuntime: typeof wasm_bindgen.ThemeRuntime;
export type ThemePreference = 'light' | 'dark' | 'system';
export interface ThemeSettings { preference: ThemePreference }
export type ThemeKey = 'appearance.title' | 'appearance.light' | 'appearance.dark' | 'appearance.system';
export interface AppearanceRowState { preference: ThemePreference; revision: number }
export interface AppearanceRowInjected { setTheme(id: ThemePreference): void }
export interface AppearanceRowComponentProps extends AppearanceRowInjected {
  useStore(selector: Function): unknown;
  t(key: ThemeKey | string): string;
}
export type ThemeTokens = Record<string, string>;
export interface ThemeTokenModes { light: string; dark: string }
export type ThemeTokenOverrides = Record<string, ThemeTokenModes>;
export interface ThemeDefinition { id: string; colorScheme: 'light' | 'dark'; tokens: ThemeTokens }
export interface ThemeSnapshot {
  preference: ThemePreference;
  active: ThemeDefinition;
  themes: readonly ThemeDefinition[];
  revision: number;
}
export interface ThemeTokenInspection {
  name: string;
  description: string;
  valueType: string;
  requiresLightAndDark: boolean;
  cssVariable?: string;
}
declare module '@seekdeep-ai/cordis' {
  interface Context { theme: InstanceType<typeof ThemeRuntime> }
  interface Events { 'theme/change'(snapshot: ThemeSnapshot): void }
}
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { 'settings.theme': ThemeKey }
}
"
    .to_owned()
}

fn client_web_react_declarations() -> String {
    r"
import type { SnapshotSelectorHook } from '@seekdeep-ai/seekdeep-client-ui-slots';
export const bindSnapshotSelector: typeof wasm_bindgen.bindSnapshotSelector;
export const createSlotRenderer: typeof wasm_bindgen.createSlotRenderer;
export const SessionProvider: (props: SessionProviderProps) => unknown;
export const useInvoke: typeof wasm_bindgen.useInvoke;
export class SlotAssemblyError extends Error {}
export class StaleAuthorizationError extends Error {}
export class SlotOwnershipError extends Error {}
export type UseSession<Snap extends object = object> = SnapshotSelectorHook<Snap>;
export type {
  ChainRenderOpts, HostObservable, RenderOpts, SessionProvideInfo, SnapshotSelectorHook,
  SlotRenderer, SlotRendererHost, StoreInstanceLike,
} from '@seekdeep-ai/seekdeep-client-ui-slots';
export interface SessionProviderProps {
  empty?: (() => unknown) | undefined;
  children(sessionId: string): unknown;
}
"
    .to_owned()
}

fn runtime_settings_contract_declarations() -> &'static str {
    r"export type SessionId = string & { readonly __brand: 'SessionId' };
export type WorkspaceId = string & { readonly __brand: 'WorkspaceId' };
export type SessionListPhase = 'pending' | 'ready';
export interface JobView {
  id: string;
  kind: string;
  label: string;
  status: 'running' | 'stopping' | 'completed' | 'killed' | 'failed';
  detail?: string;
  startedAt: number;
  finishedAt?: number;
}
export interface SessionSummary {
  id: SessionId;
  title?: string;
  displayTitle: string;
  cwd?: string;
  agentPreset?: string;
  parentId?: SessionId;
  origin?: 'subagent';
  running: boolean;
  pendingInteraction?: unknown;
  completed?: boolean;
  blank: boolean;
  updatedAt: number;
  projectionValues?: Readonly<Record<string, unknown>>;
}
export interface SessionListState {
  ids: SessionId[];
  byId: Record<SessionId, SessionSummary>;
  current: SessionId | undefined;
  phase: SessionListPhase;
  subagentsByParent: Readonly<Record<SessionId, unknown>>;
  jobsBySession: Readonly<Record<SessionId, readonly JobView[]>>;
  currentAddress: unknown | undefined;
}
export interface SettingsScopeSnapshot<T> {
  status: 'loading' | 'ready' | 'unavailable';
  value: T | undefined;
  base: unknown;
  user: unknown;
  revision: number | undefined;
  writable: boolean;
  mode: 'host' | 'memory';
}
export interface SettingsScopeSpec<T> {
  namespace: string;
  decode?: (section: unknown) => T | undefined;
}
export interface SettingsScope<T> {
  getSnapshot(): SettingsScopeSnapshot<T>;
  subscribe(listener: () => void): () => void;
  set(field: string, value: unknown): Promise<void>;
  unset(field: string): Promise<void>;
}
"
}

fn is_javascript_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|character| {
        character == '_' || character == '$' || character.is_ascii_alphabetic()
    }) && chars
        .all(|character| character == '_' || character == '$' || character.is_ascii_alphanumeric())
}

fn docs() -> anyhow::Result<()> {
    let status = ProcessCommand::new("cargo")
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ])
        .status()?;
    anyhow::ensure!(
        status.success(),
        "workspace exported-API documentation gate failed"
    );
    println!("verified exported Rust API documentation and strict prose-adjacent lints");
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
    source_commit: String,
    source_repository: String,
    source_product: String,
    target_product: String,
    target_binary: String,
    surfaces: Vec<Surface>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Surface {
    source: String,
    kind: SurfaceKind,
    status: Status,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Status {
    Pending,
    Ported,
    Verified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SurfaceKind {
    ProductionCode,
    Test,
    Snapshot,
    Fixture,
    Configuration,
    Documentation,
    Asset,
    RepositoryMetadata,
}

fn inventory(source: &Path) -> anyhow::Result<()> {
    verify_source(source)?;
    let mut manifest = read_manifest()?;
    let previous = std::mem::take(&mut manifest.surfaces)
        .into_iter()
        .map(|surface| (surface.source.clone(), surface))
        .collect::<BTreeMap<_, _>>();
    manifest.surfaces = source_files(source)?
        .into_iter()
        .map(|path| {
            previous.get(&path).cloned().unwrap_or_else(|| Surface {
                kind: classify(&path),
                source: path,
                status: Status::Pending,
                targets: Vec::new(),
                evidence: Vec::new(),
                note: None,
            })
        })
        .collect();
    let output = serde_json::to_vec_pretty(&manifest)?;
    std::fs::write("porting/parity.json", output)?;
    println!(
        "inventoried {} tracked source surfaces",
        manifest.surfaces.len()
    );
    Ok(())
}

fn parity(source: &Path, scope: Scope) -> anyhow::Result<()> {
    verify_source(source)?;
    let manifest = read_manifest()?;
    let expected = source_files(source)?;
    let actual = manifest
        .surfaces
        .iter()
        .map(|surface| surface.source.as_str())
        .collect::<Vec<_>>();
    let expected_refs = expected.iter().map(String::as_str).collect::<Vec<_>>();
    anyhow::ensure!(
        actual == expected_refs,
        "parity inventory is stale; run `cargo xtask inventory`"
    );

    let mut pending = Vec::new();
    let mut ported = Vec::new();
    let mut deferred = 0usize;
    for surface in &manifest.surfaces {
        if scope == Scope::Runtime
            && is_localization(&surface.source)
            && surface.status != Status::Verified
        {
            deferred += 1;
            continue;
        }
        match surface.status {
            Status::Pending => pending.push(surface.source.as_str()),
            Status::Ported => ported.push(surface.source.as_str()),
            Status::Verified => {
                anyhow::ensure!(
                    !surface.targets.is_empty(),
                    "verified surface has no Rust target: {}",
                    surface.source
                );
                anyhow::ensure!(
                    !surface.evidence.is_empty(),
                    "verified surface has no evidence: {}",
                    surface.source
                );
                for target in &surface.targets {
                    anyhow::ensure!(
                        Path::new(target).exists(),
                        "parity target does not exist: {target}"
                    );
                }
            }
        }
    }

    verify_rust_only()?;
    let scope_label = match scope {
        Scope::All => "all",
        Scope::Runtime => "runtime",
    };
    if !pending.is_empty() || !ported.is_empty() {
        let sample = pending
            .iter()
            .chain(&ported)
            .take(12)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "{scope_label} parity incomplete: {} pending, {} ported but unverified (deferred: {deferred}) (sample: {sample})",
            pending.len(),
            ported.len()
        );
    }
    println!(
        "verified {} {scope_label} source surfaces at 100% parity (deferred: {deferred})",
        manifest.surfaces.len() - deferred
    );
    Ok(())
}

/// Whether a surface is a localization artifact (Chinese translation or its
/// translation metadata), deferred from the runtime parity gate.
fn is_localization(source: &str) -> bool {
    if source.contains(".zh.") {
        return true;
    }
    let base = source.rsplit('/').next().unwrap_or(source);
    base.contains(".i18n.")
}

fn read_manifest() -> anyhow::Result<Manifest> {
    Ok(serde_json::from_slice(&std::fs::read(
        "porting/parity.json",
    )?)?)
}

fn verify_source(source: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.is_dir(),
        "source checkout does not exist: {}",
        source.display()
    );
    let manifest = read_manifest()?;
    let head = git_output(source, &["rev-parse", "HEAD"])?;
    anyhow::ensure!(
        head.trim() == manifest.source_commit,
        "source HEAD drifted: expected {}, got {}",
        manifest.source_commit,
        head.trim()
    );
    Ok(())
}

fn source_files(source: &Path) -> anyhow::Result<Vec<String>> {
    let output = git_output(source, &["ls-files"])?;
    Ok(output.lines().map(str::to_owned).collect())
}

fn git_output(source: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = ProcessCommand::new("git")
        .args(args)
        .current_dir(source)
        .output()?;
    anyhow::ensure!(output.status.success(), "git {} failed", args.join(" "));
    Ok(String::from_utf8(output.stdout)?)
}

fn classify(path: &str) -> SurfaceKind {
    let lower = path.to_ascii_lowercase();
    if lower.contains("snapshot")
        || Path::new(&lower)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("snap"))
    {
        return SurfaceKind::Snapshot;
    }
    if lower.contains("/fixtures/") || lower.contains("/fixture/") {
        return SurfaceKind::Fixture;
    }
    if lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains(".spec.")
        || lower.contains(".test.")
        || lower.contains(".e2e.")
    {
        return SurfaceKind::Test;
    }
    let extension = Path::new(path).extension().and_then(|value| value.to_str());
    match extension {
        Some("ts" | "tsx" | "js" | "mjs" | "cjs" | "py" | "c" | "cpp" | "sh") => {
            SurfaceKind::ProductionCode
        }
        Some("md" | "txt") => SurfaceKind::Documentation,
        Some("json" | "jsonl" | "yaml" | "yml" | "toml" | "ini" | "lock") => {
            SurfaceKind::Configuration
        }
        Some("png" | "svg" | "woff" | "woff2" | "ttf" | "html" | "css" | "webmanifest") => {
            SurfaceKind::Asset
        }
        _ => SurfaceKind::RepositoryMetadata,
    }
}

fn verify_rust_only() -> anyhow::Result<()> {
    let forbidden = ["ts", "tsx", "js", "mjs", "cjs", "py", "c", "cpp"];
    let mut violations = Vec::new();
    for entry in walkdir::WalkDir::new(".") {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .components()
            .any(|part| matches!(part.as_os_str().to_str(), Some(".git" | "target")))
            || is_generated_package_output(path)
        {
            continue;
        }
        if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| forbidden.contains(&extension))
        {
            violations.push(path.display().to_string());
        }
    }
    anyhow::ensure!(
        violations.is_empty(),
        "non-Rust implementation files present: {}",
        violations.join(", ")
    );
    Ok(())
}

fn is_generated_package_output(path: &Path) -> bool {
    let parts = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .filter(|component| *component != ".")
        .collect::<Vec<_>>();
    parts.len() >= 4 && parts[0] == "packages" && parts[3] == "lib"
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        classic_module_bundle, client_web_esm_declarations, client_web_esm_wrapper,
        compatibility_declarations, copy_ui_primitives_katex_assets,
        copy_ui_primitives_type_declarations, copy_wasm_package_assets, default_macos_platform_tag,
        is_generated_package_output, is_localization, ui_primitives_esm_wrapper,
        ui_primitives_highlight_backend, ui_primitives_internal_wrapper,
        ui_primitives_invariant_wrapper, ui_primitives_markdown_backend, watch_snapshot,
        write_wasm_package_compatibility_entries,
    };

    #[test]
    fn localization_predicate_defers_only_chinese_and_translation_metadata() {
        assert!(is_localization(".agents/notes/README.zh.md"));
        assert!(is_localization("packages/core/README.zh.md"));
        assert!(is_localization(".agents/notes/README.i18n.yaml"));
        assert!(is_localization("packages/core/README.i18n.yaml"));
        assert!(!is_localization(".agents/notes/README.md"));
        assert!(!is_localization("packages/core/src/session.rs"));
        assert!(!is_localization("packages/core/package.json"));
        assert!(!is_localization("packages/core/tests/session.spec.ts"));
    }

    #[test]
    fn classic_bundle_embeds_bytes_initializes_sync_and_registers_exact_module() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1, 2, 3],
            "__seekdeep_probe_wasm",
            "@seekdeep-ai/probe",
        )
        .unwrap();
        assert!(bundle.contains("AQID"));
        assert!(bundle.starts_with("var __seekdeep_probe_wasm = {};"));
        assert!(!bundle.contains("let wasm_bindgen ="));
        assert!(bundle.contains("__seekdeep_probe_wasm.initSync({ module: bytes })"));
        assert!(bundle.contains("id: \"@seekdeep-ai/probe\""));
        assert!(bundle.contains("factory: () => __seekdeep_probe_wasm"));
    }

    #[test]
    fn classic_bundle_rejects_an_unsafe_global_identifier() {
        let error = classic_module_bundle("", &[], "not-valid", "probe").unwrap_err();
        assert!(error.to_string().contains("JavaScript identifier"));
    }

    #[test]
    fn locale_bundle_configures_shell_modules_inside_its_materialization_factory() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_locale_wasm",
            "@seekdeep-ai/seekdeep-client-locale",
        )
        .unwrap();
        assert!(bundle.contains("apply: __seekdeep_client_locale_wasm.applyClientLocale"));
        assert!(bundle.contains("factory: require =>"));
        assert!(bundle.contains("require('react')"));
        assert!(bundle.contains("require('@seekdeep-ai/seekdeep-client-ui-primitives')"));
        assert!(bundle.contains("require('@seekdeep-ai/seekdeep-client-runtime/client')"));
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-locale");
        assert!(declarations.contains("applyClientLocale"));
        assert!(declarations.contains("settingsScope"));
    }

    #[test]
    fn ui_settings_bundle_materializes_a_traced_cordis_binder_and_exact_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_settings_wasm",
            "@seekdeep-ai/seekdeep-client-ui-settings",
        )
        .unwrap();
        for expected in [
            "require('@seekdeep-ai/cordis')",
            "class SettingsScopeBinder extends Service",
            "super(ctx, 'settingsScope')",
            ".bindSettingsScope(this.ctx, spec)",
            ".configureClientUiSettings(SettingsScopeBinder)",
            "apply: __seekdeep_client_ui_settings_wasm.applyClientUiSettings",
            "inject: []",
            "SettingsScopeController: __seekdeep_client_ui_settings_wasm.__SettingsScopeController",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-settings");
        for expected in [
            "new <T>",
            "class SettingsScopeBinder",
            "readonly []",
            "'settings.plugins.tab'",
            "'settings.general.item'",
            "openSection: (id: string) => void",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn ui_settings_general_bundle_configures_shell_modules_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_settings_general_wasm",
            "@seekdeep-ai/seekdeep-client-ui-settings-general",
        )
        .unwrap();
        for expected in [
            "configureClientUiSettingsGeneral(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "require('@seekdeep-ai/seekdeep-client-web-react')",
            "apply: __seekdeep_client_ui_settings_general_wasm.applyClientUiSettingsGeneral",
            "inject: ['slots', 'locale', 'connection']",
            "SettingsDocumentStore: __seekdeep_client_ui_settings_general_wasm.__SettingsDocumentStore",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-settings-general");
        for expected in [
            "type SettingsKey",
            "interface SettingsDocumentState",
            "const SettingsDocumentStore",
            "readonly ['slots', 'locale', 'connection']",
            "interface SettingsSectionRow",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn ui_settings_plugin_inventory_bundle_configures_tab_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_settings_plugin_inventory_wasm",
            "@seekdeep-ai/seekdeep-client-ui-settings-plugin-inventory",
        )
        .unwrap();
        for expected in [
            "configureClientUiSettingsPluginInventory(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_settings_plugin_inventory_wasm.applyClientUiSettingsPluginInventory",
            "inject: ['slots', 'locale', 'remote', 'remote.pluginInventory']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-settings-plugin-inventory");
        for expected in [
            "type PluginInventoryLocaleKey",
            "type PluginFiberPhase",
            "interface PluginInventoryEntry",
            "interface PluginInventorySnapshot",
            "interface PluginInventorySettingsTabInjected",
            "interface PluginInventorySettingsTabProps",
            "interface LocaleNamespaceMap",
            "readonly ['slots', 'locale', 'remote', 'remote.pluginInventory']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-settings-plugin-inventory",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-settings-plugin-inventory-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-settings-plugin-inventory"));
    }

    #[test]
    fn ui_skill_bundle_configures_catalog_row_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_skill_wasm",
            "@seekdeep-ai/seekdeep-client-ui-skill",
        )
        .unwrap();
        for expected in [
            "configureClientUiSkill(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_skill_wasm.applyClientUiSkill",
            "inject: ['inputTriggers', 'connection', 'sessions', 'slots', 'locale', 'remote']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-skill");
        for expected in [
            "type SkillKey",
            "interface LocaleNamespaceMap",
            "readonly ['inputTriggers', 'connection', 'sessions', 'slots', 'locale', 'remote']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-skill",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-skill-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-skill"));
    }

    #[test]
    fn ui_subagent_bundle_configures_catalog_composer_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_subagent_wasm",
            "@seekdeep-ai/seekdeep-client-ui-subagent",
        )
        .unwrap();
        for expected in [
            "configureClientUiSubagent(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_subagent_wasm.applyClientUiSubagent",
            "inject: ['inputTriggers', 'sessions', 'slots', 'locale']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-subagent");
        for expected in [
            "type SubagentKey",
            "interface SubagentCatalogInjected",
            "type SubagentCatalogActionProps",
            "interface SubagentReadOnlyMatch",
            "type SubagentReadOnlyComposerProps",
            "interface LocaleNamespaceMap",
            "readonly ['inputTriggers', 'sessions', 'slots', 'locale']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-subagent",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-subagent-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-subagent"));
    }

    #[test]
    fn ui_permission_presets_bundle_configures_controller_row_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_permission_presets_wasm",
            "@seekdeep-ai/seekdeep-client-ui-permission-presets",
        )
        .unwrap();
        for expected in [
            "configureClientUiPermissionPresets(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_permission_presets_wasm.applyClientUiPermissionPresets",
            "inject: ['commandUi', 'sessions', 'slots', 'locale', 'connection', 'remote']",
            "PermissionPresetSettingsController: __seekdeep_client_ui_permission_presets_wasm.__PermissionPresetSettingsController",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-permission-presets");
        for expected in [
            "type PermissionSettingsKey",
            "interface PermissionDefaultOption",
            "interface PermissionSettingsState",
            "interface PermissionSnapshotStore",
            "interface PermissionRowInjected",
            "type PermissionRowProps",
            "PermissionPresetSettingsController",
            "interface LocaleNamespaceMap",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-permission-presets",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-permission-presets-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-permission-presets"));
    }

    #[test]
    fn ui_model_selection_bundle_configures_directory_dual_entry_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_model_selection_wasm",
            "@seekdeep-ai/seekdeep-client-ui-model-selection",
        )
        .unwrap();
        for expected in [
            "configureClientUiModelSelection(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_model_selection_wasm.applyClientUiModelSelection",
            "inject: ['commandUi', 'connection', 'locale', 'sessions', 'slots', 'remote']",
            "ModelDirectory: __seekdeep_client_ui_model_selection_wasm.__ModelDirectory",
            "class ModelDirectoryResolver",
            "createModelDirectoryResolver(ctx, config.blockReason)",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-model-selection");
        for expected in [
            "type ModelKey",
            "interface ModelDirectoryState",
            "interface ModelDirectoryStore",
            "ModelDirectoryResolver",
            "interface ModelSelectInjected",
            "interface LocaleNamespaceMap",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-model-selection",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-model-selection-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-model-selection"));
    }

    #[test]
    fn ui_input_trigger_bundle_configures_service_menu_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_input_trigger_wasm",
            "@seekdeep-ai/seekdeep-client-ui-input-trigger",
        )
        .unwrap();
        for expected in [
            "configureClientUiInputTrigger(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_input_trigger_wasm.applyClientUiInputTrigger",
            "inject: ['sessions', 'locale']",
            "class InputTriggerService",
            "InputTriggerController: __seekdeep_client_ui_input_trigger_wasm.__InputTriggerController",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-input-trigger");
        for expected in [
            "type TriggerChar",
            "interface InputTriggerSource",
            "interface MenuState",
            "InputTriggerController",
            "class InputTriggerService",
            "interface MenuViewInjected",
            "interface SlotMap",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-input-trigger",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-input-trigger-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-input-trigger"));
    }

    #[test]
    fn ui_commands_bundle_configures_service_popup_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_commands_wasm",
            "@seekdeep-ai/seekdeep-client-ui-commands",
        )
        .unwrap();
        for expected in [
            "configureClientUiCommands(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_commands_wasm.applyClientUiCommands",
            "inject: ['inputTriggers', 'sessions', 'remote', 'remote.commands', 'locale']",
            "CommandUiRuntime: __seekdeep_client_ui_commands_wasm.__CommandUiRuntime",
            "CommandDirectory: __seekdeep_client_ui_commands_wasm.__CommandDirectory",
            "PopupSelectController: __seekdeep_client_ui_commands_wasm.__PopupSelectController",
            "PopupSelectView: __seekdeep_client_ui_commands_wasm.popupSelectViewComponent()",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-commands");
        for expected in [
            "type DirectoryStatus",
            "interface CommandDescriptor",
            "class CommandDirectory",
            "interface PopupState",
            "class PopupSelectController",
            "interface CommandContribution",
            "interface CommandDecoration",
            "class CommandUiRuntime",
            "const filterOptions",
            "interface PopupSelectInjected",
            "interface LocaleNamespaceMap",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-commands",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-commands-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-commands"));
    }

    #[test]
    fn ui_message_feedback_bundle_configures_controls_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_message_feedback_wasm",
            "@seekdeep-ai/seekdeep-client-ui-message-feedback",
        )
        .unwrap();
        for expected in [
            "configureClientUiMessageFeedback(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_message_feedback_wasm.applyClientUiMessageFeedback",
            "inject: ['slots', 'remote', 'remote.messageFeedback', 'locale']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-message-feedback");
        for expected in [
            "type MessageFeedbackStatus",
            "type MessageFeedbackRating",
            "interface MessageFeedbackView",
            "interface MessageFeedbackInjected",
            "type MessageFeedbackActionResult",
            "readonly ['slots', 'remote', 'remote.messageFeedback', 'locale']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-message-feedback",
            output.path(),
        )
        .unwrap();
        for (path, expected) in [
            ("index.js", "export function apply() {}"),
            ("invariant.js", "client-ui-feedback-invariant"),
            (
                "invariant.js",
                "@seekdeep-ai/seekdeep-client-ui-message-feedback",
            ),
            ("types/index.d.ts", "function apply(): void"),
            ("types/invariant.d.ts", "interface InvariantContext"),
            ("types/invariant.d.ts", "Promise<() => void>"),
        ] {
            let artifact = std::fs::read_to_string(output.path().join(path)).unwrap();
            assert!(artifact.contains(expected), "{path} omitted {expected:?}");
        }
    }

    #[test]
    fn ui_plan_bundle_configures_command_chip_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_plan_wasm",
            "@seekdeep-ai/seekdeep-client-ui-plan",
        )
        .unwrap();
        for expected in [
            "configureClientUiPlan(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_plan_wasm.applyClientUiPlan",
            "inject: ['slots', 'remote', 'remote.commands', 'locale']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-plan");
        for expected in [
            "type PlanKey",
            "interface PlanChipInjected",
            "Promise<string | null>",
            "interface LocaleNamespaceMap",
            "readonly ['slots', 'remote', 'remote.commands', 'locale']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-plan",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-plan-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-plan"));
    }

    #[test]
    fn ui_jobs_bundle_configures_header_popover_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_jobs_wasm",
            "@seekdeep-ai/seekdeep-client-ui-jobs",
        )
        .unwrap();
        for expected in [
            "configureClientUiJobs(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_jobs_wasm.applyClientUiJobs",
            "inject: ['sessions', 'slots', 'locale']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-jobs");
        for expected in [
            "type JobKey",
            "interface JobListActionProps",
            "SessionId, SessionListState",
            "useSessions<T>",
            "interface LocaleNamespaceMap",
            "readonly ['sessions', 'slots', 'locale']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-jobs",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-jobs-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-jobs"));
    }

    #[test]
    fn ui_goal_bundle_configures_dock_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_goal_wasm",
            "@seekdeep-ai/seekdeep-client-ui-goal",
        )
        .unwrap();
        for expected in [
            "configureClientUiGoal(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_goal_wasm.applyClientUiGoal",
            "inject: ['slots', 'sessions', 'remote', 'remote.goals', 'locale', 'conversationEvents']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-goal");
        for expected in [
            "interface GoalActionError",
            "type GoalActionResult",
            "interface GoalBarActions",
            "readonly ['slots', 'sessions', 'remote', 'remote.goals', 'locale', 'conversationEvents']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-goal",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-goal-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-goal"));
    }

    #[test]
    fn ui_deliverables_bundle_configures_file_surfaces_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_deliverables_wasm",
            "@seekdeep-ai/seekdeep-client-ui-deliverables",
        )
        .unwrap();
        for expected in [
            "configureClientUiDeliverables(require('react'))",
            "apply: __seekdeep_client_ui_deliverables_wasm.applyClientUiDeliverables",
            "inject: ['slots', 'locale', 'conversationEvents', 'connection']",
            "ProducedFiles: __seekdeep_client_ui_deliverables_wasm.producedFilesComponent()",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-deliverables");
        for expected in [
            "interface DeliverablesTurnData",
            "interface ProducedFilesInjected",
            "interface ProducedFilesProps",
            "useHostDescription<T>",
            "import('react').JSX.Element",
            "const ProducedFiles",
            "const producedForClosing",
            "interface ConversationTurnDataMap",
            "interface LocaleNamespaceMap",
            "readonly ['slots', 'locale', 'conversationEvents', 'connection']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-deliverables",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-deliverables-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-deliverables"));
    }

    #[test]
    fn ui_trajectory_bundle_configures_runtime_graph_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_trajectory_wasm",
            "@seekdeep-ai/seekdeep-client-ui-trajectory",
        )
        .unwrap();
        for expected in [
            "configureClientUiTrajectoryModules(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "configureClientUiTrajectoryRuntime(require('@seekdeep-ai/seekdeep-client-runtime/client'))",
            "apply: __seekdeep_client_ui_trajectory_wasm.applyClientUiTrajectory",
            "inject: ['slots', 'conversationEvents', 'conversationViews', 'sessions', 'locale']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-trajectory");
        for expected in [
            "interface TrajectoryViewInjected",
            "type TrajectoryTimelineMode",
            "interface TrajectoryTimeRange",
            "readonly ['slots', 'conversationEvents', 'conversationViews', 'sessions', 'locale']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-trajectory",
            output.path(),
        )
        .unwrap();
        for (path, expected) in [
            ("index.js", "export function apply() {}"),
            ("invariant.js", "client-ui-trajectory-invariant"),
            ("invariant.js", "@seekdeep-ai/seekdeep-client-ui-trajectory"),
            ("types/index.d.ts", "function apply(): void"),
            ("types/invariant.d.ts", "client-ui-trajectory-invariant"),
        ] {
            let artifact = std::fs::read_to_string(output.path().join(path)).unwrap();
            assert!(artifact.contains(expected), "{path} omitted {expected:?}");
        }
    }

    #[test]
    fn ui_user_questions_bundle_configures_composer_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_user_questions_wasm",
            "@seekdeep-ai/seekdeep-client-ui-user-questions",
        )
        .unwrap();
        for expected in [
            "configureClientUiUserQuestions(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_user_questions_wasm.applyClientUiUserQuestions",
            "inject: ['slots', 'locale']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-user-questions");
        for expected in [
            "interface QuestionComposerProps",
            "interface QuestionAnswer",
            "interface PlanReview",
            "readonly ['slots', 'locale']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-user-questions",
            output.path(),
        )
        .unwrap();
        for (path, expected) in [
            ("index.js", "export function apply() {}"),
            ("invariant.js", "client-ui-user-questions-invariant"),
            (
                "invariant.js",
                "@seekdeep-ai/seekdeep-client-ui-user-questions",
            ),
            ("types/index.d.ts", "function apply(): void"),
            ("types/invariant.d.ts", "client-ui-user-questions-invariant"),
        ] {
            let artifact = std::fs::read_to_string(output.path().join(path)).unwrap();
            assert!(artifact.contains(expected), "{path} omitted {expected:?}");
        }
    }

    #[test]
    fn ui_workflow_run_bundle_configures_keyed_renderer_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_workflow_run_wasm",
            "@seekdeep-ai/seekdeep-client-ui-workflow-run",
        )
        .unwrap();
        for expected in [
            "configureClientUiWorkflowRun(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "require('@seekdeep-ai/seekdeep-client-runtime/client')",
            "apply: __seekdeep_client_ui_workflow_run_wasm.applyClientUiWorkflowRun",
            "inject: ['conversationEvents', 'slots', 'sessions', 'locale']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-workflow-run");
        for expected in [
            "type WorkflowRunStatus",
            "import type { SessionId }",
            "interface WorkflowRunMemberData",
            "interface WorkflowRunPhaseData",
            "interface WorkflowRunChatData",
            "interface ChatNodeDataMap",
            "interface LocaleNamespaceMap",
            "readonly ['conversationEvents', 'slots', 'sessions', 'locale']",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-workflow-run",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-workflow-run-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-workflow-run"));
    }

    #[test]
    fn ui_sidebar_bundle_configures_shell_modules_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_sidebar_wasm",
            "@seekdeep-ai/seekdeep-client-ui-sidebar",
        )
        .unwrap();
        for expected in [
            "configureClientUiSidebar(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_sidebar_wasm.applyClientUiSidebar",
            "inject: ['slots', 'layout', 'sessions', 'workspaces', 'locale']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-sidebar");
        for expected in [
            "type SidebarKey",
            "interface SidebarRootInjected",
            "interface SidebarRootComponentProps",
            "'sidebar.workspaces'",
            "'sidebar.settings'",
            "'sidebar.footer.action'",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn ui_layout_bundle_configures_runtime_and_public_shell_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_layout_wasm",
            "@seekdeep-ai/seekdeep-client-ui-layout",
        )
        .unwrap();
        for expected in [
            "configureClientUiLayout(require('react')",
            "require('@seekdeep-ai/seekdeep-client-runtime/client')",
            "apply: __seekdeep_client_ui_layout_wasm.applyClientUiLayout",
            "inject: ['slots', 'theme']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-layout");
        for expected in [
            "LayoutController: typeof wasm_bindgen.LayoutController",
            "interface ILayout",
            "interface SidebarOwnerProps",
            "interface Context { layout: ILayout }",
            "scope: 'session-maybe'",
            "'shell.overlay'",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn ui_theme_bundle_configures_runtime_public_contract_and_style_assets() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_theme_wasm",
            "@seekdeep-ai/seekdeep-client-ui-theme",
        )
        .unwrap();
        for expected in [
            "configureClientUiTheme(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "require('@seekdeep-ai/seekdeep-client-runtime/client')",
            "apply: __seekdeep_client_ui_theme_wasm.applyClientUiTheme",
            "inject: ['slots', 'locale', 'connection', 'remote', 'settingsScope']",
            "SETTINGS_NS: 'settings.theme'",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-theme");
        for expected in [
            "ThemeRuntime: typeof wasm_bindgen.ThemeRuntime",
            "type ThemePreference = 'light' | 'dark' | 'system'",
            "interface AppearanceRowInjected",
            "interface ThemeSnapshot",
            "interface ThemeTokenInspection",
            "'theme/change'(snapshot: ThemeSnapshot)",
            "'settings.theme': ThemeKey",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }

        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("packages/client/ui-theme/src/styles");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("base.css"), "body {}\n").unwrap();
        std::fs::write(source.join("scrollbar.css"), "body {}\n").unwrap();
        let output = workspace.path().join("lib");
        std::fs::create_dir_all(output.join("styles")).unwrap();
        std::fs::write(output.join("styles/stale.css"), "stale\n").unwrap();
        copy_wasm_package_assets(
            workspace.path(),
            "@seekdeep-ai/seekdeep-client-ui-theme",
            &output,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(output.join("styles/base.css")).unwrap(),
            "body {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(output.join("styles/scrollbar.css")).unwrap(),
            "body {}\n"
        );
        assert!(!output.join("styles/stale.css").exists());
    }

    #[test]
    fn client_web_react_bundle_exposes_compiled_hooks_renderer_and_error_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_web_react_wasm",
            "@seekdeep-ai/seekdeep-client-web-react",
        )
        .unwrap();
        for expected in [
            "const React = require('react')",
            "configureClientWebReact(React",
            "createSelectorShim(React)",
            "webReactErrorClasses()",
            "createSlotRenderer: __seekdeep_client_web_react_wasm.createSlotRenderer",
            "SessionProvider: __seekdeep_client_web_react_wasm.sessionProviderComponent()",
            "bindSnapshotSelector: __seekdeep_client_web_react_wasm.bindSnapshotSelector",
            "useInvoke: __seekdeep_client_web_react_wasm.useInvoke",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-web-react");
        for expected in [
            "import type { SnapshotSelectorHook }",
            "const createSlotRenderer",
            "const SessionProvider",
            "class SlotAssemblyError",
            "class StaleAuthorizationError",
            "type UseSession",
            "interface SessionProviderProps",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn client_web_esm_shell_bootstraps_before_the_module_table_and_exports_public_contract() {
        let wrapper = client_web_esm_wrapper();
        for expected in [
            "import init, * as wasm from './client.js'",
            "import * as React from 'react'",
            "import Loader from '@seekdeep-ai/cordis-plugin-loader'",
            "import './base.css'",
            "await init(new URL('./client_bg.wasm', import.meta.url))",
            "'@seekdeep-ai/seekdeep-client-web-react': WebReact",
            "wasm.configureClientWeb(",
            "export const AppWebEntry = wasm.AppWebEntry",
            "export const PLATFORM_MODULES = Object.freeze",
        ] {
            assert!(wrapper.contains(expected), "missing {expected:?}");
        }
        let declarations = client_web_esm_declarations();
        for expected in [
            "export { AppWebEntry } from '../client.js'",
            "interface AppRootProps",
            "interface DocumentTitleProps",
            "interface AppShellService",
            "type LoaderEntryState",
            "const FIBER_STATE",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn ui_primitives_esm_library_keeps_shiki_linkage_thin_and_rust_policy_owned() {
        let backend = ui_primitives_highlight_backend();
        for expected in [
            "createHighlighterCoreSync",
            "createJavaScriptRegexEngine",
            "lazyCompileLength: Number.POSITIVE_INFINITY",
            "function createHighlighter()",
            "tokenizeTimeLimit: 0",
            "warm() { highlighter(); }",
            "['python', () => import('@shikijs/langs/python')]",
            "['lua', () => import('@shikijs/langs/lua')]",
            "loadLanguageSync(mod.default)",
            "codeToHtml(code, { lang, theme: 'css-variables' })",
            "codeToTokens(code, { lang, theme: 'css-variables' })",
        ] {
            assert!(backend.contains(expected), "missing {expected:?}");
        }
        assert_eq!(backend.matches("() => import('@shikijs/langs/").count(), 23);
        let markdown_backend = ui_primitives_markdown_backend();
        for expected in [
            "import katex from 'katex'",
            "import { normalizeUri } from 'micromark-util-sanitize-uri'",
            "createMarkdownBackend(cssUrl)",
            "cssUrl,",
            "katex.renderToString(value, options)",
        ] {
            assert!(markdown_backend.contains(expected), "missing {expected:?}");
        }
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace parent");
        let output = tempfile::tempdir().unwrap();
        copy_ui_primitives_katex_assets(workspace, output.path()).unwrap();
        let projected = output.path().join("katex");
        assert!(
            std::fs::read_to_string(projected.join("katex.min.css"))
                .unwrap()
                .contains("fonts/KaTeX_Main-Regular.woff2")
        );
        assert_eq!(
            std::fs::read_dir(projected.join("fonts")).unwrap().count(),
            60
        );
        assert!(projected.join("LICENSE").is_file());
        let projected_types = output.path().join("types");
        copy_ui_primitives_type_declarations(workspace, &projected_types).unwrap();
        assert_eq!(
            walkdir::WalkDir::new(&projected_types)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_file()
                        && entry.path().extension().and_then(std::ffi::OsStr::to_str) == Some("ts")
                })
                .count(),
            43
        );
        let index_declaration =
            std::fs::read_to_string(projected_types.join("index.d.ts")).unwrap();
        assert!(index_declaration.contains("export { MarkdownText }"));
        assert!(!index_declaration.contains("@deepseek-ai/dsh-"));
        assert!(ui_primitives_invariant_wrapper().contains("client-ui-primitives-invariant"));
        let internal = ui_primitives_internal_wrapper();
        assert!(internal.contains("export * from './index.js'"));
        assert!(internal.contains("export const highlightToHtml"));
        let package: serde_json::Value = serde_json::from_slice(
            &std::fs::read(workspace.join("packages/client/ui-primitives/package.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            package
                .get("exports")
                .and_then(|exports| exports.get("./src/*"))
                .and_then(|entry| entry.get("default"))
                .and_then(serde_json::Value::as_str),
            Some("./lib/internal.js")
        );
        assert!(
            package["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry.as_str() == Some("lib/client_bg.wasm"))
        );
        let wrapper = ui_primitives_esm_wrapper();
        for expected in [
            "await init({ module_or_path: new URL('./client_bg.wasm', import.meta.url) })",
            "configureClientUiPrimitiveHighlight(createHighlightBackend())",
            "configureClientUiPrimitiveMarkdownAtoms(React)",
            "configureClientUiPrimitiveCodeBlock(React)",
            "configureClientUiPrimitiveMarkdown(React, createMarkdownBackend(new URL('./katex/katex.min.css', import.meta.url).href))",
            "configureClientUiPrimitiveReadBlock(React)",
            "export const CodeBlock = wasm.codeBlockComponent()",
            "export const JsonBlock = wasm.jsonBlockComponent()",
            "export const MessageText = wasm.messageTextComponent()",
            "export const MarkdownText = wasm.markdownTextComponent()",
            "export const ReadBlock = wasm.readBlockComponent()",
            "export const DEFAULT_DIFF_MAX_LINES = wasm.defaultDiffMaxLines()",
            "export const DEFAULT_SEARCH_MAX_LINES = wasm.defaultSearchMaxLines()",
            "export const DEFAULT_TERMINAL_MAX_LINES = wasm.defaultTerminalMaxLines()",
            "export const extractMarkdownPlainText = wasm.extractMarkdownPlainText",
            "export const FishLogo = iconComponents.FishLogo",
            "export const BrandWordmark = iconComponents.BrandWordmark",
        ] {
            assert!(wrapper.contains(expected), "missing {expected:?}");
        }
        assert_eq!(
            wrapper.matches(" = iconComponents.").count(),
            seekdeep_client_ui_primitives::ICON_DEFINITIONS.len()
        );
        assert!(!wrapper.contains("export const highlightToHtml"));
        assert!(!wrapper.contains("export const usePointerGrace"));
        assert!(!wrapper.contains("Object.assign(globalThis"));
    }

    #[test]
    fn api_remotes_bundle_mounts_the_exact_generated_contribution_set() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_api_remotes_client_wasm",
            "@seekdeep-ai/seekdeep-api-remotes",
        )
        .unwrap();
        for expected in [
            "configureApiRemotes([require('@seekdeep-ai/seekdeep-commands/remote')",
            "require('@seekdeep-ai/seekdeep-goal/remote')",
            "require('@seekdeep-ai/seekdeep-cordis-host-runner/remote')",
            "require('@seekdeep-ai/seekdeep-host-plugin-inventory/remote')",
            "require('@seekdeep-ai/seekdeep-message-feedback/remote')",
            "apply: __seekdeep_api_remotes_client_wasm.applyApiRemotes",
            "inject: ['remote']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-api-remotes");
        for expected in [
            "TypertClientRemote as ClientRemote",
            "PluginInventorySnapshot",
            "ApiRemoteForwardedEvent",
            "@seekdeep-ai/seekdeep-commands/remote",
            "@seekdeep-ai/seekdeep-settings/types",
            "QuestionResponsePayload",
            "DynamicCordisRunResponse",
            "JsonValue",
            "interface Context { remote: TypertClientRemote }",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn wasm_watch_snapshot_changes_with_package_input_bytes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let source = root.path().join("src/lib.rs");
        std::fs::write(&source, "one\n").unwrap();
        let before = watch_snapshot(root.path()).unwrap();
        std::fs::write(&source, "one two\n").unwrap();
        let after = watch_snapshot(root.path()).unwrap();
        assert_ne!(before, after);

        std::fs::create_dir_all(root.path().join("lib")).unwrap();
        std::fs::create_dir_all(root.path().join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.path().join("assets")).unwrap();
        let generated = root.path().join("lib/index.js");
        let dependency = root.path().join("node_modules/pkg/index.js");
        let asset = root.path().join("assets/style.css");
        std::fs::write(&generated, "generated\n").unwrap();
        std::fs::write(&dependency, "dependency\n").unwrap();
        std::fs::write(&asset, "asset\n").unwrap();
        let inputs = watch_snapshot(root.path()).unwrap();
        assert!(!inputs.contains_key(&generated));
        assert!(!inputs.contains_key(&dependency));
        assert!(inputs.contains_key(&asset));
    }

    #[test]
    fn rust_only_gate_skips_only_package_lib_derivatives() {
        assert!(is_generated_package_output(Path::new(
            "packages/client/runtime/lib/client.js"
        )));
        assert!(!is_generated_package_output(Path::new(
            "packages/client/runtime/src/client.js"
        )));
        assert!(!is_generated_package_output(Path::new(
            "packages/client/lib/client.js"
        )));
    }

    #[test]
    fn client_runtime_declarations_expose_compatibility_aliases_and_error_classes() {
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-runtime");
        for expected in [
            "const SlotRegistry: typeof wasm_bindgen.ClientSlotRegistry",
            "const apply: typeof wasm_bindgen.applyClientRuntime",
            "class SessionCreateError extends Error",
            "class WorkspaceCreateError extends Error",
            "EMPTY_CHAT_SNAPSHOT",
            "interface SettingsScopeSnapshot<T>",
            "interface SettingsScopeSpec<T>",
            "interface SettingsScope<T>",
            "type SessionId = string &",
            "interface JobView",
            "interface SessionSummary",
            "interface SessionListState",
            "type WorkspaceId = string &",
        ] {
            assert!(declarations.contains(expected));
        }
        assert!(compatibility_declarations("other").is_empty());
    }

    #[test]
    fn macos_deployment_default_comes_from_the_runtime_platform_manifest() {
        assert_eq!(default_macos_platform_tag().unwrap(), "macosx_14_0_arm64");
    }
}
